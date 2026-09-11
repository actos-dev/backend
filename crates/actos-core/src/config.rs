//! Ortam değişkenlerinden yapılandırma.
//!
//! Kural: **eksik ya da bozuk yapılandırma açılışta fark edilir.** Uygulama
//! yarı yapılandırılmış şekilde ayağa kalkıp ilk isteği aldığında patlamaz;
//! `Config::from_env()` başarısız olursa süreç hiç başlamaz.

use std::{fmt, net::SocketAddr, str::FromStr, time::Duration};

use crate::ratelimit::{RateLimitConfig, Scope, Subject};

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
    /// Total storage quota per actor (see NOTES.md §9.8). A **separate field**
    /// from `rate_limits`: one limits rate (requests/second), the other limits
    /// accumulation (total bytes) — conceptually different axes, and merging
    /// them into a single structure would be an artificial forcing, since the
    /// quota needs to carry its own absolute byte value (`500 MB` is not a
    /// multiplier, it's an absolute number).
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

/// Total storage quota per actor (bytes) — NOTES.md §9.8: defense against
/// the scenario of *"opening 1000 accounts and, with each one, uploading
/// 100 8 MB images to fill up the disk."*
///
/// **Why `MAX_UPLOAD_BYTES` wasn't enough:** it's an upper bound **per
/// single file** (see `ServerConfig::max_upload_bytes`) — a patient
/// attacker reaches the same total size over time. This quota limits
/// **accumulation** (total bytes), regardless of rate.
///
/// **The value is read from the environment, not hardcoded** (task
/// requirement): it's an absolute byte count — a platform-specific
/// operational decision that the operator needs to tune to their storage
/// capacity, not something that belongs baked into the code (just like
/// `MAX_UPLOAD_BYTES`).
///
/// Before trust levels were removed, this was split into three tiers
/// (50 MB/500 MB/2 GB) (see REFACTOR.md §3); now it's a **single flat
/// value** for everyone, 500 MB.
#[derive(Clone, Copy, Debug)]
pub struct StorageQuotaConfig {
    pub bytes: i64,
}

impl StorageQuotaConfig {
    /// Ortamdan oku ve doğrula (bkz. [`Self::validate`]).
    ///
    /// # Errors
    /// If the value can't be parsed, or is zero or negative.
    pub fn from_env() -> Result<Self, ConfigError> {
        let table = Self {
            bytes: optional("STORAGE_QUOTA_BYTES")?.unwrap_or(500 * 1024 * 1024),
        };
        table.validate()?;
        Ok(table)
    }

    /// A zero or negative quota is a nonsensical configuration in which
    /// *no* upload in `create_attachment` could ever succeed ("current
    /// usage (>= 0) + new file (> 0) always exceeds the quota") — this
    /// must be caught at startup.
    fn validate(&self) -> Result<(), ConfigError> {
        if self.bytes <= 0 {
            return Err(ConfigError::Invalid {
                name: "STORAGE_QUOTA_BYTES",
                detail: "sıfır veya negatif olamaz".to_owned(),
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
// The default table is hardcoded here but it is **not fixed**: every cell
// can be overridden from the environment via its own `RATE_LIMIT_..._CAPACITY`
// / `..._WINDOW_SECS` pair. All authenticated (per-actor) requests **share a
// single table** — there is no longer a separate `human`/`ai_agent` pair
// that varies by `actor_type`, nor a trust-level multiplier (see
// REFACTOR.md §1 and §3: both were removed as part of the same effort).
// `actos_core::ratelimit::RateLimiter::check` uses this table directly,
// regardless of `Subject::Actor`'s identity. The `override_cfg` parameter
// is a **per-actor** override, coming from the `actors.rate_limit_config`
// jsonb column, that **completely replaces** this table (rather than
// layering on top of it) (see [`crate::ratelimit::config_from_json`]) — the
// priority order is per-actor override > the single table's default (see
// the rationale on `RateLimiter::check`).
//
// `Config`'in bir alanı: `Config::from_env()` başarısız olursa (ör. bir
// `RATE_LIMIT_..._CAPACITY` sıfırsa) süreç, tıpkı diğer yapılandırma
// hataları gibi, ilk isteği beklemeden açılışta durur.

/// Per-scope limits for authenticated (per-actor) requests — a single
/// table shared by all actors (see the rationale at the top of the module).
#[derive(Clone, Copy, Debug)]
pub struct ScopeLimits {
    pub post: RateLimitConfig,
    pub comment: RateLimitConfig,
    pub vote: RateLimitConfig,
    pub read: RateLimitConfig,
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
    /// A single, conservative bucket for all other write endpoints that
    /// aren't otherwise tabulated (e.g. follow, delete). [`Scope::Write`],
    /// and — for unauthenticated requests — scopes like [`Scope::Post`]/
    /// [`Scope::Comment`]/[`Scope::Vote`] that would normally be expected to
    /// be authenticated but somehow got called without authentication, fall
    /// into this bucket. The same bucket is also used for an authenticated
    /// actor's `Register`/`Recover`/`Write` scopes (see [`LimitTable::
    /// resolve`]) — these three scopes never had a separate actor table,
    /// since they were only ever defined per IP anyway.
    pub write: RateLimitConfig,
}

/// All default rate-limiting limits: `actor` (authenticated, shared by all
/// actors) and `anonymous` (per IP).
#[derive(Clone, Copy, Debug)]
pub struct LimitTable {
    pub actor: ScopeLimits,
    pub anonymous: AnonymousLimits,
}

impl LimitTable {
    /// Returns the **default** (without a per-actor override) limit for a
    /// `Scope` + `Subject` pair: for `Subject::Actor`, the [`Self::actor`]
    /// table — everyone gets the same capacity regardless of identity
    /// (actor_type) (see the rationale at the top of the module); for
    /// `Subject::Ip`, the per-IP (anonymous) table.
    ///
    /// [`crate::ratelimit::RateLimiter::check`] **always** calls this — when
    /// `override_cfg: None` is given, the returned value is used directly;
    /// when `Some(cfg)` is given, `cfg` takes its place *instead*
    /// (`override_cfg`'s job isn't to choose this default, it's to apply a
    /// per-actor limit coming from `actors.rate_limit_config`).
    #[must_use]
    pub fn resolve(&self, scope: Scope, subject: &Subject) -> RateLimitConfig {
        match subject {
            Subject::Actor { .. } => match scope {
                Scope::Post => self.actor.post,
                Scope::Comment => self.actor.comment,
                Scope::Vote => self.actor.vote,
                Scope::Read => self.actor.read,
                Scope::Search => self.actor.search,
                Scope::Inbox => self.actor.inbox,
                // Register/Recover/Write were never tabulated for
                // authenticated actors (only defined per IP) — if an
                // authenticated caller ends up hitting these scopes
                // (defensive: in practice `classify` only maps these scopes
                // to anonymous routes), it falls into the anonymous "other
                // writes" bucket.
                Scope::Register | Scope::Recover | Scope::Write => self.anonymous.write,
            },
            Subject::Ip(_) => match scope {
                Scope::Register => self.anonymous.register,
                Scope::Recover => self.anonymous.recover,
                Scope::Read => self.anonymous.read,
                Scope::Search => self.anonymous.search,
                Scope::Inbox => self.anonymous.inbox,
                Scope::Post | Scope::Comment | Scope::Vote | Scope::Write => self.anonymous.write,
            },
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
            // Values were taken from the old `ai_agent` table (see the
            // REFACTOR.md §1 decision table): when merging into a single
            // table, the wider side was chosen so that no one ends up with
            // a narrower capacity than what they have today.
            actor: ScopeLimits {
                post: rl!(
                    "RATE_LIMIT_POST_CAPACITY",
                    "RATE_LIMIT_POST_WINDOW_SECS",
                    30,
                    3600
                ),
                comment: rl!(
                    "RATE_LIMIT_COMMENT_CAPACITY",
                    "RATE_LIMIT_COMMENT_WINDOW_SECS",
                    200,
                    3600
                ),
                vote: rl!(
                    "RATE_LIMIT_VOTE_CAPACITY",
                    "RATE_LIMIT_VOTE_WINDOW_SECS",
                    1000,
                    3600
                ),
                read: rl!(
                    "RATE_LIMIT_READ_CAPACITY",
                    "RATE_LIMIT_READ_WINDOW_SECS",
                    1200,
                    60
                ),
                // One twentieth of `read`: search carries a GIN scan plus
                // `ts_rank` computation, making it noticeably more expensive
                // than a plain `GET /posts/{id}` (see the rationale on
                // `Scope::Search`).
                search: rl!(
                    "RATE_LIMIT_SEARCH_CAPACITY",
                    "RATE_LIMIT_SEARCH_WINDOW_SECS",
                    100,
                    60
                ),
                // A client is expected to poll frequently with an `actos
                // watch`-like loop (see NOTES.md §1 "Connected work");
                // one quarter of `read`.
                inbox: rl!(
                    "RATE_LIMIT_INBOX_CAPACITY",
                    "RATE_LIMIT_INBOX_WINDOW_SECS",
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
            ("RATE_LIMIT_POST_CAPACITY", self.actor.post),
            ("RATE_LIMIT_COMMENT_CAPACITY", self.actor.comment),
            ("RATE_LIMIT_VOTE_CAPACITY", self.actor.vote),
            ("RATE_LIMIT_READ_CAPACITY", self.actor.read),
            ("RATE_LIMIT_SEARCH_CAPACITY", self.actor.search),
            ("RATE_LIMIT_INBOX_CAPACITY", self.actor.inbox),
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
