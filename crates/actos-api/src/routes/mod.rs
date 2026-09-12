//! HTTP rotaları.
//!
//! ## OpenAPI (Faz 16) — `OpenApiRouter`, elle path listesi değil
//!
//! Her alt modülün `router()` fonksiyonu `axum::Router<AppState>` değil
//! `utoipa_axum::router::OpenApiRouter<AppState>` döner; `.route("/x",
//! get(h))` yerine `.routes(routes!(h))` kullanılır. Bu iki değişikliğin
//! dışında rota mantığı hiç değişmedi.
//!
//! **Neden bu, `#[derive(OpenApi)] #[openapi(paths(...))]` içinde elle bir
//! fonksiyon listesi tutmaktan daha invaziv bir yaklaşım değil mi?** Evet,
//! ama bilinçli bir tercih: elle tutulan bir liste, yeni bir uç eklenip o
//! listeye eklenmesi unutulduğunda **sessizce** eksik bir spec üretir —
//! derleme de, test de bunu yakalamaz. `OpenApiRouter` + `routes!()` ise
//! rotanın axum'a kaydını *ve* OpenAPI şemasına kaydını **aynı çağrıda**
//! yapar (bkz. `routes!` makrosunun ürettiği `(schemas, paths,
//! method_router)` üçlüsü) — bir uç axum'da yaşıyorsa spec'te de yaşar,
//! ikisinin ayrı düşmesi derleme zamanında imkânsız. PLAN.md'nin "spec'in
//! kodla senkron kaldığını doğrulayan CI kontrolü" maddesi bu yüzden ayrı
//! bir CI adımı gerektirmiyor: garanti zaten burada, derleme zamanında.
//!
//! `routes!(a, b)` **yalnızca aynı URL yoluna** (farklı HTTP metotlarıyla)
//! sahip handler'ları birleştirmek için kullanılır (bkz. `utoipa_axum::routes!`
//! makro dokümantasyonu) — ör. `/auth/keys` için `routes!(create_key,
//! list_keys)`. Farklı yollara `.routes()` her zaman ayrı ayrı çağrılıyor.
//!
//! ## `GET /openapi.json` ve `GET /docs`
//!
//! İkisi de **hız sınırından ve kimlik doğrulamadan muaf** (bkz.
//! `crate::middleware::ratelimit::classify`): bir ajan API'yi öğrenmeden bir
//! API key alamaz — `GET /docs`/`GET /openapi.json`'ın kendisi kimlik
//! gerektirseydi bu döngüsel olurdu. Hız sınırından muaf olmaları da aynı
//! gerekçeyle: `/health`/`/version` gibi bunlar da bir ajanın *ilk* isteği
//! olabilir, henüz hiçbir kotaya sahip değilken.
//!
//! `GET /docs` (Scalar UI) spec'i **HTML'in içine gömerek** sunuyor
//! (`utoipa_scalar::Scalar::to_html`, `$spec` yer tutucusu) — yani sayfa
//! açıldığında `/openapi.json`'a ayrı bir istek atmıyor, ikisi birbirinden
//! bağımsız.
//!
//! ## `GET /docs/agent` (Faz 16, ikinci yarı)
//!
//! Yukarıdaki ikisinin **aksine** `/docs/agent` normal bir uç gibi
//! `routes!(meta::agent_docs)` ile kaydediliyor — yani spec'in kendisinde
//! görünüyor (`GET /openapi.json`'da `/docs/agent` bir yol olarak var,
//! `/openapi.json`/`/docs` yok). Sebebi görevin kendisi: bu uç bir ajanın
//! *okuyacağı* bir dokümantasyon olmanın yanında, kendisi de spec'in
//! parçası olan sıradan bir kaynak — `/openapi.json`/`/docs` ise spec'in
//! **sunum biçimleri**, spec'in bir "yolu" değil.
//!
//! İçeriği (metin gövdesi) bu fonksiyonun **sonunda**, `split_for_parts()`
//! ile elde edilen nihai `openapi` değerinden üretilip önbelleğe alınıyor
//! (bkz. `meta::cache_endpoint_reference`) — hız sınırından/kimlikten muaf
//! olması ise yukarıdakiyle birebir aynı gerekçe: `crate::middleware::ratelimit::classify`.
//!
//! ## `GET /metrics` (Faz 17)
//!
//! `/openapi.json`/`/docs` gibi spec'in bir "yolu" değil — Prometheus'un
//! kazıdığı (scrape) işletimsel bir uç, OpenAPI'de görünmüyor (bkz. yukarıki
//! `OpenApiRouter` bölümü — bu, `OpenApiRouter`'a hiç kaydedilmediği için
//! zaten yapısal olarak imkânsız, elle bir "hariç tut" listesi gerekmiyor).
//!
//! **Kimlik doğrulama gerektirmiyor, hız sınırından muaf** (bkz.
//! `crate::middleware::ratelimit::classify`). Bilinçli bir seçim, iki
//! gerekçeyle:
//!
//! 1. Prometheus'un kendisi metrik uçlarına kimlik bilgisiyle scrape yapacak
//!    şekilde tasarlanmadı (`bearer_token`/`basic_auth` desteği var ama bu,
//!    Actos'un `actos_<key_id>_<secret>` API key şemasıyla eşleşen ayrı bir
//!    kimlik doğrulama yolu inşa etmeyi gerektirirdi — operasyonel bir
//!    uç için orantısız bir karmaşıklık).
//! 2. Servis yalnızca `127.0.0.1`'e bağlanıyor (bkz. `.env`, `APP_HOST`) —
//!    dışarıya açık değil. **Bu kalıcı bir garanti değil**, bir dağıtım
//!    kararı; servis bir gün `0.0.0.0`'a açılırsa bu uç trafik
//!    hacmi/hata oranı gibi operasyonel bilgiyi (ama hiçbir kullanıcı
//!    verisini/kimlik bilgisini/iş verisini **değil** — etiketler yalnızca
//!    `method`/`route` şablonu/`status`/`scope`) dışarıya sızdırır hâle
//!    gelir. Bu senaryoda doğru düzeltme uygulama katmanında bir auth
//!    kontrolü eklemek değil (yanlış katman — bir scrape endpoint'i bir
//!    "actor" değil), reverse-proxy/güvenlik grubu seviyesinde `/metrics`'i
//!    yalnızca Prometheus'un IP'sine kısıtlamak olurdu (bkz. Faz 19
//!    "Paketleme ve Deploy" — bu görevin kapsamı dışında, ama gelecekteki
//!    bir okuyucu için not düşülüyor).
//!
//! `crate::telemetry::observe` **buraya eklenmiyor** — `/metrics`'in kendisi
//! `route_layer`'dan önce, ayrı bir `.route(...)` ile kaydediliyor (aşağıya
//! bakın), yani kendi scrape'lerini kendi trafiği olarak saymıyor. Aksi hâlde
//! `http_requests_total{route="/metrics"}` serisi yalnızca "metrikler ne
//! sıklıkla okunuyor" der, gerçek API trafiğiyle ilgisiz bir gürültü katardı.
//!
//! DB havuzu gauge'ları (`db_pool_connections`) **arka planda değil, scrape
//! anında** güncelleniyor (bkz. `crate::telemetry::record_db_pool_gauges`).
//!
//! ## `GET /docs`'un CSP'si neden ayrı
//!
//! `/docs` (Scalar UI) `https://cdn.jsdelivr.net/npm/@scalar/api-reference`
//! adresinden bir JS paketi çekiyor (ölçüldü: `curl` ile sayfanın kendisi ve
//! bu paket indirilip incelendi) — `crate::app::DEFAULT_CSP`'nin
//! `default-src 'none'`'u bunu bloklar, sayfa boş kalır. Diğer her uç saf
//! JSON döndüğü için katı varsayılanı hak ediyor, yalnızca `/docs`'un kendi,
//! **daha gevşek** bir CSP'ye ihtiyacı var — bu yüzden iki farklı politika:
//! `crate::app::DEFAULT_CSP` her yanıta `if_not_present` ile düşer,
//! [`DOCS_CSP`] yalnızca bu modülün altta kurduğu `/docs`'a özel alt-router'a
//! (`docs_router`) katman olarak ekleniyor — tower'ın onion modelinde iç
//! katman (buradaki) dış katmandan (`app::build`) **önce** çalıştığı için
//! `if_not_present` onu ezmiyor (bkz. `crate::app::build` dokümanındaki
//! `SecurityHeaders` maddesi).
//!
//! [`DOCS_CSP`] içeriği ölçüme dayalı ama tam değil: `script-src` ve
//! `font-src` gerçekten indirilen dosyalardan doğrulandı (JS paketi
//! `https://fonts.scalar.com/*.woff2` istiyor, başka bir CDN'e dokunmuyor).
//! `style-src 'unsafe-inline'` ve `connect-src` (sayfanın "Try it" paneli
//! API'ye doğrudan istek atabilsin diye `'self'`) ise Scalar'ın bilinen
//! mimarisinden **çıkarım** — bu depoda tarayıcı yok, gerçek bir tarayıcıda
//! DevTools konsolu açıp CSP ihlali olup olmadığını doğrulamak hâlâ
//! öneriliyor. Bozulursa önce burası kontrol edilmeli.

pub mod actors;
pub mod admin;
pub mod auth;
pub mod comments;
pub mod feed;
pub mod health;
pub mod interactions;
pub mod meta;
pub mod notifications;
pub mod posts;
pub mod search;
pub mod tags;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, header},
    middleware::from_fn,
    response::IntoResponse,
    routing::get,
};
use tower_http::set_header::SetResponseHeaderLayer;
use utoipa::OpenApi as _;
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_scalar::{Scalar, Servable as _};

use crate::{error::ApiError, openapi::ApiDoc, state::AppState, telemetry};

/// `/docs`'a özel, gevşetilmiş `Content-Security-Policy` — gerekçe ve
/// ölçüm kaynağı bu dosyanın başındaki "`GET /docs`'un CSP'si neden ayrı"
/// bölümünde.
const DOCS_CSP: &str = "default-src 'none'; \
    script-src 'self' https://cdn.jsdelivr.net; \
    style-src 'self' 'unsafe-inline'; \
    font-src 'self' https://fonts.scalar.com; \
    img-src 'self' data: https:; \
    connect-src 'self' https://proxy.scalar.com; \
    worker-src 'self' blob:; \
    base-uri 'none'";

/// Uygulamanın rota ağacı. Katmanlar burada değil, `app` içinde eklenir.
///
/// `max_upload_bytes`: threaded through to `actors::router` so it can size
/// the avatar route's own `DefaultBodyLimit` override at router-construction
/// time — see `crate::app::build` (the caller, and where the value comes
/// from) and `actors::upload_avatar`'s doc for why a route-level layer is
/// needed at all.
pub fn router(max_upload_bytes: usize) -> Router<AppState> {
    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(health::live))
        .routes(routes!(health::ready))
        .routes(routes!(meta::version))
        .routes(routes!(meta::agent_docs))
        .merge(auth::router())
        .merge(actors::router(max_upload_bytes))
        .merge(posts::router())
        .merge(comments::router())
        .merge(tags::router())
        .merge(search::router())
        .merge(interactions::router())
        .merge(notifications::router())
        .merge(feed::router())
        .merge(admin::router())
        .split_for_parts();

    // `GET /docs/agent`'ın metnini bu nihai (tüm `.merge(...)`lardan sonraki)
    // `openapi` değerinden **bir kez** üretip önbelleğe alıyoruz — bkz.
    // `meta::cache_endpoint_reference` dokümanı. `meta::agent_docs` handler'ı
    // yukarıda `routes!(meta::agent_docs)` ile zaten kaydedildiği (dolayısıyla
    // spec'in kendisinde de göründüğü) için burada yalnızca *içeriğini*
    // dolduruyoruz — routing burada değişmiyor.
    meta::cache_endpoint_reference(&openapi);

    // Faz 17: `MatchedPath`e ihtiyaç duyan istek-metrikleri/log middleware'i
    // yalnızca buradaki **gerçek** rotalara (`health`/`meta`'dan `admin`'e
    // kadar yukarıdaki tüm `.merge(...)`lar) ekleniyor — `/openapi.json`,
    // `/docs`, `/metrics` bilerek dışarıda kalıyor (bkz. bu dosyanın başındaki
    // "`GET /metrics`" bölümü). `route_layer`, `Router::layer`'ın aksine
    // yalnızca **o ana kadar kayıtlı rotaları** sarmalıyor ve fallback'i hiç
    // kapsamıyor — bu yüzden aşağıda eklenecek üç uç ve `not_found` fallback'i
    // otomatik olarak muaf.
    let router = router.route_layer(from_fn(telemetry::observe));

    // `/openapi.json` bu üretilmiş `openapi` değerinden serveden ham bir
    // handler — `Arc` ile sarmalanıyor ki her istek tüm spec'i yeniden
    // klonlamak yerine yalnızca referans sayacını artırsın (spec küçük
    // olmasa da bu uç sık çağrılan bir "hot path" değil, yine de bedelsiz
    // bir optimizasyon).
    let spec = Arc::new(openapi.clone());

    // `/docs`'un kendi CSP'si: bkz. bu dosyanın başındaki "`GET /docs`'un
    // CSP'si neden ayrı" bölümü ve [`DOCS_CSP`]. Yalnızca bu alt-router'a
    // (tek rota: `/docs`) ekleniyor, `crate::app::DEFAULT_CSP`'yi değil.
    let docs_router: Router<AppState> = Scalar::with_url("/docs", openapi).into();
    let docs_router = docs_router.layer(SetResponseHeaderLayer::overriding(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(DOCS_CSP),
    ));

    router
        .route(
            "/openapi.json",
            get(move || {
                let spec = Arc::clone(&spec);
                async move { Json((*spec).clone()) }
            }),
        )
        .merge(docs_router)
        .route("/metrics", get(metrics))
        .fallback(not_found)
}

/// `GET /metrics` — Prometheus text-format kazıma (scrape) uç.
///
/// Kimlik doğrulama/hız sınırından muaf ve OpenAPI spec'inde **görünmüyor**
/// olma gerekçeleri bu dosyanın başındaki "`GET /metrics`" bölümünde.
async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    telemetry::record_db_pool_gauges(state.db());
    let body = telemetry::prometheus_handle().render();
    (
        [(
            header::CONTENT_TYPE,
            // Prometheus text exposition format — `version=0.0.4` istemci
            // tarafında hâlâ yaygın beklenen, geriye dönük uyumlu bir etiket
            // (OpenMetrics `charset`/`escaping` parametrelerine ihtiyacımız
            // yok, `metrics-exporter-prometheus` klasik text format üretiyor).
            "text/plain; version=0.0.4",
        )],
        body,
    )
}

/// Eşleşmeyen rotalar için de aynı hata biçimi.
///
/// Varsayılan davranış boş gövdeli bir 404 döndürmek olurdu; istemcilerin
/// (özellikle ajanların) her hatayı tek bir şemayla ayrıştırabilmesi için
/// burada da `application/problem+json` üretiyoruz.
async fn not_found(headers: HeaderMap) -> ApiError {
    ApiError::new(actos_core::Error::NotFound("route")).with_request_id(&headers)
}
