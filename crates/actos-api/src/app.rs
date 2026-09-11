//! Router'ın kurulumu: rotalar + middleware yığını.

use std::time::Duration;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, HeaderValue, StatusCode},
    middleware::from_fn_with_state,
};
use tower::ServiceBuilder;
use tower_http::{
    LatencyUnit,
    catch_panic::CatchPanicLayer,
    cors::{Any, CorsLayer},
    request_id::{PropagateRequestIdLayer, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

use crate::{
    middleware::{identity, ratelimit},
    routes,
    state::AppState,
    telemetry::{self, MakeRequestUuidV7},
};

/// Rotaları ve middleware yığınını birleştirir.
///
/// Katman sırası önemli — dıştan içe:
/// 1. `SetRequestId` — sonraki her katman kimliği görebilsin diye en dışta
/// 2. `Trace` — kimlik atandıktan sonra, ki log satırlarında yer alsın
/// 3. `PropagateRequestId` — kimliği yanıta da yaz
/// 4. `CatchPanic` — panik olursa süreç ölmesin, 500 dönsün
/// 5. `SecurityHeaders` (Faz 17) — `X-Content-Type-Options`,
///    `Referrer-Policy`, `Content-Security-Policy`. `CatchPanic`'ten
///    **sonra** ki bir panikten dönen 500 de bu header'ları taşısın; asıl
///    rotalardan önce olması (`Cors`'tan bile önce) gereken bir sıra yok —
///    yalnızca yanıta yazıyor, isteğe hiç dokunmuyor (bkz. `security_headers`
///    dokümanı — CSP özelinde `/docs`'un kendi katmanıyla nasıl geçersiz
///    kıldığı orada anlatılıyor).
/// 6. `SetSensitiveRequestHeaders` — `Authorization` loglara düşmesin
/// 7. `Cors`
/// 8. `Timeout` — asılı kalan istek bağlantıyı sonsuza dek tutmasın
/// 9. `ConcurrencyLimit` — doygunlukta kuyruğa yığmak yerine reddet
/// 10. `identity::resolve` — `Authorization`'ı **bir kez** çözer, sonucu
///     request extension'ına koyar (bkz. `crate::middleware::identity`).
///     Handler'ların/extractor'ların ikinci kez `authenticate` çağırmaması
///     ve hız sınırlamanın (aşağıdaki katman) doğru `Subject`'i seçebilmesi
///     için `RequestBodyLimit`'ten önce, `ConcurrencyLimit`'ten sonra
///     çalışıyor — kabul edilmeyecek (doygunlukta reddedilen) bir istek için
///     boşuna veritabanına gitmesin.
/// 11. `ratelimit::enforce` — `identity`'den **sonra** (Subject'e ihtiyacı
///     var), gövde sınırından **önce** (gövdeye hiç dokunmuyor, erken
///     reddetmek daha ucuz). `X-RateLimit-*`/`Retry-After` header'larını
///     buradan sonraki her yanıta (401/404 dahil) ekler.
/// 12. `DefaultBodyLimit::max(cfg.max_body_bytes)` — en içte, `routes::
///     router()`'ı doğrudan sarıyor.
///
///     **Not `tower_http::limit::RequestBodyLimitLayer` — a per-route
///     override needs to be possible.** `RequestBodyLimitLayer` checks
///     `Content-Length` and wraps the raw body unconditionally, before axum
///     ever routes the request; nothing registered *inside* `routes::
///     router()` (i.e. on any individual route) runs early enough to change
///     that decision. `axum::extract::DefaultBodyLimit` works differently:
///     each layer that runs just stores a limit value on the request
///     (overwriting whatever value, if any, a previous `DefaultBodyLimit`
///     layer already stored there), and it's only actually enforced later,
///     lazily, by whichever extractor reads the body (`Json`, `Multipart`,
///     ...). Layers registered inside `routes::router()` — like the avatar
///     upload route's own `.layer(DefaultBodyLimit::max(max_upload_bytes))`
///     in `crate::routes::actors::router` — sit *inside* this one in the
///     tower stack, so they run *after* it and overwrite its value for
///     their own route before the extractor ever reads it. That's exactly
///     the override this fixes: without it, `POST /actors/me/avatar`'s own
///     limit was unreachable, because the outer `RequestBodyLimitLayer`
///     rejected anything past `max_body_bytes` (1 MiB) before the handler,
///     or even axum's router, ever ran — see REFACTOR.md §4's "Uploads
///     over 1 MB do not work at default config" for the bug this replaces.
///     `max_upload_bytes` is threaded down to `routes::router()` from here
///     (see that function's doc) because it, not this module, decides the
///     route's shape — this module only owns the app-wide default.
///
/// Bunların **hiçbiri** `MatchedPath`'e ihtiyaç duymuyor — Faz 17'nin
/// istek-metrikleri/yapılandırılmış-log middleware'i (`telemetry::observe`)
/// bu yüzden burada değil, `routes::router()`'ın içinde `Router::route_layer`
/// ile ekleniyor (bkz. `crate::telemetry` modül dokümanı "Neden `MatchedPath`
/// bu modülde..." bölümü) — axum rota eşleştirmesini bu `ServiceBuilder`
/// tamamen dışarıdan sardığı için burada asla göremez.
pub fn build(state: AppState) -> Router {
    // Faz 17: global `metrics` kaydını burada, ilk isteği hiç beklemeden
    // kur — `main.rs` ve her entegrasyon testi (`tests/*.rs`, hepsi bu
    // fonksiyonu çağırıyor) için tek ortak nokta burası. Kurulumu `GET
    // /metrics`'in ilk çağrısına ertelemiş olsaydık, o ana kadar `identity`/
    // `ratelimit::enforce`/`telemetry::observe`'un yazdığı sayaçlar
    // `metrics` crate'inin varsayılan no-op recorder'ına düşüp sessizce
    // kaybolurdu (bkz. `metrics::with_recorder`: kurulu bir global recorder
    // yoksa her çağrı o an için no-op'a düşer — kalıcı bir "unutma" değil,
    // ama o ana kadarki veri geri gelmez). `prometheus_handle()` süreç
    // başına tam bir kez kurduğu için (`OnceLock`) burada birden fazla
    // çağrılması (ör. testlerde her `build_router` çağrısı) zararsız.
    telemetry::prometheus_handle();

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
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static(REFERRER_POLICY),
        ))
        // `if_not_present`, `overriding` değil: `/docs` kendi (gevşek) CSP'sini
        // `crate::routes::router`'da, bu katmandan **önce** (yanıt daha içteyken,
        // ServiceBuilder onion modelinde) kendi rotasına özel bir katmanla
        // zaten koyuyor. Bu katman burada koşulsuz `overriding` olsaydı,
        // dıştaki (daha geç çalışan, çünkü yanıt dıştan-içe değil içten-dışa
        // akıyor) bu global katman `/docs`'un değerini ezip herkese aynı katı
        // politikayı dayatırdı. `if_not_present` tam olarak "başka biri zaten
        // karar verdiyse dokunma" anlamına geliyor — bkz. `crate::routes::router`
        // dokümanındaki `/docs` bölümü.
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static(DEFAULT_CSP),
        ))
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
        .layer(DefaultBodyLimit::max(cfg.max_body_bytes));

    routes::router(cfg.max_upload_bytes)
        .layer(middleware)
        .with_state(state)
}

/// `Referrer-Policy` seçimi: `no-referrer`.
///
/// Actos'ta okuma uçlarının çoğu kaynak-özel path segmentleri (`/posts/{id}`,
/// `/actors/{username}`) ya da sorgu string'i (`/search?q=...`) taşıyor —
/// `q` özellikle hassas olabilir (bir kullanıcının ne aradığı). `GET
/// /docs`'un gömdüğü spec metninde de üçüncü taraf bağlantılar var (GitHub,
/// scalar.com — bkz. `crate::routes::router`'daki `/docs` bölümü). Daha
/// gevşek bir seçenek olan `strict-origin-when-cross-origin` cross-origin
/// bir tıklamada en azından origin'i (şema+host) sızdırır; burada üçüncü
/// tarafların "bu isteğin actos'tan geldiğini" bilmesinin hiçbir operasyonel
/// faydası yok, `no-referrer` ile bu bilgi hiç gitmiyor.
const REFERRER_POLICY: &str = "no-referrer";

/// Varsayılan (API/JSON) `Content-Security-Policy`.
///
/// Bu servis `/docs` dışında **hiçbir yerde** HTML/JS üretmiyor — her yanıt
/// ya `application/problem+json` ya da düz `application/json`. Bir CSP'nin
/// koruduğu şey "bu sayfa çalışırken tarayıcı neyi yükleyip çalıştırabilir"
/// sorusu; JSON'un kendisi hiçbir şey yüklemediği için burada mümkün olan en
/// katı politika (`'none'`) güvenlik açısından bedelsiz — kırılacak hiçbir
/// meşru davranış yok. Yine de header'ı koymamızın nedeni savunma
/// derinliği: bir yanıt yanlışlıkla `Content-Type` sniffing ile HTML olarak
/// yorumlanırsa (`X-Content-Type-Options: nosniff` bunu zaten engellemeli,
/// ama iki bağımsız katman bir tekinden daha güvenli) ya da API'nin önüne
/// ileride statik bir şey (ör. bir hata sayfası) eklenirse, varsayılan katı
/// kalmaya devam eder — yalnızca `/docs` bilerek gevşetiliyor (bkz.
/// `crate::routes::router`).
const DEFAULT_CSP: &str = "default-src 'none'; base-uri 'none'; frame-ancestors 'none'";

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
