//! Actos REST API sunucusu.

use std::process::ExitCode;

use actos_api::{app, state};
use actos_core::{Config, Storage, cache, db, id::IdCodec};
use tower::Layer as _;
use tower_http::normalize_path::NormalizePathLayer;

#[tokio::main]
async fn main() -> ExitCode {
    // Geliştirmede .env; üretimde gerçek ortam değişkenleri kullanılır,
    // dosyanın yokluğu hata değildir.
    let _ = dotenvy::dotenv();
    actos_api::telemetry::init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("başlatılamadı: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    tracing::info!(?config, "yapılandırma yüklendi");

    // Üçü de burada doğrulanıyor: bağımlılığı olmayan bir sunucu ayağa
    // kalkıp sağlıklı görünmesin.
    let db = db::connect(&config.database).await?;
    let redis = cache::connect(&config.redis).await?;
    let storage = Storage::new(&config.storage);
    storage.ping().await?;
    tracing::info!(bucket = storage.bucket(), "nesne depolama hazır");

    // Boş anahtarla açılış başarısız olmalı — dış ID'lerin tahmin edilebilir
    // olması demek (bkz. `crates/actos-core/src/id.rs`).
    let id_codec = IdCodec::new(&config.security.id_obfuscation_key)?;

    let addr = config.server.addr;
    let state = state::AppState::new(config, db, redis, storage, id_codec);

    // NormalizePath yönlendirmeden önce çalışmalı, o yüzden router'ın
    // dışında kalıyor: `/posts/` ile `/posts` aynı rotaya düşsün.
    let service = NormalizePathLayer::trim_trailing_slash().layer(app::build(state));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("actos-api dinlemede: http://{addr}");

    axum::serve(
        listener,
        <_ as axum::ServiceExt<axum::extract::Request>>::into_make_service(service),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    tracing::info!("kapandı");
    Ok(())
}

/// SIGTERM/SIGINT geldiğinde işlenmekte olan isteklerin bitmesini bekler.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => tracing::error!("SIGTERM dinlenemedi: {e}"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("SIGINT alındı, kapanılıyor"),
        () = terminate => tracing::info!("SIGTERM alındı, kapanılıyor"),
    }
}
