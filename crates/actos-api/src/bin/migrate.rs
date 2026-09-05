//! Veritabanı şemasını göç ettiren (migrate) tek seferlik job.
//!
//! **Neden ayrı bir binary, API açılışında otomatik değil** (PLAN.md Faz 19):
//! üretimde API birden fazla instance olarak koşabilir. Migration'ı açılışa
//! bağlamak, aynı anda kalkan üç instance'ın aynı `ALTER TABLE`'ı sürmesi
//! demektir. `sqlx` bir `_sqlx_migrations` kilidi tutuyor, yani veri
//! bozulmaz — ama iki instance kilidi beklerken sağlıksız görünür ve
//! dağıtım gereksiz yere yavaşlar. Ayrı job, sırayı açık hale getirir:
//! önce migrate, sonra API.
//!
//! **Neden `Config::from_env()` değil:** göç yalnızca `DATABASE_URL`
//! istiyor. Tam yapılandırmayı okumak bu job'ı S3 anahtarlarına, Redis'e ve
//! `ID_OBFUSCATION_KEY`'e bağımlı yapardı — hiçbirine dokunmayan bir işe
//! gereksiz sır dağıtmak olurdu.
//!
//! Migration dosyaları `sqlx::migrate!` ile derleme zamanında binary'ye
//! gömülüdür (bkz. `actos_core::db::MIGRATOR`); imajın içinde `migrations/`
//! dizini bulunmasına gerek yoktur.
//!
//! Kullanım:
//! ```text
//! DATABASE_URL=postgres://... cargo run -p actos-api --bin migrate
//! ```

use std::process::ExitCode;

use actos_core::db::MIGRATOR;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> ExitCode {
    // Geliştirmede .env; üretimde gerçek ortam değişkenleri kullanılır,
    // dosyanın yokluğu hata değildir (bkz. `main.rs`, aynı gerekçe).
    let _ = dotenvy::dotenv();
    actos_api::telemetry::init();

    match run().await {
        Ok(uygulanan) => {
            tracing::info!(uygulanan, "migration tamamlandı");
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::error!("migration başarısız: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<i64, Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL tanımlı değil — migration job'ı yalnızca bunu ister")?;

    // Tek bağlantı yeter: göç sıralı koşar, havuz genişletmenin karşılığı yok.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect(&url)
        .await?;

    // Uygulanmış göçleri koşmadan önce say ki "kaç tanesi bu turda uygulandı"
    // loglanabilsin — sessizce hiçbir şey yapmayan bir job ile gerçekten
    // ilerleyen bir job'ı ayırt etmek dağıtım sırasında işe yarıyor.
    let onceki = uygulanan_gocler(&pool).await?;
    MIGRATOR.run(&pool).await?;
    let sonraki = uygulanan_gocler(&pool).await?;

    pool.close().await;
    Ok(sonraki.saturating_sub(onceki))
}

/// `_sqlx_migrations` tablosundaki satır sayısı; tablo henüz yoksa 0.
///
/// Varlık kontrolü ayrı bir sorgu olmak zorunda: Postgres tablo adını
/// *ayrıştırma* zamanında çözdüğü için `CASE WHEN to_regclass(...)` gibi bir
/// koruma ilk koşuda yine hata verirdi.
async fn uygulanan_gocler(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
    let var: bool = sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
        .fetch_one(pool)
        .await?;

    if !var {
        return Ok(0);
    }

    sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await
}
