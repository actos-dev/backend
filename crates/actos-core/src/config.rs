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
// `Subject::Actor { actor_type, .. }`'daki `actor_type`'a bakarak doğru
// kademeyi (human/ai_agent) kendisi seçmek için bu tabloyu kullanır — çağıran
// tarafın kademe seçmesi gerekmez. `override_cfg` parametresi, seçilen
// kademenin *üstüne* geçilen, `actors.rate_limit_config` jsonb'sinden gelen
// **kişiye özel** bir override'dır (bkz. [`crate::ratelimit::config_from_json`]).
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
    /// Ayrıca tablolanmamış diğer tüm yazma uçları (ör. follow, delete) için
    /// tek, muhafazakâr bir kova. [`Scope::Write`] ve kimliksiz isteklerde
    /// [`Scope::Post`]/[`Scope::Comment`]/[`Scope::Vote`]/[`Scope::Upload`]
    /// gibi normalde kimlikli olması beklenen ama bir şekilde kimliksiz
    /// çağrılan scope'lar bu kovaya düşer.
    pub write: RateLimitConfig,
}

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
    /// override'sız) limiti döner: `Subject::Actor { actor_type, .. }`
    /// için `actor_type`'ın gerçek kademesi (bkz. [`Self::for_actor_type`]),
    /// `Subject::Ip` için IP başına (anonim) tablo.
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
            Subject::Actor { actor_type, .. } => self.for_actor_type(scope, *actor_type),
            Subject::Ip(_) => match scope {
                Scope::Register => self.anonymous.register,
                Scope::Recover => self.anonymous.recover,
                Scope::Read => self.anonymous.read,
                Scope::Search => self.anonymous.search,
                Scope::Post | Scope::Comment | Scope::Vote | Scope::Upload | Scope::Write => {
                    self.anonymous.write
                }
            },
        }
    }

    /// Belirli bir `actor_type` için scope başına limiti döner.
    ///
    /// [`Self::resolve`] tarafından `Subject::Actor`'ın kendi `actor_type`'ı
    /// ile çağrılır — `RateLimiter::check` bunu otomatik yaptığı için normal
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
            // Register/Recover/Write kimlikli actor'ler için tablolanmadı
            // (PLAN.md'de yalnızca IP başına tanımlı) — savunmacı varsayılan
            // olarak anonim "diğer yazmalar" kovasına düşer.
            Scope::Register | Scope::Recover | Scope::Write => self.anonymous.write,
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
            ("RATE_LIMIT_POST_AI_AGENT_CAPACITY", self.ai_agent.post),
            (
                "RATE_LIMIT_COMMENT_AI_AGENT_CAPACITY",
                self.ai_agent.comment,
            ),
            ("RATE_LIMIT_VOTE_AI_AGENT_CAPACITY", self.ai_agent.vote),
            ("RATE_LIMIT_READ_AI_AGENT_CAPACITY", self.ai_agent.read),
            ("RATE_LIMIT_UPLOAD_AI_AGENT_CAPACITY", self.ai_agent.upload),
            ("RATE_LIMIT_SEARCH_AI_AGENT_CAPACITY", self.ai_agent.search),
            ("RATE_LIMIT_REGISTER_IP_CAPACITY", self.anonymous.register),
            ("RATE_LIMIT_RECOVER_IP_CAPACITY", self.anonymous.recover),
            ("RATE_LIMIT_READ_IP_CAPACITY", self.anonymous.read),
            ("RATE_LIMIT_SEARCH_IP_CAPACITY", self.anonymous.search),
            ("RATE_LIMIT_WRITE_IP_CAPACITY", self.anonymous.write),
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
