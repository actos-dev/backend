//! PostgreSQL bağlantı havuzu.

use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::DatabaseConfig;

/// Havuzu kur ve bağlantıyı **hemen** doğrula.
///
/// Tembel bağlantı kurmuyoruz: veritabanı erişilemiyorsa bunu açılışta
/// öğrenmek, ilk isteğin 500 dönmesinden iyidir.
///
/// # Errors
/// Bağlantı kurulamazsa veya ilk sorgu başarısız olursa.
pub async fn connect(cfg: &DatabaseConfig) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(cfg.acquire_timeout)
        // Uzun süre boşta kalan bağlantılar güvenlik duvarı/proxy tarafından
        // sessizce düşürülebiliyor; havuzda ölü bağlantı tutmayalım.
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        .test_before_acquire(true)
        .connect(&cfg.url)
        .await?;

    sqlx::query("SELECT 1").execute(&pool).await?;
    tracing::info!(
        max_connections = cfg.max_connections,
        "postgres bağlantısı hazır"
    );

    Ok(pool)
}

/// Sağlık kontrolü için hafif bir sorgu.
///
/// # Errors
/// Havuzdan bağlantı alınamazsa veya sorgu başarısız olursa.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(pool).await.map(|_| ())
}
