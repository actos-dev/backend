//! Ortam değişkenlerinden yapılandırma.
//!
//! Kural: **eksik ya da bozuk yapılandırma açılışta fark edilir.** Uygulama
//! yarı yapılandırılmış şekilde ayağa kalkıp ilk isteği aldığında patlamaz;
//! `Config::from_env()` başarısız olursa süreç hiç başlamaz.

use std::{fmt, net::SocketAddr, str::FromStr, time::Duration};

use crate::{
    auth::ActorType,
    ratelimit::{RateLimitConfig, Scope, Subject},
};

/// Yapılandırma okunurken oluşan hatalar.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("zorunlu ortam değişkeni eksik: {0}")]
    Missing(&'static str),

    #[error("ortam değişkeni `{name}` çözümlenemedi: {detail}")]
    Invalid { name: &'static str, detail: String },
}

/// Uygulamanın tüm çalışma zamanı yapılandırması.
#[derive(Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub storage: StorageConfig,
    pub security: SecurityConfig,
    pub rate_limits: LimitTable,
    /// Actor başına toplam depolama kotası, güven kademesine göre (Faz
    /// 18.A, bkz. NOTES.md §9.8). `rate_limits`'ten **ayrı bir alan**:
    /// ikisi de güven kademesine bağlı ama biri hızı (istek/saniye) biri
    /// birikimi (toplam bayt) sınırlıyor — kavramsal olarak farklı
    /// eksenler, tek bir yapıda birleştirmek ["kademe çarpanı" tek bir
    /// katsayıya indirgenmiş `TRUST_LEVEL_CAPACITY_MULTIPLIER`'ın aksine]
    /// kotanın kendi mutlak bayt değerlerini taşıması gerektiğinden
    /// (`50 MB`/`500 MB`/`2 GB` çarpan değil, mutlak sayı) yapay bir
    /// zorlama olurdu.
    pub storage_quota: StorageQuotaConfig,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub addr: SocketAddr,
    /// Tek bir isteğin tamamlanması için üst sınır.
    pub request_timeout: Duration,
    /// Aynı anda işlenebilecek en fazla istek — arka uç doyduğunda
    /// kuyruğa yığmak yerine 503 dönmeyi tercih ediyoruz.
    pub max_concurrent_requests: usize,
    /// İstek gövdesi üst sınırı (dosya yükleme kendi daha yüksek limitini kullanır).
    pub max_body_bytes: usize,
    /// `POST /uploads` için gövde üst sınırı (bkz. PLAN.md Faz 13: 8 MB).
    /// [`Self::max_body_bytes`]'tan ayrı ve daha yüksek: genel uçlar JSON
    /// alıyor, yükleme ucu görsel.
    pub max_upload_bytes: usize,
    /// Kullanılmayan etiketleri toplayan periyodik işin çalışma aralığı
    /// (bkz. `crate::tag::cleanup_unused`). Sıfır verilirse iş hiç
    /// başlatılmaz — tek seferlik bir kurulumda ya da temizliği dışarıdan
    /// (cron) yürütmek isteyen bir dağıtımda kapatılabilsin diye.
    pub tag_cleanup_interval: Duration,
    /// `hot_score` tazeleme işinin çalışma aralığı (bkz.
    /// `crate::feed::recompute_hot_scores`). Sıfır = iş hiç başlatılmaz.
    pub hot_score_interval: Duration,
    /// Bağlanmamış yüklemeleri toplayan işin aralığı (bkz.
    /// `crate::attachment::cleanup_orphaned`). Sıfır = iş hiç başlatılmaz.
    pub orphan_cleanup_interval: Duration,
    /// Güven kademesi (trust level) tazeleme işinin çalışma aralığı (bkz.
    /// `crate::actor::recompute_trust_levels`). Sıfır = iş hiç başlatılmaz.
    pub trust_level_interval: Duration,
    /// Önümüzde kaç **güvenilir** ters proxy (reverse proxy) olduğu —
    /// `X-Forwarded-For` header'ının IP başına hız sınırlamada ne kadar
    /// güvenilebileceğini belirler.
    ///
    /// **`0` (varsayılan): `X-Forwarded-For` tamamen yok sayılır**, istemci
    /// IP'si olarak her zaman soket adresi (`ConnectInfo`) kullanılır. Bu
    /// güvenli varsayılandır: istemci bu header'ı kendi uydurabilir, önünde
    /// gerçekten bir proxy yoksa (ya da proxy bu header'ı kendi
    /// yazmıyor/temizlemiyorsa) `XFF`'e güvenmek IP başına hız sınırını
    /// tamamen atlatılabilir kılar.
    ///
    /// `N > 0` verildiğinde, gerçek istemci IP'sinin `XFF` zincirinde
    /// **sağdan `N+1`. sırada** olduğu varsayılır (soldan değil — sol taraf
    /// istemcinin uydurabildiği kısım). Seçim mantığı ve birim testleri
    /// `actos-api`'de (`crates/actos-api/src/middleware/client_ip.rs`) —
    /// bu crate HTTP'yi bilmiyor, yalnızca değeri taşıyor.
    pub trusted_proxy_hops: usize,
}

#[derive(Clone)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

#[derive(Clone)]
pub struct RedisConfig {
    pub url: String,
    pub pool_size: usize,
}

#[derive(Clone)]
pub struct StorageConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub public_base_url: String,
}

/// Actor başına toplam depolama kotası, güven kademesine göre (bayt) — Faz
/// 18.A, NOTES.md §9.8: *"biri 1000 hesap açıp her biriyle 100 tane 8 MB'lık
/// görsel yükleyip diski doldurabilir"* senaryosuna karşı savunma.
///
/// **Neden `MAX_UPLOAD_BYTES` yetmiyordu:** o, **tek dosya** başına bir üst
/// sınır (bkz. `ServerConfig::max_upload_bytes`) — rate limit (`Scope::
/// Upload`) yükleme **hızını** kısıyor ama sabırlı bir saldırgan zamanla
/// aynı toplam boyuta ulaşır. Bu kota **birikimi** (toplam bayt) sınırlıyor,
/// hıza bakmaksızın.
///
/// **Değerler `koda gömülü değil, env'den okunuyor`** (görev gereksinimi):
/// rate limit çarpanlarının aksine (`TRUST_LEVEL_CAPACITY_MULTIPLIER`,
/// birimsiz oran) bunlar mutlak bayt sayıları — operatörün depolama
/// kapasitesine göre ayarlaması gereken, platforma özgü işletme kararları,
/// kodda sabitlenmeye uygun değil (tıpkı `MAX_UPLOAD_BYTES` gibi).
#[derive(Clone, Copy, Debug)]
pub struct StorageQuotaConfig {
    /// Kademe 0 (taze/doğrulanmamış hesap) — en dar. Varsayılan 50 MB.
    pub trust_level_0_bytes: i64,
    /// Kademe 1 (asgari etkinlik göstermiş hesap). Varsayılan 500 MB.
    pub trust_level_1_bytes: i64,
    /// Kademe 2 (kurulmuş hesap) — en geniş. Varsayılan 2 GB.
    pub trust_level_2_bytes: i64,
}

impl StorageQuotaConfig {
    /// `trust_level` (0-2) için kota baytı.
    ///
    /// `crate::attachment::create_attachment`'a doğrudan `quota_bytes: i64`
    /// olarak geçiliyor — `attachment.rs` bu struct'ı ya da `trust_level`
    /// kavramını bilmiyor, yalnızca çağıranın (`actos-api::routes::uploads`)
    /// hesapladığı nihai bayt sınırını uyguluyor. Bu, `RateLimitConfig`
    /// (çözümlenmiş kapasite) ile `LimitTable` (kademe → kapasite kuralları)
    /// arasındaki aynı sorumluluk ayrımı: kural burada, uygulama orada.
    ///
    /// Aralık dışı bir `trust_level` (savunmacı — şema `CHECK` ile `0..=2`
    /// garanti ediyor ama bu fonksiyon şemaya güvenmeden de doğru
    /// davranmalı) en yakın uca kenetlenir.
    #[must_use]
    pub const fn for_trust_level(&self, trust_level: i16) -> i64 {
        match trust_level {
            i16::MIN..=0 => self.trust_level_0_bytes,
            1 => self.trust_level_1_bytes,
            _ => self.trust_level_2_bytes,
        }
    }

    /// Ortamdan oku ve doğrula (bkz. [`Self::validate`]).
    ///
    /// # Errors
    /// Bir değer çözümlenemiyorsa veya kademeler artan sırada değilse.
    pub fn from_env() -> Result<Self, ConfigError> {
        let table = Self {
            trust_level_0_bytes: optional("STORAGE_QUOTA_TRUST_0_BYTES")?
                .unwrap_or(50 * 1024 * 1024),
            trust_level_1_bytes: optional("STORAGE_QUOTA_TRUST_1_BYTES")?
                .unwrap_or(500 * 1024 * 1024),
            trust_level_2_bytes: optional("STORAGE_QUOTA_TRUST_2_BYTES")?
                .unwrap_or(2 * 1024 * 1024 * 1024),
        };
        table.validate()?;
        Ok(table)
    }

    /// Sıfır/negatif bir kota, `create_attachment`'ta *hiçbir* yükleme
    /// başarılı olamayacağı ("mevcut kullanım (>= 0) + yeni dosya (> 0)
    /// her zaman kotayı aşar") anlamsız bir yapılandırma — açılışta
    /// yakalanmalı. Kademeler **kesin artan** olmalı: bu şart bir CHECK
    /// kısıtı değil (migration eklenmedi, bkz. görev kısıtı) ama sessizce
    /// tersine dönmüş bir sıralama ("kademe 2 kademe 0'dan dar") operatör
    /// hatasının en olası biçimi, açılışta durdurmak ilk isteği beklemekten
    /// daha iyi.
    fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in [
            ("STORAGE_QUOTA_TRUST_0_BYTES", self.trust_level_0_bytes),
            ("STORAGE_QUOTA_TRUST_1_BYTES", self.trust_level_1_bytes),
            ("STORAGE_QUOTA_TRUST_2_BYTES", self.trust_level_2_bytes),
        ] {
            if value <= 0 {
                return Err(ConfigError::Invalid {
                    name,
                    detail: "sıfır veya negatif olamaz".to_owned(),
                });
            }
        }
        if !(self.trust_level_0_bytes <= self.trust_level_1_bytes
            && self.trust_level_1_bytes <= self.trust_level_2_bytes)
        {
            return Err(ConfigError::Invalid {
                name: "STORAGE_QUOTA_TRUST_*_BYTES",
                detail: "kademeler artan sırada olmalı (kademe 0 <= 1 <= 2)".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct SecurityConfig {
    /// Dış ID'lerin ardışık görünmemesi için kullanılan permütasyon anahtarı.
    pub id_obfuscation_key: String,
    /// Keyset sayfalama cursor'larını imzalamak için kullanılan HMAC anahtarı
    /// (bkz. `crate::cursor::CursorCodec`). `id_obfuscation_key`'den **ayrı**
    /// bir anahtar: ikisi aynı anahtarı paylaşsaydı, biri sızarsa (ya da
    /// rotasyona ihtiyaç duyulursa) diğer mekanizma da gereksiz yere
    /// etkilenirdi — alan ayrımı burada da geçerli.
    pub cursor_signing_key: String,
}

// Sırlar hata mesajlarında veya loglarda görünmesin diye elle Debug.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("server", &self.server)
            .field("database", &"<gizli>")
            .field("redis", &"<gizli>")
            .field("storage", &"<gizli>")
            .field("security", &"<gizli>")
            // Sır değil: operasyonda "hangi limitler yürürlükte" sorusuna
            // loglardan cevap verebilmek daha değerli.
            .field("rate_limits", &self.rate_limits)
            .field("storage_quota", &self.storage_quota)
            .finish()
    }
}

impl Config {
    /// Ortamdan oku ve doğrula.
    ///
    /// # Errors
    /// Zorunlu bir değişken eksikse veya bir değer çözümlenemiyorsa.
    pub fn from_env() -> Result<Self, ConfigError> {
        let host: String = optional("APP_HOST")?.unwrap_or_else(|| "127.0.0.1".to_owned());
        let port: u16 = optional("APP_PORT")?.unwrap_or(3100);
        let addr = format!("{host}:{port}")
            .parse()
            .map_err(|e| ConfigError::Invalid {
                name: "APP_HOST/APP_PORT",
                detail: format!("{e}"),
            })?;

        let config = Self {
            server: ServerConfig {
                addr,
                request_timeout: Duration::from_secs(
                    optional("REQUEST_TIMEOUT_SECS")?.unwrap_or(30),
                ),
                max_concurrent_requests: optional("MAX_CONCURRENT_REQUESTS")?.unwrap_or(512),
                max_body_bytes: optional("MAX_BODY_BYTES")?.unwrap_or(1024 * 1024),
                max_upload_bytes: optional("MAX_UPLOAD_BYTES")?.unwrap_or(8 * 1024 * 1024),
                trusted_proxy_hops: optional("TRUSTED_PROXY_HOPS")?.unwrap_or(0),
                // Varsayılan 6 saat: etiket çöpü birikmesi yavaş bir olgu
                // (yalnızca bir post'un son etiketi kalktığında oluşur),
                // sık koşmak advisory lock çekişmesinden başka bir şey
                // üretmez.
                tag_cleanup_interval: Duration::from_secs(
                    optional("TAG_CLEANUP_INTERVAL_SECS")?.unwrap_or(6 * 60 * 60),
                ),
                // Varsayılan 15 dakika. `hot_score` her oyla zaten anında
                // güncelleniyor; bu iş yalnızca zaman terimi kaydıkça
                // değerleri tazeliyor, sık koşmasının bir karşılığı yok.
                hot_score_interval: Duration::from_secs(
                    optional("HOT_SCORE_INTERVAL_SECS")?.unwrap_or(15 * 60),
                ),
                // Varsayılan 1 saat. Yetim yüklemeler 24 saatten eski
                // olduğunda siliniyor, yani daha sık koşmanın karşılığı yok.
                orphan_cleanup_interval: Duration::from_secs(
                    optional("ORPHAN_CLEANUP_INTERVAL_SECS")?.unwrap_or(60 * 60),
                ),
                // Varsayılan 1 saat: terfi eşikleri saat/gün mertebesinde
                // (24 saat, 7 gün), bu yüzden dakikalarca sık koşmanın
                // karşılığı yok — ama bir raporun onaylanmasından sonraki
                // düşürmenin makul bir sürede yansıması için `tag_cleanup`
                // kadar seyrek de değil (bkz. `crate::actor::
                // recompute_trust_levels`).
                trust_level_interval: Duration::from_secs(
                    optional("TRUST_LEVEL_INTERVAL_SECS")?.unwrap_or(60 * 60),
                ),
            },
            database: DatabaseConfig {
                url: required("DATABASE_URL")?,
                max_connections: optional("DATABASE_MAX_CONNECTIONS")?.unwrap_or(20),
                acquire_timeout: Duration::from_secs(
                    optional("DATABASE_ACQUIRE_TIMEOUT_SECS")?.unwrap_or(5),
                ),
            },
            redis: RedisConfig {
                url: required("REDIS_URL")?,
                pool_size: optional("REDIS_POOL_SIZE")?.unwrap_or(16),
            },
            storage: StorageConfig {
                endpoint: required("S3_ENDPOINT")?,
                region: optional("S3_REGION")?.unwrap_or_else(|| "us-east-1".to_owned()),
                bucket: required("MINIO_BUCKET")?,
                access_key: required("S3_ACCESS_KEY")?,
                secret_key: required("S3_SECRET_KEY")?,
                public_base_url: required("S3_PUBLIC_BASE_URL")?,
            },
            security: SecurityConfig {
                id_obfuscation_key: required("ID_OBFUSCATION_KEY")?,
                cursor_signing_key: required("CURSOR_SIGNING_KEY")?,
            },
            rate_limits: LimitTable::from_env()?,
            storage_quota: StorageQuotaConfig::from_env()?,
        };

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        // Üretimde varsayılan anahtarla çalışmak, dış ID'lerin tahmin
        // edilebilir olması demek. Sessizce geçiştirilecek bir şey değil.
        if self.security.id_obfuscation_key.len() < 32 {
            return Err(ConfigError::Invalid {
                name: "ID_OBFUSCATION_KEY",
                detail: "en az 32 karakter olmalı (openssl rand -hex 32)".to_owned(),
            });
        }
        if self.security.cursor_signing_key.len() < 32 {
            return Err(ConfigError::Invalid {
                name: "CURSOR_SIGNING_KEY",
                detail: "en az 32 karakter olmalı (openssl rand -hex 32)".to_owned(),
            });
        }
        if self.server.max_concurrent_requests == 0 {
            return Err(ConfigError::Invalid {
                name: "MAX_CONCURRENT_REQUESTS",
                detail: "sıfır olamaz".to_owned(),
            });
        }
        Ok(())
    }
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(ConfigError::Missing(name)),
    }
}

fn optional<T>(name: &'static str) -> Result<Option<T>, ConfigError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    match std::env::var(name) {
        Err(_) => Ok(None),
        Ok(raw) if raw.trim().is_empty() => Ok(None),
        Ok(raw) => raw
            .trim()
            .parse()
            .map(Some)
            .map_err(|e: T::Err| ConfigError::Invalid {
                name,
                detail: e.to_string(),
            }),
    }
}

// --- Hız sınırlama (rate limiting) limit tablosu -----------------------
//
// PLAN.md "Faz 6"daki varsayılan tablo burada kodlanır ama **sabit değil**:
// her hücre kendi `RATE_LIMIT_..._CAPACITY` / `..._WINDOW_SECS` çifti ile
// ortamdan override edilebilir. `actos_core::ratelimit::RateLimiter::check`,
// `Subject::Actor { actor_type, trust_level, .. }`'daki `actor_type`'a
// bakarak doğru temel kademeyi (human/ai_agent) seçip `trust_level`'a göre
// [`TRUST_LEVEL_CAPACITY_MULTIPLIER`] ile ölçeklemek (Faz 18.A) için bu
// tabloyu kullanır — çağıran tarafın kademe seçmesi gerekmez. `override_cfg`
// parametresi, seçilen bu kademenin **yerine tamamen geçen** (üstüne
// binmeyen), `actors.rate_limit_config` jsonb'sinden gelen **kişiye özel**
// bir override'dır (bkz. [`crate::ratelimit::config_from_json`]) — öncelik
// sırası kişiye özel override > güven kademesi çarpanı > actor_type temel
// kademesi (bkz. `RateLimiter::check` üzerindeki gerekçe).
//
// `Config`'in bir alanı: `Config::from_env()` başarısız olursa (ör. bir
// `RATE_LIMIT_..._CAPACITY` sıfırsa) süreç, tıpkı diğer yapılandırma
// hataları gibi, ilk isteği beklemeden açılışta durur.

/// Kimlikli (actor başına) istekler için, `actor_type`'a göre değişen
/// scope başına limitler.
#[derive(Clone, Copy, Debug)]
pub struct ScopeLimits {
    pub post: RateLimitConfig,
    pub comment: RateLimitConfig,
    pub vote: RateLimitConfig,
    pub read: RateLimitConfig,
    pub upload: RateLimitConfig,
    /// `GET /search` için ayrı kova (bkz. `crate::ratelimit::Scope::Search`
    /// üzerindeki gerekçe — arama genel okumadan belirgin ölçüde daha
    /// pahalı).
    pub search: RateLimitConfig,
    /// `GET /me/inbox` için ayrı kova (bkz. `crate::ratelimit::Scope::Inbox`
    /// üzerindeki gerekçe — sık yoklanması teşvik edilen bir uç, genel
    /// `read` kovasıyla paylaşılırsa ikisi birbirinin kotasını tüketir).
    pub inbox: RateLimitConfig,
}

/// Kimliksiz (IP başına) istekler için scope başına limitler.
#[derive(Clone, Copy, Debug)]
pub struct AnonymousLimits {
    pub register: RateLimitConfig,
    pub recover: RateLimitConfig,
    pub read: RateLimitConfig,
    /// `GET /search`, kimliksiz (IP başına) — bkz. [`ScopeLimits::search`]
    /// üzerindeki aynı gerekçe.
    pub search: RateLimitConfig,
    /// `GET /me/inbox`, kimliksiz (IP başına) — bu uç aslında kimlik
    /// gerektirir (`CurrentActor`), yani bu kova pratikte yalnızca
    /// `Authorization` header'ı olmadan/geçersiz gönderilmiş isteklere
    /// (bunlar zaten `401` alacak) uygulanır. Yine de hız sınırlama
    /// middleware'i kimlik çözümünden **önce** çalıştığı için (bkz.
    /// `crate::ratelimit` modül dokümantasyonu) bir karar üretebilmesi
    /// için bu kademenin var olması gerekiyor — bkz. [`ScopeLimits::inbox`].
    pub inbox: RateLimitConfig,
    /// Ayrıca tablolanmamış diğer tüm yazma uçları (ör. follow, delete) için
    /// tek, muhafazakâr bir kova. [`Scope::Write`] ve kimliksiz isteklerde
    /// [`Scope::Post`]/[`Scope::Comment`]/[`Scope::Vote`]/[`Scope::Upload`]
    /// gibi normalde kimlikli olması beklenen ama bir şekilde kimliksiz
    /// çağrılan scope'lar bu kovaya düşer.
    pub write: RateLimitConfig,
}

/// Güven kademesi (`actors.trust_level`, 0-2) başına rate limit **kapasite
/// çarpanı** — Faz 18.A, bkz. NOTES.md §9.3 ve §9.8. **Tek yerde tanımlı**:
/// büyütülecek/küçültülecek her ayar burada, başka hiçbir yerde bu üç sayı
/// tekrar yazılmamalı (bkz. [`LimitTable::scale_for_trust_level`]).
///
/// İndeks = `trust_level` (0, 1, 2).
///
/// **Neden `human`/`ai_agent` tablolarına ayrı bir üçüncü boyut (3 kademe ×
/// mevcut 7 scope alanı) eklenmedi de bir çarpan tercih edildi:** mevcut
/// tablolar zaten `actor_type` başına 7 alan taşıyor, hepsi ayrı ayrı env
/// değişkeniyle override edilebilir (`RATE_LIMIT_*_CAPACITY`/`_WINDOW_SECS`).
/// Kademe başına ayrı bir tablo bunu 3 katına çıkarırdı (42 alan + 84 env
/// değişkeni), `.env.example`'ı ve `LimitTable::validate`'i şişirir, ve en
/// önemlisi **hiçbir yeni bilgi taşımaz** — kademe zaten var olan temel
/// kapasiteyi ölçekliyor, yeni bir eksen açmıyor. Çarpan aynı ilkeyi
/// (kademe arttıkça kapasite artar) sıfır yeni env değişkeniyle sağlıyor.
///
/// **Neden `trust_level = 1` temel (1.0×) alındı, `0` değil:** `human`/
/// `ai_agent` tabloları zaten Faz 6'da "kurulmuş, normal davranan hesap"
/// için kalibre edilmişti — bu değerleri değiştirmeden kademe 1'in
/// karşılığı yapmak mevcut hiçbir üretim ayarını (env override'ları dahil)
/// bozmuyor. Kademe 0 (taze hesap, §9.3'ün asıl hedefi — bir gecede açılan
/// 100 hesabın manipülasyon/spam hızını kısmak) bunun **yarısı**; kademe 2
/// (yaşını kanıtlamış hesap) **iki katı**. 2×'in ötesine geçilmedi: amaç
/// kıdemli bir hesabı ödüllendirmek, `human`/`ai_agent` arasındaki temel
/// farkı (bazı scope'larda zaten 3-10×) gölgede bırakacak kadar büyütmek
/// değil.
const TRUST_LEVEL_CAPACITY_MULTIPLIER: [f64; 3] = [0.5, 1.0, 2.0];

/// Rate limiting için tüm varsayılan limitler: `human`/`ai_agent`
/// (kimlikli) ve `anonymous` (IP başına).
#[derive(Clone, Copy, Debug)]
pub struct LimitTable {
    pub human: ScopeLimits,
    pub ai_agent: ScopeLimits,
    pub anonymous: AnonymousLimits,
}

impl LimitTable {
    /// Bir `Scope` + `Subject` çifti için **varsayılan** (kişiye özel
    /// override'sız) limiti döner: `Subject::Actor { actor_type, trust_level,
    /// .. }` için `actor_type`'ın temel kademesi (bkz.
    /// [`Self::for_actor_type`]) `trust_level`'a göre ölçeklenir (bkz.
    /// [`Self::scale_for_trust_level`] ve [`TRUST_LEVEL_CAPACITY_MULTIPLIER`]);
    /// `Subject::Ip` için IP başına (anonim, güven kademesiz) tablo.
    ///
    /// [`crate::ratelimit::RateLimiter::check`] bunu **her zaman** çağırır —
    /// `override_cfg: None` verildiğinde döndürdüğü değer doğrudan kullanılır,
    /// `Some(cfg)` verildiğinde ise `cfg` bunun *yerine* geçer (kademe zaten
    /// doğru seçilmiş olur; `override_cfg`'nin işi kademe seçmek değil,
    /// `actors.rate_limit_config`'ten gelen kişiye özel bir sınırı
    /// uygulamaktır).
    #[must_use]
    pub fn resolve(&self, scope: Scope, subject: &Subject) -> RateLimitConfig {
        match subject {
            Subject::Actor {
                actor_type,
                trust_level,
                ..
            } => {
                let temel = self.for_actor_type(scope, *actor_type);
                Self::scale_for_trust_level(temel, *trust_level)
            }
            Subject::Ip(_) => match scope {
                Scope::Register => self.anonymous.register,
                Scope::Recover => self.anonymous.recover,
                Scope::Read => self.anonymous.read,
                Scope::Search => self.anonymous.search,
                Scope::Inbox => self.anonymous.inbox,
                Scope::Post | Scope::Comment | Scope::Vote | Scope::Upload | Scope::Write => {
                    self.anonymous.write
                }
            },
        }
    }

    /// Belirli bir `actor_type` için scope başına **temel** (henüz güven
    /// kademesi çarpanı uygulanmamış — `trust_level = 1`'in karşılığı, bkz.
    /// [`TRUST_LEVEL_CAPACITY_MULTIPLIER`] üzerindeki gerekçe) limiti döner.
    ///
    /// [`Self::resolve`] tarafından `Subject::Actor`'ın kendi `actor_type`'ı
    /// ile çağrılır, dönen değer ardından [`Self::scale_for_trust_level`]'a
    /// verilir — `RateLimiter::check` bunu otomatik yaptığı için normal
    /// akışta **doğrudan çağrılması gerekmez**. Public kalmasının nedeni
    /// test edilebilirlik ve HTTP katmanının (ör. bir yönetim panelinde
    /// "bu actor_type için mevcut limit ne?" göstermek gibi) ihtiyaç
    /// duyabileceği kenar durumlar.
    #[must_use]
    pub fn for_actor_type(&self, scope: Scope, actor_type: ActorType) -> RateLimitConfig {
        let tier = match actor_type {
            ActorType::AiAgent => &self.ai_agent,
            // İnsan, sistem botu ve organizasyon güvenli tarafta kalır
            // (human tier). Bu tipler için daha gevşek bir limit gerekiyorsa
            // actor'e özel `rate_limit_config` jsonb override'ı kullanılmalı.
            ActorType::Human | ActorType::SystemBot | ActorType::Organization => &self.human,
        };
        match scope {
            Scope::Post => tier.post,
            Scope::Comment => tier.comment,
            Scope::Vote => tier.vote,
            Scope::Read => tier.read,
            Scope::Upload => tier.upload,
            Scope::Search => tier.search,
            Scope::Inbox => tier.inbox,
            // Register/Recover/Write kimlikli actor'ler için tablolanmadı
            // (PLAN.md'de yalnızca IP başına tanımlı) — savunmacı varsayılan
            // olarak anonim "diğer yazmalar" kovasına düşer.
            Scope::Register | Scope::Recover | Scope::Write => self.anonymous.write,
        }
    }

    /// [`Self::for_actor_type`]'ın döndürdüğü **temel** (henüz güven
    /// kademesi uygulanmamış) kapasiteyi [`TRUST_LEVEL_CAPACITY_MULTIPLIER`]
    /// ile kademeye göre ölçekler — Faz 18.A "kademeye bağlı rate limit"
    /// (bkz. NOTES.md §9.3: "yüksek kademe daha geniş rate limit demek").
    ///
    /// Yalnızca **kapasite** ölçekleniyor, `window` aynı kalıyor: token
    /// bucket'ın dolum formülü (`elapsed_ms * capacity / window_ms`, bkz.
    /// `crate::ratelimit::BUCKET_SCRIPT_SRC`) kapasiteyle orantılı olduğu
    /// için pencereyi de değiştirmeye gerek yok — kapasiteyi ölçeklemek tek
    /// başına dolum hızını da doğru oranda ölçekler.
    ///
    /// `trust_level` şema düzeyinde `0..=2` ile sınırlı (bkz.
    /// `migrations/0020_trust_levels.up.sql` `ck_actors_trust_level_range`)
    /// ama bu fonksiyon savunmacı: aralık dışı bir değer çökmek yerine en
    /// yakın uca kenetlenir (`clamp`).
    ///
    /// Yuvarlama `round()` ile, sonuç en az `1`: kapasite sıfır olursa Lua
    /// script'i sıfıra böler (bkz. [`Self::validate`]'in aynı gerekçesi).
    #[must_use]
    fn scale_for_trust_level(cfg: RateLimitConfig, trust_level: i16) -> RateLimitConfig {
        let index = trust_level.clamp(0, (TRUST_LEVEL_CAPACITY_MULTIPLIER.len() - 1) as i16);
        #[allow(clippy::indexing_slicing)] // `clamp` üstteki satırda aralığı garanti ediyor.
        let multiplier = TRUST_LEVEL_CAPACITY_MULTIPLIER[index as usize];

        let scaled = (f64::from(cfg.capacity) * multiplier).round();
        // `scaled` negatif olamaz (capacity `u32`, multiplier > 0), yani tek
        // risk üstten taşma değil `1`'in altına düşmek — `max` bunu kapatıyor.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let capacity = (scaled as u32).max(1);

        RateLimitConfig {
            capacity,
            window: cfg.window,
        }
    }

    /// Ortamdan oku ve doğrula (bkz. [`Self::validate`]).
    ///
    /// # Errors
    /// Bir değer çözümlenemiyorsa veya bir kapasite sıfırsa.
    pub fn from_env() -> Result<Self, ConfigError> {
        // `$cap_env`/`$win_env` derleme zamanı string literalleri olduğu
        // için `optional::<T>(name: &'static str)` ile doğrudan uyumlu.
        macro_rules! rl {
            ($cap_env:literal, $win_env:literal, $default_capacity:expr, $default_window_secs:expr) => {
                RateLimitConfig {
                    capacity: optional($cap_env)?.unwrap_or($default_capacity),
                    window: Duration::from_secs(
                        optional($win_env)?.unwrap_or($default_window_secs),
                    ),
                }
            };
        }

        let table = Self {
            human: ScopeLimits {
                post: rl!(
                    "RATE_LIMIT_POST_HUMAN_CAPACITY",
                    "RATE_LIMIT_POST_HUMAN_WINDOW_SECS",
                    10,
                    3600
                ),
                comment: rl!(
                    "RATE_LIMIT_COMMENT_HUMAN_CAPACITY",
                    "RATE_LIMIT_COMMENT_HUMAN_WINDOW_SECS",
                    60,
                    3600
                ),
                vote: rl!(
                    "RATE_LIMIT_VOTE_HUMAN_CAPACITY",
                    "RATE_LIMIT_VOTE_HUMAN_WINDOW_SECS",
                    300,
                    3600
                ),
                read: rl!(
                    "RATE_LIMIT_READ_HUMAN_CAPACITY",
                    "RATE_LIMIT_READ_HUMAN_WINDOW_SECS",
                    600,
                    60
                ),
                upload: rl!(
                    "RATE_LIMIT_UPLOAD_HUMAN_CAPACITY",
                    "RATE_LIMIT_UPLOAD_HUMAN_WINDOW_SECS",
                    20,
                    3600
                ),
                // `read`'in (600/dk) yirmide biri: arama GIN taraması +
                // `ts_rank` hesaplaması taşıyor, sıradan bir
                // `GET /posts/{id}`'den belirgin ölçüde daha pahalı (bkz.
                // `Scope::Search` üzerindeki gerekçe). Oran kademeye göre
                // değişiyor: ajan 100/1200 (~1/12), anonim 20/120 (~1/6) —
                // ajanların arama yükünü daha çok taşıması bilinçli, bu
                // platformda keşif birinci sınıf bir kullanım.
                search: rl!(
                    "RATE_LIMIT_SEARCH_HUMAN_CAPACITY",
                    "RATE_LIMIT_SEARCH_HUMAN_WINDOW_SECS",
                    30,
                    60
                ),
                // İnsan bir istemcinin (ör. web arayüzü) inbox'ı birkaç
                // saniyede bir yoklaması makul; 120/dk buna bolca pay
                // bırakıyor (`read`in 600/dk'sının beşte biri — inbox tek
                // satır/sayfa okuduğu için `search` kadar pahalı değil, ama
                // yine de kendi kovasında, bkz. `Scope::Inbox` gerekçesi).
                inbox: rl!(
                    "RATE_LIMIT_INBOX_HUMAN_CAPACITY",
                    "RATE_LIMIT_INBOX_HUMAN_WINDOW_SECS",
                    120,
                    60
                ),
            },
            ai_agent: ScopeLimits {
                post: rl!(
                    "RATE_LIMIT_POST_AI_AGENT_CAPACITY",
                    "RATE_LIMIT_POST_AI_AGENT_WINDOW_SECS",
                    30,
                    3600
                ),
                comment: rl!(
                    "RATE_LIMIT_COMMENT_AI_AGENT_CAPACITY",
                    "RATE_LIMIT_COMMENT_AI_AGENT_WINDOW_SECS",
                    200,
                    3600
                ),
                vote: rl!(
                    "RATE_LIMIT_VOTE_AI_AGENT_CAPACITY",
                    "RATE_LIMIT_VOTE_AI_AGENT_WINDOW_SECS",
                    1000,
                    3600
                ),
                read: rl!(
                    "RATE_LIMIT_READ_AI_AGENT_CAPACITY",
                    "RATE_LIMIT_READ_AI_AGENT_WINDOW_SECS",
                    1200,
                    60
                ),
                upload: rl!(
                    "RATE_LIMIT_UPLOAD_AI_AGENT_CAPACITY",
                    "RATE_LIMIT_UPLOAD_AI_AGENT_WINDOW_SECS",
                    20,
                    3600
                ),
                search: rl!(
                    "RATE_LIMIT_SEARCH_AI_AGENT_CAPACITY",
                    "RATE_LIMIT_SEARCH_AI_AGENT_WINDOW_SECS",
                    100,
                    60
                ),
                // Bir ajanın `actos watch`-benzeri bir döngüyle (bkz.
                // NOTES.md §1 "Bağlı iş") daha sık yoklaması bekleniyor;
                // human'ın 2.5 katı.
                inbox: rl!(
                    "RATE_LIMIT_INBOX_AI_AGENT_CAPACITY",
                    "RATE_LIMIT_INBOX_AI_AGENT_WINDOW_SECS",
                    300,
                    60
                ),
            },
            anonymous: AnonymousLimits {
                register: rl!(
                    "RATE_LIMIT_REGISTER_IP_CAPACITY",
                    "RATE_LIMIT_REGISTER_IP_WINDOW_SECS",
                    3,
                    3600
                ),
                recover: rl!(
                    "RATE_LIMIT_RECOVER_IP_CAPACITY",
                    "RATE_LIMIT_RECOVER_IP_WINDOW_SECS",
                    5,
                    86_400
                ),
                read: rl!(
                    "RATE_LIMIT_READ_IP_CAPACITY",
                    "RATE_LIMIT_READ_IP_WINDOW_SECS",
                    120,
                    60
                ),
                search: rl!(
                    "RATE_LIMIT_SEARCH_IP_CAPACITY",
                    "RATE_LIMIT_SEARCH_IP_WINDOW_SECS",
                    20,
                    60
                ),
                write: rl!(
                    "RATE_LIMIT_WRITE_IP_CAPACITY",
                    "RATE_LIMIT_WRITE_IP_WINDOW_SECS",
                    30,
                    3600
                ),
                // Kimliksiz bir isteğin bu uca ulaşması yalnızca geçersiz/
                // eksik `Authorization` header'ı anlamına gelir (uç zaten
                // auth zorunlu) — düşük tutmak yeterli, bkz.
                // [`AnonymousLimits::inbox`] üzerindeki gerekçe.
                inbox: rl!(
                    "RATE_LIMIT_INBOX_IP_CAPACITY",
                    "RATE_LIMIT_INBOX_IP_WINDOW_SECS",
                    30,
                    60
                ),
            },
        };

        table.validate()?;
        Ok(table)
    }

    /// Sıfır kapasiteli bir kova, Lua script'inde sıfıra bölmeye yol açar
    /// (`window_ms / capacity`) — açılışta yakalanmalı, ilk isteğin 500
    /// döndürmesini beklememeli.
    fn validate(&self) -> Result<(), ConfigError> {
        let all = [
            ("RATE_LIMIT_POST_HUMAN_CAPACITY", self.human.post),
            ("RATE_LIMIT_COMMENT_HUMAN_CAPACITY", self.human.comment),
            ("RATE_LIMIT_VOTE_HUMAN_CAPACITY", self.human.vote),
            ("RATE_LIMIT_READ_HUMAN_CAPACITY", self.human.read),
            ("RATE_LIMIT_UPLOAD_HUMAN_CAPACITY", self.human.upload),
            ("RATE_LIMIT_SEARCH_HUMAN_CAPACITY", self.human.search),
            ("RATE_LIMIT_INBOX_HUMAN_CAPACITY", self.human.inbox),
            ("RATE_LIMIT_POST_AI_AGENT_CAPACITY", self.ai_agent.post),
            (
                "RATE_LIMIT_COMMENT_AI_AGENT_CAPACITY",
                self.ai_agent.comment,
            ),
            ("RATE_LIMIT_VOTE_AI_AGENT_CAPACITY", self.ai_agent.vote),
            ("RATE_LIMIT_READ_AI_AGENT_CAPACITY", self.ai_agent.read),
            ("RATE_LIMIT_UPLOAD_AI_AGENT_CAPACITY", self.ai_agent.upload),
            ("RATE_LIMIT_SEARCH_AI_AGENT_CAPACITY", self.ai_agent.search),
            ("RATE_LIMIT_INBOX_AI_AGENT_CAPACITY", self.ai_agent.inbox),
            ("RATE_LIMIT_REGISTER_IP_CAPACITY", self.anonymous.register),
            ("RATE_LIMIT_RECOVER_IP_CAPACITY", self.anonymous.recover),
            ("RATE_LIMIT_READ_IP_CAPACITY", self.anonymous.read),
            ("RATE_LIMIT_SEARCH_IP_CAPACITY", self.anonymous.search),
            ("RATE_LIMIT_WRITE_IP_CAPACITY", self.anonymous.write),
            ("RATE_LIMIT_INBOX_IP_CAPACITY", self.anonymous.inbox),
        ];
        for (name, cfg) in all {
            if cfg.capacity == 0 {
                return Err(ConfigError::Invalid {
                    name,
                    detail: "kapasite sıfır olamaz".to_owned(),
                });
            }
        }
        Ok(())
    }
}
