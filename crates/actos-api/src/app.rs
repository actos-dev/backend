//! Router'ın kurulumu: rotalar + middleware yığını.

use std::time::Duration;

use axum::{
    Router,
    http::{HeaderName, StatusCode},
    middleware::from_fn_with_state,
};
use tower::ServiceBuilder;
use tower_http::{
    LatencyUnit,
    catch_panic::CatchPanicLayer,
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    request_id::{PropagateRequestIdLayer, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    timeout::TimeoutLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

use crate::{
    middleware::{identity, ratelimit},
    routes,
    state::AppState,
    telemetry::MakeRequestUuidV7,
};

/// Rotaları ve middleware yığınını birleştirir.
///
/// Katman sırası önemli — dıştan içe:
/// 1. `SetRequestId` — sonraki her katman kimliği görebilsin diye en dışta
/// 2. `Trace` — kimlik atandıktan sonra, ki log satırlarında yer alsın
/// 3. `PropagateRequestId` — kimliği yanıta da yaz
/// 4. `CatchPanic` — panik olursa süreç ölmesin, 500 dönsün
/// 5. `SetSensitiveRequestHeaders` — `Authorization` loglara düşmesin
/// 6. `Cors`
/// 7. `Timeout` — asılı kalan istek bağlantıyı sonsuza dek tutmasın
/// 8. `ConcurrencyLimit` — doygunlukta kuyruğa yığmak yerine reddet
/// 9. `identity::resolve` — `Authorization`'ı **bir kez** çözer, sonucu
///    request extension'ına koyar (bkz. `crate::middleware::identity`).
///    Handler'ların/extractor'ların ikinci kez `authenticate` çağırmaması
///    ve hız sınırlamanın (aşağıdaki katman) doğru `Subject`'i seçebilmesi
///    için `RequestBodyLimit`'ten önce, `ConcurrencyLimit`'ten sonra
///    çalışıyor — kabul edilmeyecek (doygunlukta reddedilen) bir istek için
///    boşuna veritabanına gitmesin.
/// 10. `ratelimit::enforce` — `identity`'den **sonra** (Subject'e ihtiyacı
///     var), `RequestBodyLimit`'ten **önce** (gövdeye hiç dokunmuyor, erken
///     reddetmek daha ucuz). `X-RateLimit-*`/`Retry-After` header'larını
///     buradan sonraki her yanıta (401/404 dahil) ekler.
/// 11. `RequestBodyLimit` — en içte, gövde okunmadan hemen önce
pub fn build(state: AppState) -> Router {
    let cfg = state.config().server.clone();
    let request_id = HeaderName::from_static(crate::telemetry::REQUEST_ID_HEADER);

    let middleware = ServiceBuilder::new()
        .layer(SetRequestIdLayer::new(
            request_id.clone(),
            MakeRequestUuidV7,
        ))
        .layer(
            TraceLayer::new_for_http().on_response(
                DefaultOnResponse::new()
                    .level(Level::INFO)
                    .latency_unit(LatencyUnit::Millis),
            ),
        )
        .layer(PropagateRequestIdLayer::new(request_id))
        .layer(CatchPanicLayer::new())
        .layer(SetSensitiveRequestHeadersLayer::new([
            axum::http::header::AUTHORIZATION,
            axum::http::header::COOKIE,
        ]))
        .layer(cors())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            cfg.request_timeout,
        ))
        .layer(tower::limit::ConcurrencyLimitLayer::new(
            cfg.max_concurrent_requests,
        ))
        .layer(from_fn_with_state(state.clone(), identity::resolve))
        .layer(from_fn_with_state(state.clone(), ratelimit::enforce))
        .layer(RequestBodyLimitLayer::new(cfg.max_body_bytes));

    routes::router().layer(middleware).with_state(state)
}

/// CORS politikası.
///
/// Actos'un amacı herkesin kendi istemcisini yazabilmesi; tarayıcıdan gelen
/// isteklere origin kısıtı koymak bu amaca ters düşer. Kimlik doğrulama
/// çerezle değil `Authorization` header'ıyla yapıldığı için `allow_credentials`
/// kapalı — CSRF yüzeyi yok.
fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers([
            HeaderName::from_static("x-request-id"),
            HeaderName::from_static("x-ratelimit-limit"),
            HeaderName::from_static("x-ratelimit-remaining"),
            HeaderName::from_static("x-ratelimit-reset"),
            HeaderName::from_static("retry-after"),
        ])
        .max_age(Duration::from_secs(86400))
}
