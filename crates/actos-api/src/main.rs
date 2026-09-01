//! Actos REST API sunucusu.

use std::process::ExitCode;

use actos_api::{app, jobs, state};
use actos_core::{
    Config, Storage, cache, cursor::CursorCodec, db, id::IdCodec, idempotency::IdempotencyStore,
    ratelimit::RateLimiter,
};
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

    // `CursorCodec::new` başarısızlıkla dönmez (bkz. `cursor.rs`) — anahtar
    // uzunluğu zaten `Config::validate` içinde denetlendi.
    let cursor_codec = CursorCodec::new(&config.security.cursor_signing_key);

    // `redis.clone()` ucuz: `deadpool_redis::Pool` zaten `Arc` tabanlı.
    let rate_limiter = RateLimiter::new(redis.clone(), config.rate_limits);
    let idempotency = IdempotencyStore::new(redis.clone());

    let addr = config.server.addr;
    let tag_cleanup_interval = config.server.tag_cleanup_interval;
    let hot_score_interval = config.server.hot_score_interval;
    let state = state::AppState::new(
        config,
        db,
        redis,
        storage,
        id_codec,
        cursor_codec,
        rate_limiter,
        idempotency,
    );

    // Periyodik bakım işleri (Faz 10 ve 12). İkisi de kendi advisory
    // lock'ını kendi içinde alıyor, bu yüzden birden fazla instance
    // çalışsa bile aynı anda yalnızca birinde iş yapılıyor — bkz.
    // `crate::jobs` modül dokümantasyonu.
    jobs::spawn_periodic(
        "etiket temizliği",
        state.db().clone(),
        tag_cleanup_interval,
        |pool| async move { actos_core::tag::cleanup_unused(&pool).await },
    );
    jobs::spawn_periodic(
        "hot score tazeleme",
        state.db().clone(),
        hot_score_interval,
        |pool| async move { actos_core::feed::recompute_hot_scores(&pool).await },
    );

    // NormalizePath yönlendirmeden önce çalışmalı, o yüzden router'ın
    // dışında kalıyor: `/posts/` ile `/posts` aynı rotaya düşsün.
    let service = NormalizePathLayer::trim_trailing_slash().layer(app::build(state));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("actos-api dinlemede: http://{addr}");

    // `into_make_service_with_connect_info`: hız sınırlama middleware'inin
    // IP başına limit uygulayabilmesi için `ConnectInfo<SocketAddr>`
    // extension'ı her isteğe eklenmesi gerekiyor (bkz.
    // `crates/actos-api/src/middleware/ratelimit.rs`). `NormalizePathLayer`
    // bunun **dışında** kalıyor ama bu satır bütün `service`'i (NormalizePath
    // + router) sarmaladığı için extension router'a ulaşana kadar zaten
    // ekli oluyor.
    axum::serve(
        listener,
        <_ as axum::ServiceExt<axum::extract::Request>>::into_make_service_with_connect_info::<
            std::net::SocketAddr,
        >(service),
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
