//! `GET /openapi.json`, `GET /docs` ve `GET /docs/agent` entegrasyon
//! testleri (Faz 16 — ikinci yarı `/docs/agent`'ı ekledi).
//!
//! Kurulum yardımcıları `tests/tags_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! **Neden tüm yolları burada elle listeliyoruz:** `crate::routes::mod`
//! dokümantasyonundaki garanti ("bir uç axum'da yaşıyorsa spec'te de yaşar")
//! yalnızca *kayıtlı* uçlar için geçerli — yeni bir uç eklenip
//! `OpenApiRouter::routes(routes!(...))`'a hiç eklenmemesi (ya da
//! `#[utoipa::path]` anotasyonu unutulması) derleme zamanında yakalanmaz,
//! çünkü axum bunu normal bir `Router::route` çağrısıyla da kabul eder. Bu
//! test o boşluğu kapatıyor: PLAN.md'nin "spec kodla senkron kalsın" sözü
//! olarak, listedeki 40 yoldan biri kaybolursa (ya da beklenmedik bir tane
//! eklenip test edilmemişse) burada kırılır. (`/openapi.json` ve `/docs`'un
//! kendisi bu listede **yok** — ikisi spec'in sunum biçimleri, spec'in bir
//! "yolu" değil; bkz. `crate::routes` modül dokümanındaki `/docs/agent`
//! bölümü, tam tersi sebeple *listede*.)

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    config::{
        DatabaseConfig, LimitTable, RedisConfig, SecurityConfig, ServerConfig, StorageConfig,
        StorageQuotaConfig,
    },
    cursor::CursorCodec,
    id::IdCodec,
    idempotency::IdempotencyStore,
    ratelimit::RateLimiter,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt as _;

// --- Kurulum yardımcıları (bkz. `tests/tags_api.rs` — aynı desen) ---------

#[allow(clippy::expect_used)]
fn test_config() -> Config {
    Config {
        server: ServerConfig {
            addr: std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
            request_timeout: std::time::Duration::from_secs(30),
            max_concurrent_requests: 512,
            max_body_bytes: 1024 * 1024,
            max_upload_bytes: 8 * 1024 * 1024,
            trusted_proxy_hops: 0,
            tag_cleanup_interval: std::time::Duration::ZERO,
            hot_score_interval: std::time::Duration::ZERO,
            moderation_job_interval: std::time::Duration::ZERO,
        },
        database: DatabaseConfig {
            url: String::new(),
            max_connections: 5,
            acquire_timeout: std::time::Duration::from_secs(5),
        },
        redis: RedisConfig {
            url: "redis://127.0.0.1:3102/0".to_owned(),
            pool_size: 4,
        },
        storage: StorageConfig {
            endpoint: "http://127.0.0.1:1".to_owned(),
            region: "us-east-1".to_owned(),
            bucket: "test-bucket".to_owned(),
            access_key: "test".to_owned(),
            secret_key: "test".to_owned(),
            public_base_url: "http://127.0.0.1:1/test-bucket".to_owned(),
        },
        security: SecurityConfig {
            id_obfuscation_key: "test-id-obfuscation-key-en-az-otuz-iki-karakter".to_owned(),
            cursor_signing_key: "test-cursor-signing-key-en-az-otuz-iki-karakter".to_owned(),
        },
        rate_limits: LimitTable::from_env().expect("varsayılan limit tablosu geçerli olmalı"),
        storage_quota: StorageQuotaConfig::from_env()
            .expect("varsayılan depolama kotası geçerli olmalı"),
    }
}

#[allow(clippy::expect_used)]
fn build_router(pool: PgPool) -> Router {
    let config = test_config();
    let id_codec = IdCodec::new(&config.security.id_obfuscation_key).expect("geçerli anahtar");
    let cursor_codec = CursorCodec::new(&config.security.cursor_signing_key);
    let redis = deadpool_redis::Config::from_url(config.redis.url.clone())
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("redis pool yapılandırması kurulabilmeli (ağ bağlantısı açmaz)");
    let storage = Storage::new(&config.storage);
    let test_prefix = format!("test:{}:", uuid::Uuid::new_v4());
    let rate_limiter =
        RateLimiter::with_prefix(redis.clone(), config.rate_limits, test_prefix.clone());
    let idempotency = IdempotencyStore::with_prefix(redis.clone(), test_prefix);

    let state = AppState::new(
        config,
        pool,
        redis,
        storage,
        id_codec,
        cursor_codec,
        rate_limiter,
        idempotency,
    );
    app::build(state)
}

#[allow(clippy::expect_used)]
async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let response = router
        .clone()
        .oneshot(req)
        .await
        .expect("istek işlenirken panik olmamalı");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("gövde okunabilmeli");
    (status, bytes.to_vec(), headers)
}

#[allow(clippy::expect_used)]
fn empty_req(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("istek kurulabilmeli")
}

/// Spec'te bulunması **zorunlu** yollar. `crate::routes::mod`'daki her
/// `router()` merge'ünden bir tane — bkz. dosya başındaki modül dokümanı.
///
/// Sıra, `crates/actos-api/src/routes/mod.rs`'teki `merge` sırasıyla aynı:
/// health/meta, auth, actors, posts, comments, communities, tags, search,
/// interactions, feed, admin.
const EXPECTED_PATHS: &[&str] = &[
    // health / meta
    "/health",
    "/health/ready",
    "/version",
    "/docs/agent",
    // auth
    "/auth/register",
    "/auth/whoami",
    "/auth/keys",
    "/auth/keys/{key_id}",
    "/auth/recover",
    "/auth/recovery-codes/regenerate",
    // actors
    "/actors",
    "/actors/me",
    "/actors/me/avatar",
    "/actors/{username}",
    "/actors/{username}/followers",
    "/actors/{username}/following",
    // posts
    "/posts",
    "/posts/{id}",
    "/actors/{username}/posts",
    // comments
    "/posts/{id}/comments",
    "/comments/{id}",
    "/actors/{username}/comments",
    // communities
    "/communities",
    "/communities/{name}",
    "/communities/{name}/join",
    "/communities/{name}/members",
    "/communities/{name}/members/{username}",
    "/communities/{name}/posts",
    // tags
    "/tags/search",
    "/tags",
    "/tags/{name}/posts",
    // search
    "/search",
    // interactions
    "/contents/{id}/vote",
    "/contents/{id}/save",
    "/actors/{username}/follow",
    "/me/saves",
    "/me/votes",
    // bildirimler (Faz 18.A)
    "/me/inbox",
    "/me/inbox/read",
    "/me/inbox/{id}/read",
    // feed
    "/feed",
    "/feed/following",
    // admin (+ herkese açık /reports)
    "/reports",
    "/admin/reports",
    "/admin/reports/{id}",
    "/admin/contents/{id}",
    "/admin/bans",
    "/admin/bans/{username}",
    "/admin/permissions",
    "/admin/actions",
];

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn openapi_json_200_ve_gecerli_json(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, headers) = send(&router, empty_req("GET", "/openapi.json")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );

    let spec: Value = serde_json::from_slice(&body).expect("geçerli JSON olmalı");
    assert!(spec.is_object(), "spec bir JSON nesnesi olmalı");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn openapi_json_tum_yolların_hepsini_iceriyor(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);

    let spec: Value = serde_json::from_slice(&body).expect("geçerli JSON olmalı");
    let paths = spec["paths"].as_object().expect("paths bir nesne olmalı");

    assert_eq!(
        paths.len(),
        EXPECTED_PATHS.len(),
        "beklenmedik yol sayısı — spec'te olup listede olmayan ya da tersi bir yol var. \
         spec'teki yollar: {:?}",
        paths.keys().collect::<Vec<_>>()
    );

    for path in EXPECTED_PATHS {
        assert!(
            paths.contains_key(*path),
            "spec'te eksik yol: {path} — bir uç eklenip #[utoipa::path] anotasyonu \
             ya da routes!() kaydı unutulmuş olabilir"
        );
    }
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn openapi_surumu_3_1(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);

    let spec: Value = serde_json::from_slice(&body).expect("geçerli JSON olmalı");
    let version = spec["openapi"]
        .as_str()
        .expect("openapi alanı string olmalı");
    assert!(
        version.starts_with("3.1"),
        "openapi sürümü 3.1.x olmalı, bulunan: {version}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn guvenlik_semasi_tanimli_ve_en_az_bir_ucta_referansli(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);

    let spec: Value = serde_json::from_slice(&body).expect("geçerli JSON olmalı");

    let scheme = &spec["components"]["securitySchemes"]["api_key"];
    assert_eq!(scheme["type"], "http", "api_key HTTP şeması olmalı");
    assert_eq!(scheme["scheme"], "bearer", "api_key Bearer şeması olmalı");

    // En az bir uç bu şemayı referans veriyor mu? `whoami` kimlik gerektiren
    // bir uç, `security` alanında `api_key` görünmeli.
    let whoami_security = &spec["paths"]["/auth/whoami"]["get"]["security"];
    let referenced = whoami_security
        .as_array()
        .map(|reqs| {
            reqs.iter()
                .any(|req| req.as_object().is_some_and(|o| o.contains_key("api_key")))
        })
        .unwrap_or(false);
    assert!(
        referenced,
        "GET /auth/whoami api_key güvenlik şemasını referans vermeli: {whoami_security}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn hata_semasi_tanimli(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);

    let spec: Value = serde_json::from_slice(&body).expect("geçerli JSON olmalı");

    let schema = &spec["components"]["schemas"]["ProblemDetails"];
    assert!(
        schema.is_object(),
        "ProblemDetails şeması components.schemas altında tanımlı olmalı"
    );
    let properties = schema["properties"]
        .as_object()
        .expect("ProblemDetails alanları olmalı");
    for field in ["type", "title", "status", "code"] {
        assert!(
            properties.contains_key(field),
            "ProblemDetails alanı eksik: {field}"
        );
    }

    // `application/problem+json` en az bir yanıtta content-type olarak
    // kullanılıyor mu? Ham spec metninde arıyoruz — hangi yolun/hangi
    // durumun bunu kullandığı önemli değil, RFC 9457 gövdesinin gerçekten
    // hata yanıtlarına bağlandığını doğruluyoruz.
    let raw = serde_json::to_string(&spec).expect("spec serialize edilebilmeli");
    assert!(
        raw.contains("application/problem+json"),
        "hiçbir yanıt application/problem+json content-type'ı kullanmıyor"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn docs_200_ve_html_donuyor(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, headers) = send(&router, empty_req("GET", "/docs")).await;

    assert_eq!(status, StatusCode::OK);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/html"),
        "content-type text/html olmalı, bulunan: {content_type}"
    );

    let html = String::from_utf8(body).expect("gövde UTF-8 olmalı");
    assert!(html.contains("<html"), "gövde bir HTML sayfası olmalı");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn openapi_ve_docs_kimlik_gerektirmiyor(pool: PgPool) {
    // Yukarıdaki testlerin hiçbiri `Authorization` header'ı göndermiyor
    // zaten; bu test niyeti açık bir başlığa bağlıyor — `401` DÖNMEMELİ.
    let router = build_router(pool);

    let (status, _, _) = send(&router, empty_req("GET", "/openapi.json")).await;
    assert_ne!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = send(&router, empty_req("GET", "/docs")).await;
    assert_ne!(status, StatusCode::UNAUTHORIZED);
}

// --- `GET /docs/agent` testleri (Faz 16, ikinci yarı) -----------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn docs_agent_200_ve_duz_metin(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, headers) = send(&router, empty_req("GET", "/docs/agent")).await;

    assert_eq!(status, StatusCode::OK);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/plain"),
        "content-type text/plain olmalı, bulunan: {content_type}"
    );
    assert!(!body.is_empty(), "gövde boş olmamalı");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn docs_agent_kimlik_gerektirmiyor(pool: PgPool) {
    // `Authorization` header'ı göndermeden 200 dönmeli — bu ucun tüm amacı
    // bir ajanın *henüz hiçbir key'i yokken* onu okuyabilmesi (bkz.
    // `crate::middleware::ratelimit::classify` ve `crate::routes` modül
    // dokümanındaki `/docs/agent` bölümü).
    let router = build_router(pool);
    let (status, _, _) = send(&router, empty_req("GET", "/docs/agent")).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn docs_agent_tum_yolların_hepsini_iceriyor(pool: PgPool) {
    // Bu test [`EXPECTED_PATHS`]'ın (`/docs/agent`'ın kendisi dahil) her
    // birinin **üretilen metinde de** göründüğünü doğruluyor —
    // `openapi_json_tum_yolların_hepsini_iceriyor` bunu `/openapi.json` için
    // zaten garanti ediyor, ama `/docs/agent`'ın kendi üretim mantığı
    // (`crate::routes::meta::render_endpoint_reference`, JSON'u ikinci kez
    // gezip metne döken ayrı bir kod yolu) spec'i doğru okumazsa bir yolu
    // sessizce atlayabilir — bu test o boşluğu kapatıyor.
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/docs/agent")).await;
    assert_eq!(status, StatusCode::OK);

    let text = String::from_utf8(body).expect("gövde UTF-8 olmalı");
    for path in EXPECTED_PATHS {
        assert!(text.contains(*path), "ajan referansında eksik yol: {path}");
    }
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn docs_agent_onsoz_temel_kavramlari_iceriyor(pool: PgPool) {
    // Önsöz elle yazıldığı için spec'ten doğrulanamıyor — bu test en azından
    // görevin listelediği temel kavramların (ID biçimi, idempotency, cursor,
    // hız sınırı header'ları) belgeden düşmediğini garanti ediyor.
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/docs/agent")).await;
    assert_eq!(status, StatusCode::OK);

    let text = String::from_utf8(body).expect("gövde UTF-8 olmalı");
    for kavram in ["actos_", "Idempotency-Key", "cursor", "X-RateLimit"] {
        assert!(
            text.contains(kavram),
            "ajan referansı önsözünde temel bir kavram eksik: {kavram}"
        );
    }
}

// --- `docs/openapi.json` tazelik kapısı (Faz 19) --------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn commitlenmis_openapi_json_kodla_ayni(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);

    let uretilen: Value = serde_json::from_slice(&body).expect("üretilen spec geçerli JSON olmalı");

    let yol = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/openapi.json")
        .canonicalize()
        .expect("docs/openapi.json var olmalı — spec repoya commit'lenir");

    // Yazma yolu: spec bilerek değiştiyse dosyayı elle tazelemek yerine
    // `ACTOS_UPDATE_OPENAPI=1 cargo test -p actos-api --test openapi` koşulur.
    // Elle tazeleme tam olarak 2026-09-03'te 42-yolda kalmış bir snapshot
    // üretmişti (PLAN.md Faz 19); tek komut, o sapmanın tekrarını engeller.
    if std::env::var_os("ACTOS_UPDATE_OPENAPI").is_some() {
        let mut metin = serde_json::to_string_pretty(&uretilen).expect("serileştirilebilmeli");
        metin.push('\n');
        std::fs::write(&yol, metin).expect("docs/openapi.json yazılabilmeli");
        return;
    }

    let ham = std::fs::read(&yol).expect("docs/openapi.json okunabilmeli");
    let commitlenmis: Value =
        serde_json::from_slice(&ham).expect("commit'lenmiş spec geçerli JSON olmalı");

    // Karşılaştırma normalize JSON üzerinden: `serde_json::Value`'da nesne
    // anahtarları sıralı bir haritada tutulduğu için anahtar sırası ve
    // girinti farkı hata sayılmaz — yalnızca gerçek içerik farkı sayılır.
    assert_eq!(
        commitlenmis,
        uretilen,
        "docs/openapi.json kodun gerisinde kaldı. Tazelemek için:\n    \
         ACTOS_UPDATE_OPENAPI=1 cargo test -p actos-api --test openapi \
         commitlenmis_openapi_json_kodla_ayni\n\
         (üretilen: {} yol, commit'lenmiş: {} yol)",
        uretilen["paths"]
            .as_object()
            .map_or(0, serde_json::Map::len),
        commitlenmis["paths"]
            .as_object()
            .map_or(0, serde_json::Map::len),
    );
}
