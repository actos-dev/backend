//! Faz 17 (gözlemlenebilirlik + güvenlik header'ları) entegrasyon testleri:
//! `GET /metrics`, kardinalite garantisi, güvenlik header'ları, `/docs`'un
//! bozulmadığı.
//!
//! Kurulum yardımcıları `tests/openapi.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! **Prometheus kaydı (recorder) bu binary'nin tüm testleri arasında
//! paylaşılan global bir durum** (bkz. `actos_api::telemetry::
//! prometheus_handle` — süreç başına tek `OnceLock`). Bu yüzden aşağıdaki
//! testler mutlak sayaç değerlerine (`== N`) değil, yalnızca *varlığa* ve
//! *kardinaliteye* (kaç farklı zaman serisi açıldığına) bakıyor —
//! `cargo test` aynı binary içindeki testleri paralel çalıştırdığında bir
//! testin sayacı diğerini etkileyebilir, ama hangi *etiketlerin* ortaya
//! çıktığı (kardinalite) testler arası çakışmadan bağımsız, güvenilir bir
//! sinyal.

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
    http::{Request, StatusCode},
};
use sqlx::PgPool;
use tower::ServiceExt as _;

// --- Kurulum yardımcıları (bkz. `tests/openapi.rs` — aynı desen) ---------

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

// --- `GET /metrics` -------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn metrics_200_prometheus_formatinda_ve_kimliksiz_erisilebilir(pool: PgPool) {
    let router = build_router(pool);

    // Önce en az bir "gerçek" isteği tetikleyip `http_requests_total`ın boş
    // olmadığından emin oluyoruz (bkz. `crate::telemetry::observe` — yalnızca
    // `route_layer`'a eklenen rotalar sayılıyor, `/health` bunlardan biri).
    let (health_status, _, _) = send(&router, empty_req("GET", "/health")).await;
    assert_eq!(health_status, StatusCode::OK);

    let (status, body, headers) = send(&router, empty_req("GET", "/metrics")).await;
    assert_eq!(status, StatusCode::OK);

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/plain"),
        "beklenen text/plain, gelen: {content_type}"
    );

    // Kimlik doğrulama header'ı **hiç** gönderilmedi (bkz. `empty_req`) ve
    // yine de 200 döndü — `GET /metrics`'in kimliksiz erişilebilir olması
    // gerektiği kararının kanıtı (bkz. `crate::routes` modül dokümanındaki
    // "GET /metrics" bölümü).
    let text = String::from_utf8(body).expect("prometheus çıktısı geçerli UTF-8 olmalı");
    assert!(
        text.contains("http_requests_total"),
        "en az bir istek metriği bulunmalı, gövde: {text}"
    );
    assert!(
        text.contains("db_pool_connections"),
        "DB havuzu gauge'ları bulunmalı, gövde: {text}"
    );
    assert!(
        text.contains(r#"state="idle""#) && text.contains(r#"state="active""#),
        "db_pool_connections hem idle hem active etiketiyle görünmeli, gövde: {text}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn metrics_ratelimit_headerinden_muaf(pool: PgPool) {
    let router = build_router(pool);
    let (status, _, headers) = send(&router, empty_req("GET", "/metrics")).await;
    assert_eq!(status, StatusCode::OK);

    // `crate::middleware::ratelimit::classify` `/metrics`'i `None` (muaf)
    // döndürdüğü için hız sınırlama katmanı bu yanıta hiç dokunmamalı —
    // `X-RateLimit-*` header'larının **hiç bulunmaması** bunun kanıtı (bkz.
    // `crate::middleware::ratelimit::enforce`: muaf uçlarda `apply_headers`
    // hiç çağrılmıyor).
    assert!(
        headers.get("x-ratelimit-limit").is_none(),
        "/metrics hız sınırlama header'ı taşımamalı, taşıdı: {headers:?}"
    );
}

/// **Kardinalite testi.** Şablon (`/posts/{id}`) değil ham yol etiketlensen
/// her post için ayrı bir zaman serisi açılırdı — bu test iki *farklı* (ve
/// var olmayan) post id'sine istek atıp tek bir şablon serisi oluştuğunu,
/// ham id'lerin hiçbirinin metrik gövdesinde **hiç** geçmediğini doğruluyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn metrics_route_etiketi_sablon_ham_id_degil_kardinalite(pool: PgPool) {
    let router = build_router(pool);

    // Gerçek bir `IdCodec`in üretebileceği biçimde olmasına gerek yok —
    // axum'un rota eşleştirmesi salt sözdizimsel, `{id}` segmenti herhangi
    // bir string'i kabul eder; el ile bariz biçimde birbirinden farklı iki
    // rastgele değer kullanmak kardinalite iddiasını test etmek için yeterli
    // ve daha net (üretilen kod ile aynı encoding'e bağımlı değil).
    let id_one = format!("c_test_kardinalite_bir_{}", uuid::Uuid::new_v4().simple());
    let id_two = format!("c_test_kardinalite_iki_{}", uuid::Uuid::new_v4().simple());
    assert_ne!(id_one, id_two);

    let (status_one, _, _) = send(&router, empty_req("GET", &format!("/posts/{id_one}"))).await;
    let (status_two, _, _) = send(&router, empty_req("GET", &format!("/posts/{id_two}"))).await;
    // İkisi de var olmayan/çözümlenemeyen bir id — `404` bekleniyor, ama bu
    // testin asıl iddiası durum kodu değil aşağıdaki metrik gövdesi.
    assert_eq!(status_one, StatusCode::NOT_FOUND);
    assert_eq!(status_two, StatusCode::NOT_FOUND);

    let (metrics_status, body, _) = send(&router, empty_req("GET", "/metrics")).await;
    assert_eq!(metrics_status, StatusCode::OK);
    let text = String::from_utf8(body).expect("prometheus çıktısı geçerli UTF-8 olmalı");

    assert!(
        !text.contains(&id_one),
        "ham id ({id_one}) metrik gövdesinde hiç görünmemeli — kardinalite ihlali"
    );
    assert!(
        !text.contains(&id_two),
        "ham id ({id_two}) metrik gövdesinde hiç görünmemeli — kardinalite ihlali"
    );
    assert!(
        text.contains(r#"route="/posts/{id}""#),
        "her iki farklı id de aynı şablon etikete (`/posts/{{id}}`) düşmeli, gövde: {text}"
    );
}

// --- Güvenlik header'ları ---------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn guvenlik_headerlari_json_yanitlarda_var(pool: PgPool) {
    let router = build_router(pool);
    let (status, _, headers) = send(&router, empty_req("GET", "/health")).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        headers
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        headers.get("referrer-policy").and_then(|v| v.to_str().ok()),
        Some("no-referrer")
    );
    let csp = headers
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        csp.contains("default-src 'none'"),
        "JSON uçları en katı varsayılan CSP'yi almalı, gelen: {csp}"
    );
    // `/health` katı varsayılanı kullanmalı, `/docs`'un gevşek CSP'sini değil.
    assert!(
        !csp.contains("cdn.jsdelivr.net"),
        "katı varsayılan CSP `/docs`'a özel CDN'i içermemeli, gelen: {csp}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn docs_hala_200_html_ve_kendi_gevsek_cspsi_var(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, headers) = send(&router, empty_req("GET", "/docs")).await;
    assert_eq!(status, StatusCode::OK);

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/html"),
        "beklenen text/html, gelen: {content_type}"
    );

    let html = String::from_utf8(body).expect("/docs gövdesi geçerli UTF-8 olmalı");
    assert!(html.contains("<html"), "gövde bir HTML sayfası olmalı");
    assert!(
        html.contains("cdn.jsdelivr.net"),
        "Scalar UI paketinin script kaynağı hâlâ gömülü olmalı — sayfa bozulmamış"
    );

    let csp = headers
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        csp.contains("cdn.jsdelivr.net"),
        "/docs kendi gevşek CSP'sini almalı (script-src cdn.jsdelivr.net), gelen: {csp}"
    );
    assert_ne!(
        csp, "default-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        "/docs katı varsayılan CSP'yi almamalı"
    );
}

// --- Rate limit isabet sayacı -------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn rate_limit_isabeti_scope_etiketiyle_sayiliyor(pool: PgPool) {
    let router = build_router(pool);

    // `/auth/register` anonim kovası (`RATE_LIMIT_REGISTER_IP_CAPACITY`,
    // varsayılan 3/saat) — kapasiteyi aşana kadar art arda çağırıp en az bir
    // `429` üretiyoruz.
    let capacity = test_config().rate_limits.anonymous.register.capacity;
    for i in 0..=capacity {
        let body = format!(r#"{{"username":"rl_metrik_{i}","actor_type":"human"}}"#);
        let req = Request::builder()
            .method("POST")
            .uri("/auth/register")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("istek kurulabilmeli");
        send(&router, req).await;
    }

    let (status, body, _) = send(&router, empty_req("GET", "/metrics")).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).expect("prometheus çıktısı geçerli UTF-8 olmalı");

    assert!(
        text.contains(r#"rate_limit_rejections_total{scope="register"}"#),
        "en az bir `register` scope'lu ret sayılmalı, gövde: {text}"
    );
}
