//! Redis bağlantı havuzu (rate limit sayaçları ve kısa ömürlü cache).

use deadpool_redis::{Config as PoolConfig, Pool, Runtime};
use redis::AsyncTypedCommands;

use crate::config::RedisConfig;

/// Havuzu kur ve `PING` ile doğrula.
///
/// # Errors
/// Havuz oluşturulamazsa veya Redis yanıt vermezse.
pub async fn connect(cfg: &RedisConfig) -> Result<Pool, CacheError> {
    let mut pool_cfg = PoolConfig::from_url(&cfg.url);
    pool_cfg.pool = Some(deadpool_redis::PoolConfig::new(cfg.pool_size));

    let pool = pool_cfg.create_pool(Some(Runtime::Tokio1))?;
    ping(&pool).await?;
    tracing::info!(pool_size = cfg.pool_size, "redis bağlantısı hazır");

    Ok(pool)
}

/// Sağlık kontrolü.
///
/// # Errors
/// Havuzdan bağlantı alınamazsa veya `PING` başarısız olursa.
pub async fn ping(pool: &Pool) -> Result<(), CacheError> {
    let mut conn = pool.get().await?;
    conn.ping().await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("redis havuzu kurulamadı: {0}")]
    Build(#[from] deadpool_redis::CreatePoolError),

    #[error("redis bağlantısı alınamadı: {0}")]
    Pool(#[from] deadpool_redis::PoolError),

    #[error("redis komutu başarısız: {0}")]
    Command(#[from] redis::RedisError),
}
