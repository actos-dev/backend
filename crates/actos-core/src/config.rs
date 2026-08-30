//! Ortam değişkenlerinden yapılandırma.
//!
//! Kural: **eksik ya da bozuk yapılandırma açılışta fark edilir.** Uygulama
//! yarı yapılandırılmış şekilde ayağa kalkıp ilk isteği aldığında patlamaz;
//! `Config::from_env()` başarısız olursa süreç hiç başlamaz.

use std::{fmt, net::SocketAddr, str::FromStr, time::Duration};

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
            },
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
