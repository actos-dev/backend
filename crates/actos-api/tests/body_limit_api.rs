//! Proves the app-wide `DefaultBodyLimit` (see `crate::app::build`) still
//! applies to an ordinary route that does **not** set its own per-route
//! override.
//!
//! This is the coverage gap `b2ddb8a` (the commit immediately before this
//! unit) left open on purpose: that commit replaced
//! `tower_http::limit::RequestBodyLimitLayer` with `axum::extract::
//! DefaultBodyLimit` so a route (the avatar upload) could widen the limit
//! for itself, and `avatar_api.rs` proves that widening works. But nothing
//! proved the OTHER half — that a route which never touches
//! `DefaultBodyLimit::max(...)` still inherits the app-wide ceiling set in
//! `crate::app::build` (`cfg.max_body_bytes`) rather than accidentally
//! becoming unlimited. `PATCH /actors/me` is used here specifically because
//! it is an "ordinary" JSON route: authenticated, a ordinary `Json<T>`
//! extractor, no multipart, no `DefaultBodyLimit` layer of its own anywhere
//! near it (compare `crate::routes::actors::router`, where only the avatar
//! sub-router gets one).

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
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt as _;

// --- Kurulum yardımcıları (bkz. `tests/actors_api.rs` — aynı desen) -------

#[allow(clippy::expect_used)]
fn test_config() -> Config {
    Config {
        server: ServerConfig {
            addr: std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
            request_timeout: std::time::Duration::from_secs(30),
            max_concurrent_requests: 512,
            // Deliberately small and exact, not the production default: the
            // test needs a body it can straightforwardly build that is
            // bigger than this number but trivially small in absolute
            // terms.
            max_body_bytes: 64 * 1024,
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
async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(req)
        .await
        .expect("istek işlenirken panik olmamalı");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("gövde okunabilmeli");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

#[allow(clippy::expect_used)]
fn json_req(method: &str, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("istek kurulabilmeli")
}

/// Same shape, but with a raw (already-serialized) body — needed here
/// because the oversized payload is built directly as bytes rather than
/// through `serde_json::Value` (a `Value` holding tens of thousands of
/// repeated characters is a needless detour).
#[allow(clippy::expect_used)]
fn raw_json_req(method: &str, uri: &str, token: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("istek kurulabilmeli")
}

#[allow(clippy::expect_used)]
async fn register(router: &Router, username: &str) -> String {
    let (status, body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/auth/register")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "username": username, "actor_type": "human", "display_name": null })
                    .to_string(),
            ))
            .expect("istek kurulabilmeli"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "kayıt başarısız oldu: {body}");
    body["api_key"].as_str().expect("api_key olmalı").to_owned()
}

// --- Testler ---------------------------------------------------------------

/// A `PATCH /actors/me` body comfortably under `max_body_bytes` (64 KiB in
/// `test_config`) must still work — this is the control case: without it, a
/// failing "too large" assertion below could just as easily mean the route
/// rejects everything, not that it correctly enforces a limit.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn siniri_asmayan_govde_kabul_edilir(pool: PgPool) {
    let router = build_router(pool);
    let api_key = register(&router, "govde_normal_boyut").await;

    let (status, body) = send(
        &router,
        json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "display_name": "Normal boyutlu isim" }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The actual gap this file closes: a JSON body bigger than
/// `max_body_bytes`, sent to a route that has no `DefaultBodyLimit`
/// override of its own, must be rejected — proving the app-wide layer
/// (`crate::app::build`) still applies to routes that don't widen it. Before
/// `b2ddb8a`'s `RequestBodyLimitLayer` → `DefaultBodyLimit` swap this was
/// enforced by a different mechanism; nothing in this repository proved
/// either mechanism kept working for a route THIS shape (JSON, no override)
/// until now.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn siniri_asan_govde_reddedilir(pool: PgPool) {
    let router = build_router(pool);
    let api_key = register(&router, "govde_asiri_boyut").await;

    // `test_config().server.max_body_bytes` is 64 KiB; this field alone is
    // already comfortably past it (well over 100 KiB once JSON-escaped and
    // wrapped), with no need to approach any other limit (`bio`'s 500-char
    // cap, say) to prove the point — the body limit is enforced before
    // field-level validation ever runs.
    let too_big = json!({ "bio": "a".repeat(200 * 1024) });
    let body_bytes = too_big.to_string().into_bytes();
    assert!(
        body_bytes.len() > test_config().server.max_body_bytes,
        "test gövdesi gerçekten sınırın üstünde olmalı: {} bytes",
        body_bytes.len()
    );

    let (status, body) = send(
        &router,
        raw_json_req("PATCH", "/actors/me", &api_key, body_bytes),
    )
    .await;

    assert!(
        status.is_client_error(),
        "sınırı aşan gövde bir 4xx ile reddedilmeli: {status} {body}"
    );
}
