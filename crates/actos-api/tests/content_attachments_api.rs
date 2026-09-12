//! `POST /posts`/`POST /posts/{id}/comments` multipart (image-carrying)
//! integration tests — REFACTOR.md §4: there is no standalone upload any
//! more, an image travels with the post or comment that carries it, in the
//! same `multipart/form-data` request, as a `payload` JSON part plus up to
//! 4 `files` parts.
//!
//! **Storage here is REAL, not the unreachable-address pattern used in
//! `posts_api.rs`/`comments_api.rs`.** `attachment::create_for_content`
//! (`actos_core::attachment`) calls `Storage::put_object` directly inside
//! the same transaction as the post/comment row — a successful multipart
//! creation genuinely depends on that call succeeding, and the quota test
//! below needs a real accumulated byte size to check a real limit against.
//! So, like `avatar_api.rs`, this file talks to the local MinIO from
//! `docker-compose.yml`, with the same `.env`-sourced-with-fallback pattern.
//!
//! Kurulum yardımcıları `tests/avatar_api.rs` ile aynı desen (gerçek
//! depolama, üretilen PNG'ler) + `tests/posts_api.rs` ile aynı desen (JSON
//! istek kurucuları, `register`) — ayrı bir entegrasyon test binary'si
//! olduğu için ikisi de burada tekrar tanımlanıyor.

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
use image::{ImageFormat, Rgb, RgbImage};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt as _;

// --- Kurulum yardımcıları (bkz. `tests/avatar_api.rs`) --------------------

/// Same S3 variables `actos_core::config::StorageConfig::from_env` reads —
/// see `avatar_api.rs`'s identical helper for the full rationale (a REAL,
/// reachable storage backend is required here).
#[allow(clippy::expect_used)]
fn test_storage_config() -> StorageConfig {
    fn var(name: &str, default: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| default.to_owned())
    }

    StorageConfig {
        endpoint: var("S3_ENDPOINT", "http://127.0.0.1:3103"),
        region: var("S3_REGION", "us-east-1"),
        bucket: var("MINIO_BUCKET", "actos-media"),
        access_key: var("S3_ACCESS_KEY", "actos_minio"),
        secret_key: var("S3_SECRET_KEY", "actos_minio_dev_password"),
        public_base_url: var("S3_PUBLIC_BASE_URL", "http://127.0.0.1:3103/actos-media"),
    }
}

/// Like `posts_api.rs`'s `test_config`, but with a caller-supplied storage
/// quota — the quota test below needs a much smaller, exact number than the
/// production default (500 MB) to check a real limit without uploading
/// hundreds of megabytes.
#[allow(clippy::expect_used)]
fn test_config_with_quota(quota_bytes: i64) -> Config {
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
        storage: test_storage_config(),
        security: SecurityConfig {
            id_obfuscation_key: "test-id-obfuscation-key-en-az-otuz-iki-karakter".to_owned(),
            cursor_signing_key: "test-cursor-signing-key-en-az-otuz-iki-karakter".to_owned(),
        },
        rate_limits: LimitTable::from_env().expect("varsayılan limit tablosu geçerli olmalı"),
        storage_quota: StorageQuotaConfig { bytes: quota_bytes },
    }
}

/// 500 MB — the production default (`StorageQuotaConfig::from_env`'s own
/// fallback), comfortably above anything this file's non-quota tests
/// upload.
#[allow(clippy::expect_used)]
fn test_config() -> Config {
    test_config_with_quota(500 * 1024 * 1024)
}

#[allow(clippy::expect_used)]
fn build_router_with_config(pool: PgPool, config: Config) -> Router {
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

    let state = actos_api::state::AppState::new(
        config,
        pool,
        redis,
        storage,
        id_codec,
        cursor_codec,
        rate_limiter,
        idempotency,
    );
    actos_api::app::build(state)
}

#[allow(clippy::expect_used)]
fn build_router(pool: PgPool) -> Router {
    build_router_with_config(pool, test_config())
}

#[allow(clippy::expect_used)]
async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(req)
        .await
        .expect("istek işlenirken panik olmamalı");
    let status = response.status();
    // 64 MiB: comfortably above this file's largest response body (a post
    // with 4 attachments) — `send` only reads the RESPONSE, requests are
    // built separately and are the ones that actually approach the size
    // limits under test.
    let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024)
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
fn plain_json_req(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("istek kurulabilmeli")
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

/// Hand-builds a `multipart/form-data` body with one `payload` JSON part
/// plus zero or more `files` parts — the exact shape
/// `crate::routes::posts::extract_content_payload` parses. There is no
/// multipart *client* helper anywhere in this codebase to reuse (only the
/// server-side `axum::extract::Multipart`), same situation `avatar_api.rs`
/// is in.
#[allow(clippy::expect_used)]
fn multipart_content_req(
    method: &str,
    uri: &str,
    token: &str,
    payload: &Value,
    files: &[Vec<u8>],
) -> Request<Body> {
    const BOUNDARY: &str = "ActosContentAttachmentsTestBoundary9911";

    let mut body = Vec::new();

    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"payload\"\r\n");
    body.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
    body.extend_from_slice(payload.to_string().as_bytes());
    body.extend_from_slice(b"\r\n");

    for (i, file) in files.iter().enumerate() {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"files\"; filename=\"{i}.png\"\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
        body.extend_from_slice(file);
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .expect("istek kurulabilmeli")
}

#[allow(clippy::expect_used)]
async fn register(router: &Router, username: &str) -> String {
    let (status, body) = send(
        router,
        plain_json_req(
            "POST",
            "/auth/register",
            json!({ "username": username, "actor_type": "human", "display_name": null }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "kayıt başarısız oldu: {body}");
    body["api_key"].as_str().expect("api_key olmalı").to_owned()
}

// --- Görsel üretimi (bkz. `avatar_api.rs`) --------------------------------

/// A tiny, solid-color, valid PNG.
#[allow(clippy::expect_used)]
fn small_png() -> Vec<u8> {
    encode_png(&RgbImage::from_pixel(16, 16, Rgb([200, 120, 40])))
}

/// A distinctly different tiny PNG for tests that upload several files —
/// different dimensions AND a different color per `seed`, so each call
/// produces an image with a different encoded (and therefore normalized)
/// byte size and a different pixel content.
#[allow(clippy::expect_used)]
fn small_png_variant(seed: u8) -> Vec<u8> {
    let size = 16 + u32::from(seed);
    encode_png(&RgbImage::from_pixel(
        size,
        size,
        Rgb([40, 120, 200_u8.wrapping_add(seed)]),
    ))
}

#[allow(clippy::expect_used)]
fn encode_png(img: &RgbImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(img.clone())
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("PNG encode edilebilmeli");
    bytes
}

/// Not an image at all — used by the "failing file aborts everything" test.
fn not_an_image() -> Vec<u8> {
    b"this is definitely not an image, just plain text pretending to be one".to_vec()
}

/// Total number of live posts by an actor — used to prove a rejected
/// multipart creation wrote nothing at all, content row included.
#[allow(clippy::expect_used)]
async fn post_count(router: &Router, username: &str) -> usize {
    let (status, body) = send(
        router,
        Request::builder()
            .method("GET")
            .uri(format!("/actors/{username}/posts"))
            .body(Body::empty())
            .expect("istek kurulabilmeli"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["posts"]
        .as_array()
        .expect("posts bir dizi olmalı")
        .len()
}

// --- POST /posts: JSON (no images) — must keep working unchanged ---------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn json_ile_olusturulan_post_gorselsiz_calismaya_devam_eder(pool: PgPool) {
    let router = build_router(pool);
    let api_key = register(&router, "ca_json_post").await;

    let (status, body) = send(
        &router,
        json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "Düz metin post", "body": "Hiç görsel yok.", "tags": [] }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["attachments"],
        json!([]),
        "görselsiz bir post boş bir attachments dizisi taşımalı: {body}"
    );
}

// --- POST /posts: multipart ------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn multipart_tek_gorselli_post_olusturulur(pool: PgPool) {
    let router = build_router(pool);
    let api_key = register(&router, "ca_bir_gorsel").await;

    let (status, body) = send(
        &router,
        multipart_content_req(
            "POST",
            "/posts",
            &api_key,
            &json!({ "title": "Tek görselli post", "body": "Bir resim ekliyorum.", "tags": [] }),
            &[small_png()],
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    let attachments = body["attachments"].as_array().expect("{body}");
    assert_eq!(attachments.len(), 1, "{body}");
    assert!(
        attachments[0]["url"]
            .as_str()
            .is_some_and(|u| !u.is_empty()),
        "{body}"
    );
    assert!(attachments[0]["thumbnail_url"].as_str().is_some(), "{body}");
    assert_eq!(attachments[0]["mime_type"], "image/webp", "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn multipart_dort_gorselli_post_olusturulur(pool: PgPool) {
    let router = build_router(pool);
    let api_key = register(&router, "ca_dort_gorsel").await;

    let files: Vec<Vec<u8>> = (0..4).map(small_png_variant).collect();

    let (status, body) = send(
        &router,
        multipart_content_req(
            "POST",
            "/posts",
            &api_key,
            &json!({ "title": "Dört görselli post", "body": "Dört resim ekliyorum.", "tags": [] }),
            &files,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    let attachments = body["attachments"].as_array().expect("{body}");
    assert_eq!(attachments.len(), 4, "{body}");

    // Her ek farklı bir object key almalı — bkz. `attachment::object_key_uret`.
    let urls: std::collections::BTreeSet<&str> = attachments
        .iter()
        .map(|a| a["url"].as_str().expect("{body}"))
        .collect();
    assert_eq!(urls.len(), 4, "her ek farklı bir url almalı: {body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bes_gorsel_reddedilir(pool: PgPool) {
    let router = build_router(pool);
    let username = "ca_bes_gorsel";
    let api_key = register(&router, username).await;

    let files: Vec<Vec<u8>> = (0..5).map(small_png_variant).collect();

    let (status, body) = send(
        &router,
        multipart_content_req(
            "POST",
            "/posts",
            &api_key,
            &json!({ "title": "Beş görselli post", "body": "Bu reddedilmeli.", "tags": [] }),
            &files,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
    assert_eq!(
        post_count(&router, username).await,
        0,
        "reddedilen istek hiçbir post yazmamalı"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn basarisiz_dosya_tum_olusturmayi_iptal_eder(pool: PgPool) {
    let router = build_router(pool);
    let username = "ca_basarisiz_dosya";
    let api_key = register(&router, username).await;

    // İlk dosya geçerli, ikincisi değil — `attachment::create_for_content`
    // TÜM dosyaları depolamaya hiç dokunmadan önce doğruluyor (bkz. o
    // fonksiyonun "All-or-nothing" bölümü), yani geçerli dosya bile
    // yüklenmemiş olmalı.
    let files = vec![small_png(), not_an_image()];

    let (status, body) = send(
        &router,
        multipart_content_req(
            "POST",
            "/posts",
            &api_key,
            &json!({ "title": "Kısmen bozuk post", "body": "Bu hiç oluşmamalı.", "tags": [] }),
            &files,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    assert_eq!(body["code"], "UNSUPPORTED_MEDIA", "{body}");
    assert_eq!(
        post_count(&router, username).await,
        0,
        "başarısız bir dosya, geçerli dosyalar dahil, içerik satırının kendisini de iptal etmeli"
    );
}

// --- POST /posts/{id}/comments: multipart ----------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yorum_gorselle_olusturulur(pool: PgPool) {
    let router = build_router(pool);
    let api_key = register(&router, "ca_yorum_gorsel").await;

    let (status, body) = send(
        &router,
        json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "Yorum alacak post", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let post_id = body["id"].as_str().expect("{body}").to_owned();

    let (status, body) = send(
        &router,
        multipart_content_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            &api_key,
            &json!({ "body": "Görsel taşıyan bir yorum." }),
            &[small_png()],
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    let attachments = body["attachments"].as_array().expect("{body}");
    assert_eq!(attachments.len(), 1, "{body}");
}

// --- Depolama kotası --------------------------------------------------------

/// The quota sits between one small image's normalized size and four —
/// see the comment at its use site for the exact numbers.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kota_asimi_yazmayi_reddeder(pool: PgPool) {
    // A quota generous enough for exactly one post's worth of one small
    // image (a 16x16 solid-color PNG normalizes to a 160-byte WebP,
    // measured empirically), but not for a second post's THREE more on top
    // of it (3 × 160 = 480) — 300 sits strictly between the two.
    let quota_bytes = 300;
    let router = build_router_with_config(pool, test_config_with_quota(quota_bytes));
    let username = "ca_kota_asan";
    let api_key = register(&router, username).await;

    let (status, first_body) = send(
        &router,
        multipart_content_req(
            "POST",
            "/posts",
            &api_key,
            &json!({ "title": "İlk post", "body": "gövde", "tags": [] }),
            &[small_png()],
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "kotanın altındaki ilk yükleme kabul edilmeli: {first_body}"
    );

    let (status, second_body) = send(
        &router,
        multipart_content_req(
            "POST",
            "/posts",
            &api_key,
            &json!({ "title": "İkinci post", "body": "gövde", "tags": [] }),
            &[small_png(), small_png_variant(1), small_png_variant(2)],
        ),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "kotayı aşan yazma reddedilmeli: {second_body}"
    );
    assert_eq!(second_body["code"], "VALIDATION_FAILED", "{second_body}");
    assert!(
        second_body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("quota")),
        "{second_body}"
    );
    assert_eq!(
        post_count(&router, username).await,
        1,
        "kotayı aşan ikinci istek hiçbir şey yazmamalı, ilk post duruyor olmalı"
    );
}
