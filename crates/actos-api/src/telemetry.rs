//! Loglama, istek kimliği ve Prometheus metrikleri.
//!
//! ## Neden `MatchedPath` bu modülde bir `route_layer` middleware'i ile
//! okunuyor, `crate::app::build`'teki dış `ServiceBuilder` katmanında değil
//!
//! `axum::extract::MatchedPath` yalnızca axum'un kendi rota eşleştirmesi
//! **tamamlandıktan sonra** istek extension'larına konur. `crate::app::build`
//! içindeki `ServiceBuilder` zinciri (`SetRequestId`, `Trace`, ...,
//! `DefaultBodyLimit`) `routes::router()`'ın **dışını** sarmalıyor —
//! yani bu katmanlardaki bir `from_fn` middleware'i çalıştığında rota
//! eşleştirmesi henüz olmamıştır, `MatchedPath` orada hep `None` gelir. Bu
//! yüzden [`observe`] dış katmana değil, `crate::routes::router`'ın
//! **içine**, `Router::route_layer` ile ekleniyor — bu, eşleşmiş her rotayı
//! ayrı ayrı sarmalıyor, dolayısıyla handler'a ulaşıldığı an (ve `Next::run`
//! çağrılmadan önce) `req.extensions()`'ta `MatchedPath` zaten hazır oluyor.
//! (Kaynak: axum'un resmi `examples/prometheus-metrics` örneği aynı deseni
//! kullanıyor — burada icat edilmiş bir çözüm değil.)
//!
//! **Kardinalite:** `MatchedPath` ham yol değil şablon döner (`/posts/{id}`,
//! `/posts/c_7fGh2Kd` değil) — Prometheus'a her post/actor/... için ayrı bir
//! zaman serisi açılmasını engelleyen tam olarak bu. `route_layer` yalnızca
//! **eşleşmiş** rotalara eklendiği için 404 (fallback) hiç bu middleware'den
//! geçmiyor — o yolun kendisi zaten sınırsız kardinaliteli (istemcinin
//! attığı rastgele yol), metriğe hiç girmemesi bilinçli.
//!
//! `/openapi.json`, `/docs`, `/metrics`in kendisi `route_layer`'dan **önce**
//! eklenmiyor, bilerek `.route(...)`/`.merge(...)` ile ayrıca kaydediliyor
//! (bkz. `crate::routes::router`) — bunlar "trafik" değil sunum/işletim
//! uçları, `/health`/`/version` gibi gerçek API rotalarından ayrı tutuluyor.
//!
//! ## Neden iki ayrı log satırı (`TraceLayer` + [`observe`])
//!
//! `crate::app::build`'teki `TraceLayer` **her** isteği (rota eşleşmese de,
//! `429`/`503`/timeout gibi dış katmanlarda erken kesilenler dahil) INFO
//! seviyesinde logluyor ama yalnızca ham `uri`/`method`/`status`/`latency`
//! taşıyor — kimlik ya da rota şablonu bilmiyor (bilemez de, dış katmanda
//! çalışıyor). [`observe`] bunun **yerine değil yanına** ikinci, daha
//! zengin bir satır ekliyor (`actor_id`, şablon `route`, `status`,
//! `duration_ms`) ama yalnızca gerçekten eşleşmiş rotalar için. İki satırı
//! birleştirip `TraceLayer`'ı susturmak bu görevin dışında bir yeniden
//! yapılanma olurdu — o katman zaten çalışıyor (Faz 2), burada yalnızca
//! eksik olan alanları ekliyoruz.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use http::{HeaderValue, Request as HttpRequest};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use sqlx::PgPool;
use tower_http::request_id::{MakeRequestId, RequestId};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::middleware::identity::ResolvedIdentity;

/// `tracing`'i kur.
///
/// `LOG_FORMAT=json` ise makine-okunur çıktı (üretim), aksi halde okunabilir
/// çıktı (geliştirme). Seviye `RUST_LOG` ile ayarlanır.
pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("actos_api=info,actos_core=info,warn"));

    let json = std::env::var("LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));

    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry
            .with(fmt::layer().json().flatten_event(true))
            .init();
    } else {
        registry.with(fmt::layer().compact()).init();
    }
}

/// Her isteğe UUIDv7 kimlik üretir.
///
/// v4 yerine v7: zaman sıralı olduğu için log'larda ve veritabanı
/// index'lerinde ardışık gelir, karşılaştırılabilir.
#[derive(Clone, Copy, Default)]
pub struct MakeRequestUuidV7;

impl MakeRequestId for MakeRequestUuidV7 {
    fn make_request_id<B>(&mut self, _request: &HttpRequest<B>) -> Option<RequestId> {
        let id = uuid::Uuid::now_v7().to_string();
        HeaderValue::from_str(&id).ok().map(RequestId::new)
    }
}

/// İçinde bulunulan isteğin kimliği — hata yanıtlarına eklemek için.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

// --- Prometheus (Faz 17: `GET /metrics`) ------------------------------

/// Süreç başına **tam bir kez** kurulan global `metrics` kaydı (recorder).
///
/// `OnceLock::get_or_init`: hem `main.rs` (üretim) hem her entegrasyon test
/// binary'si (her biri kendi `AppState`'ini elle kurup `app::build`
/// çağırıyor, bkz. `tests/openapi.rs`'teki desen) aynı kod yolundan geçiyor
/// ve `metrics::set_global_recorder` süreç başına yalnızca bir kez
/// çağrılabiliyor — ayrı bir "önce kur" adımına gerek kalmadan ilk çağıran
/// kurar, sonrakiler aynı `&'static` referansı alır. `AppState`'e yeni bir
/// alan eklemek (ve onunla birlikte 10+ test dosyasındaki `AppState::new`
/// çağrısını güncellemek) yerine bilinçli olarak global: `metrics`
/// makrolarının (`counter!`/`histogram!`/`gauge!`) kendisi zaten süreç
/// genelinde global bir kayda yazıyor, `PrometheusHandle` da mantıksal
/// olarak o tekil kaydın bir görünümü — `AppState`'in taşıması gereken bir
/// "bağımlılık" değil.
static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// [`PROMETHEUS_HANDLE`]'ı döner, yoksa kurar.
///
/// `PrometheusBuilder::install_recorder()` **yalnızca** `metrics` facade'ını
/// global recorder olarak kaydeder — kendi HTTP sunucusunu açmaz (bkz. kök
/// `Cargo.toml`'daki `default-features = false` gerekçesi), `/metrics`'i biz
/// `crate::routes::router`'dan sunuyoruz.
///
/// `install_recorder()` başarısız olursa (yalnızca ulaşılamaz bir durumda:
/// bu fonksiyonun dışında birinin zaten bir global recorder kurmuş olması —
/// `OnceLock` bu fonksiyonun kendi içinden ikinci bir çağrıyı zaten
/// engelliyor) süreci `panic!`/`unwrap` ile düşürmek yerine bağımsız,
/// global'e **bağlanmamış** bir recorder'dan üretilmiş boş bir handle
/// dönülür: `/metrics` bu durumda boş/eksik veri döner ama süreç ayakta
/// kalır — gözlemlenebilirlik bir "nice to have", onun için birincil
/// işlevi (istek işlemeyi) durdurmaya değmez (bkz. `actos_core::idempotency`
/// modülündeki aynı felsefe: bir gözlem/kolaylık katmanının arızası ana işi
/// düşürmemeli).
pub fn prometheus_handle() -> &'static PrometheusHandle {
    PROMETHEUS_HANDLE.get_or_init(|| match PrometheusBuilder::new().install_recorder() {
        Ok(handle) => handle,
        Err(err) => {
            tracing::error!(
                error = %err,
                "prometheus recorder global olarak kurulamadı, /metrics eksik/boş veri dönecek",
            );
            PrometheusBuilder::new().build_recorder().handle()
        }
    })
}

/// [`PrometheusHandle::run_upkeep`]'i periyodik çalıştırır.
///
/// `install_recorder` (aksine `install`/`build`'e) bu bakım görevini
/// **otomatik başlatmıyor** (bkz. crate dokümantasyonunun "Upkeep and
/// maintenance" bölümü) — histogram dağılımlarının eskimiş (recency
/// penceresi dışına çıkmış) örnekleri bu çağrılmadan asla temizlenmez ve
/// bellek zamanla büyür. 5 saniyelik aralık keyfi değil: kütüphanenin
/// kendi `install`/`build` yolunun kullandığı varsayılan değerle aynı
/// (`metrics-exporter-prometheus` kaynağındaki `upkeep_timeout`).
///
/// Yalnızca `main.rs`'ten çağrılır — testler kısa ömürlü olduğu için bu
/// arka plan görevine ihtiyaç duymuyor (bkz. `tests/*.rs`, hiçbiri bunu
/// çağırmıyor).
pub fn spawn_upkeep(handle: &'static PrometheusHandle) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        loop {
            ticker.tick().await;
            handle.run_upkeep();
        }
    });
}

/// `/metrics` scrape edilirken DB havuzunun **o anki** durumunu Prometheus
/// gauge'larına yazar.
///
/// Arka planda periyodik bir görev yerine bilerek scrape-zamanlı: `sqlx::
/// Pool::size`/`num_idle` zaten ucuz, senkron, kilitsiz okumalar (atomic
/// sayaçlar) — periyodik bir görevle önceden hesaplayıp saklamanın hiçbir
/// tazelik kazancı olmaz, yalnızca gereksiz bir arka plan görevi ve
/// senkronizasyon yüzeyi ekler.
pub fn record_db_pool_gauges(pool: &PgPool) {
    let idle = pool.num_idle();
    let active = (pool.size() as usize).saturating_sub(idle);

    metrics::gauge!("db_pool_connections", "state" => "idle").set(idle as f64);
    metrics::gauge!("db_pool_connections", "state" => "active").set(active as f64);
}

/// `crate::routes::router`'a `Router::route_layer` ile eklenen, eşleşmiş
/// **her** rota için çalışan gözlemlenebilirlik middleware'i.
///
/// Neden `route_layer` (dış `ServiceBuilder` katmanı değil) ve neden iki ayrı
/// log satırı var: bu dosyanın başındaki modül dokümantasyonuna bakın.
///
/// İki şey üretir:
/// 1. Prometheus metrikleri: `http_requests_total` (sayaç) ve
///    `http_request_duration_seconds` (histogram), üçü de `method`/`route`/
///    `status` etiketli. `route` her zaman **şablon** (`MatchedPath`), ham
///    yol değil — kardinalite garantisi burada.
/// 2. Tek bir yapılandırılmış `tracing::info!` satırı: `actor_id`, `route`,
///    `method`, `status`, `duration_ms`.
///
/// **`actor_id` alanı kimliksiz isteklerde log satırında hiç görünmez**
/// (yok sayılır, `null` yazılmaz). Bilinçli seçim: `tracing`'in
/// `Option<T: Value>` desteği `None` için alanı tamamen atlıyor;
/// `application/problem+json` hatalarındaki `code` alanı gibi "yokluk
/// anlamlı" bir durumda `null` yazmak (varlığı ama boşluğu ima eder) yerine
/// alanın **hiç bulunmaması** (kimliksiz istek, tanım gereği bir actor'e
/// bağlı değil) hem JSON log tüketicilerinde (`actor_id` alanı `exists`
/// sorgusuyla ayrıştırılabilir) hem disk boyutunda daha temiz bir sinyal.
pub async fn observe(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().to_string();

    // Yalnızca eşleşmiş rotalara `route_layer` ile eklendiği için bu extension
    // pratikte her zaman dolu olmalı; `unwrap_or_else` savunmacı bir
    // yedek — ör. `crate::routes::not_found` fallback'i bu middleware'in
    // dışında kaldığı için oraya hiç düşülmez, ama axum'un iç davranışına
    // körü körüne güvenmemek adına burada da bir sentinel bırakılıyor.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "{unmatched}".to_owned());

    let actor_id: Option<i64> = req
        .extensions()
        .get::<ResolvedIdentity>()
        .and_then(|identity| match identity {
            ResolvedIdentity::Authenticated(actor) => Some(actor.actor.id),
            ResolvedIdentity::Anonymous | ResolvedIdentity::Failed(_) => None,
        });

    let response = next.run(req).await;

    let elapsed = start.elapsed();
    let status = response.status().as_u16();
    let status_str = status.to_string();

    metrics::counter!(
        "http_requests_total",
        "method" => method.clone(),
        "route" => route.clone(),
        "status" => status_str.clone(),
    )
    .increment(1);
    metrics::histogram!(
        "http_request_duration_seconds",
        "method" => method.clone(),
        "route" => route.clone(),
        "status" => status_str,
    )
    .record(elapsed.as_secs_f64());

    tracing::info!(
        actor_id,
        route = %route,
        method = %method,
        status,
        duration_ms = elapsed.as_millis() as u64,
        "istek tamamlandı",
    );

    response
}
