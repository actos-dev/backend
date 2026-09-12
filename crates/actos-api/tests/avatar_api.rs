//! `POST`/`DELETE /actors/me/avatar` entegrasyon testleri.
//!
//! **Why this file exists, and why it's not just more cases in
//! `actors_api.rs`:** no integration test in this repo has ever sent a real
//! `multipart/form-data` body through the router. Every other test that
//! touches an "attachment"-shaped row (`actors_api.rs`, `posts_api.rs`)
//! seeds it directly with SQL and points `Storage` at an unreachable
//! address (`http://127.0.0.1:1`), because the point of those tests was
//! never the upload mechanics — it was what happens once a row already
//! exists. That's exactly the gap this file closes: it is the first test
//! suite to actually exercise `axum::extract::Multipart` end to end, which
//! is also why the app-wide 1 MiB body limit swallowing every upload over
//! that size (see REFACTOR.md §4, and `crate::app::build`'s doc for the
//! fix) went uncaught for as long as it did.
//!
//! **Storage here is REAL, not the unreachable-address pattern used
//! elsewhere.** `set_avatar`/`clear_avatar` (`actos_core::avatar`) call
//! `Storage::put_object`/`delete_object` directly in the request path — a
//! successful upload genuinely depends on that call succeeding. Pointing
//! `Storage` at `http://127.0.0.1:1` like `actors_api.rs` does would make
//! every one of these tests fail on the storage call, not on whatever
//! behavior they're meant to pin. So this file talks to the local MinIO
//! from `docker-compose.yml` (`S3_ENDPOINT`/`MINIO_BUCKET`/... from `.env`,
//! same variables `actos_core::config::StorageConfig::from_env` reads),
//! with a fallback to this repo's own `.env`/`docker-compose.yml` defaults
//! in case a contributor's shell doesn't have `.env` sourced — the
//! completion gates for this unit already `source ./.env` before running
//! `cargo test`, so in that path these fallbacks never trigger.
//!
//! Generated PNGs are used instead of a committed binary fixture — see
//! `noisy_png`'s doc for why the "1-8 MiB" case in particular needs
//! (pseudo-)random pixel data rather than a solid color.

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

// --- Kurulum yardımcıları (bkz. `tests/actors_api.rs` — aynı desen, yalnızca
// `storage` burada gerçek) --------------------------------------------------

/// Same S3 variables `actos_core::config::StorageConfig::from_env` reads —
/// this file needs a REAL, reachable storage backend (see module doc), so
/// it can't hardcode an unreachable address the way `actors_api.rs` does.
/// The fallbacks match this repo's own `.env`/`docker-compose.yml` so the
/// test also works for a contributor who forgot to source `.env` before
/// `cargo test` (the completion gates for this unit do source it).
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
        storage: test_storage_config(),
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
async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value, axum::http::HeaderMap) {
    let response = router
        .clone()
        .oneshot(req)
        .await
        .expect("istek işlenirken panik olmamalı");
    let status = response.status();
    let headers = response.headers().clone();
    // 16 MiB: bu dosyanın en büyük gövdesinden (`over_limit_png`, ~9 MiB)
    // büyük — `send` yalnızca YANITI okuyor, yanıtlar burada hep küçük JSON,
    // ama sınır isteğin kendisiyle karışmasın diye cömert tutuldu.
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("gövde okunabilmeli");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body, headers)
}

#[allow(clippy::expect_used)]
fn empty_req(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("istek kurulabilmeli")
}

#[allow(clippy::expect_used)]
fn json_req(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("istek kurulabilmeli")
}

#[allow(clippy::expect_used)]
fn auth_req(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("istek kurulabilmeli")
}

/// Hand-builds a `multipart/form-data` body with exactly one field — this
/// is the "real multipart file" the module doc talks about, not a
/// `serde_json` body. There's no multipart *client* helper anywhere in this
/// codebase to reuse (only the server-side `axum::extract::Multipart`), so
/// this constructs the wire format directly: a fixed boundary, one
/// `Content-Disposition`/`Content-Type` pair, the raw bytes, and the
/// closing boundary line.
#[allow(clippy::expect_used)]
fn multipart_avatar_req(
    token: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> Request<Body> {
    const BOUNDARY: &str = "ActosAvatarTestBoundary7331";

    let mut body = Vec::with_capacity(bytes.len() + 256);
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    Request::builder()
        .method("POST")
        .uri("/actors/me/avatar")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .expect("istek kurulabilmeli")
}

async fn register(router: &Router, username: &str) -> Value {
    let (status, body, _) = send(
        router,
        json_req(
            "POST",
            "/auth/register",
            json!({ "username": username, "actor_type": "human", "display_name": null }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "kayıt başarısız oldu: {body}");
    body
}

/// Registers a fixture actor and returns `(username, api_key)`.
#[allow(clippy::expect_used)]
async fn register_actor(router: &Router, username: &str) -> (String, String) {
    let body = register(router, username).await;
    let api_key = body["api_key"].as_str().expect("api_key olmalı").to_owned();
    (username.to_owned(), api_key)
}

// --- Görsel üretimi ----------------------------------------------------

/// A tiny, solid-color, valid PNG — enough to pass magic-byte detection and
/// decode, and small enough that its byte size is a non-issue for every
/// limit in play.
#[allow(clippy::expect_used)]
fn small_png() -> Vec<u8> {
    encode_png(&RgbImage::from_pixel(16, 16, Rgb([200, 120, 40])))
}

/// A PNG built from **pseudo-random**, not solid-color, pixel data.
///
/// This matters for the "1-8 MiB" test case specifically: PNG deflates its
/// pixel data, and a solid color compresses to almost nothing regardless of
/// the image's pixel dimensions — a 4000x4000 solid-color PNG can still be
/// under 1 KB on the wire. To land the ENCODED byte size inside a specific
/// window (here, between the app's old 1 MiB ceiling and the 8 MiB upload
/// limit) the pixel data has to be close to incompressible, so the encoded
/// size tracks the raw pixel count instead of collapsing under deflate. A
/// small xorshift32 PRNG (no external `rand` dependency needed for this)
/// is high-entropy enough for that — deflate cannot find the long,
/// low-order-bit-aligned matches it needs on this output the way it would
/// on real photographic noise, but it also finds essentially nothing to
/// compress, which is exactly the property this test needs.
#[allow(clippy::expect_used)]
fn noisy_png(width: u32, height: u32) -> Vec<u8> {
    let mut state: u32 = 0x9E37_79B9;
    let mut next_u32 = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };

    let img = RgbImage::from_fn(width, height, |_, _| {
        let v = next_u32();
        Rgb([
            (v & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            ((v >> 16) & 0xFF) as u8,
        ])
    });
    encode_png(&img)
}

#[allow(clippy::expect_used)]
fn encode_png(img: &RgbImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(img.clone())
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("PNG encode edilebilmeli");
    bytes
}

const ONE_MIB: usize = 1024 * 1024;
const EIGHT_MIB: usize = 8 * 1024 * 1024;

/// Extracts the object key `Storage::public_url` embedded in a returned
/// avatar URL — the inverse of that method, needed here to check the
/// object directly in storage (see [`object_exists`]).
#[allow(clippy::expect_used)]
fn object_key_from_url(url: &str, public_base_url: &str) -> String {
    url.strip_prefix(&format!("{public_base_url}/"))
        .expect("avatar_url beklenen public_base_url ile başlamalı")
        .to_owned()
}

/// Checks the object's real presence in MinIO via a `HEAD` — this is the
/// one place in this file that looks past the HTTP response to confirm
/// `set_avatar`/`clear_avatar` (`actos_core::avatar`) actually did what the
/// response claims: stored the new object, and (for the replace test)
/// genuinely deleted the old one rather than just overwriting the
/// database's pointer to it.
async fn object_exists(storage: &Storage, key: &str) -> bool {
    storage
        .client()
        .head_object()
        .bucket(storage.bucket())
        .key(key)
        .send()
        .await
        .is_ok()
}

// --- POST /actors/me/avatar ----------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kucuk_gecerli_gorsel_yuklenir_avatar_url_doner(pool: PgPool) {
    let router = build_router(pool);
    let (_, api_key) = register_actor(&router, "avatar_kucuk_gorsel").await;

    let (status, body, _) = send(
        &router,
        multipart_avatar_req(&api_key, "avatar.png", "image/png", &small_png()),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    let avatar_url = body["avatar_url"]
        .as_str()
        .expect("avatar_url string olmalı: {body}");
    assert!(
        avatar_url.starts_with(&test_config().storage.public_base_url),
        "beklenmeyen avatar_url: {avatar_url}"
    );

    // `GET /actors/{username}` de aynı `avatar_url`'i taşımalı — `POST`
    // yanıtına özel bir kısayol değil, `actors.avatar_object_key` gerçekten
    // yazıldı.
    let (status, profile_body, _) =
        send(&router, empty_req("GET", "/actors/avatar_kucuk_gorsel")).await;
    assert_eq!(status, StatusCode::OK, "{profile_body}");
    assert_eq!(profile_body["actor"]["avatar_url"], avatar_url);
}

/// **The exact case that fails without this unit's body-limit fix**: a file
/// bigger than the app-wide `max_body_bytes` (1 MiB) but within
/// `max_upload_bytes` (8 MiB). Before the avatar route's own
/// `DefaultBodyLimit` override, this request never reached
/// `actos_core::media::process_image` at all — the app-wide limit rejected
/// it first.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bir_ile_sekiz_mib_arasi_dosya_basariyla_yuklenir(pool: PgPool) {
    let router = build_router(pool);
    let (_, api_key) = register_actor(&router, "avatar_orta_boy").await;

    let png = noisy_png(1000, 1000);
    assert!(
        png.len() > ONE_MIB && png.len() < EIGHT_MIB,
        "test görseli beklenen aralıkta değil: {} bytes",
        png.len()
    );

    let (status, body, _) = send(
        &router,
        multipart_avatar_req(&api_key, "avatar.png", "image/png", &png),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["avatar_url"].as_str().is_some(), "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sekiz_mib_ustu_dosya_reddedilir(pool: PgPool) {
    let router = build_router(pool);
    let (_, api_key) = register_actor(&router, "avatar_cok_buyuk").await;

    // Gerçek bir görsel olması gerekmiyor — `DefaultBodyLimit` gövdeyi
    // `max_upload_bytes`'ı aşar aşmaz akışı keser, `actos_core::media::
    // process_image`'a hiç ulaşılmadan. Sabit bir bayt deseni bunun için
    // yeterli ve gerçek bir görsel üretmekten çok daha ucuz.
    let too_big = vec![0xABu8; EIGHT_MIB + ONE_MIB];

    let (status, body, _) = send(
        &router,
        multipart_avatar_req(&api_key, "avatar.png", "image/png", &too_big),
    )
    .await;

    assert!(
        status.is_client_error(),
        "çok büyük dosya bir 4xx ile reddedilmeli: {status} {body}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gorsel_olmayan_dosya_uzantisi_gorsel_olsa_bile_reddedilir(pool: PgPool) {
    let router = build_router(pool);
    let (_, api_key) = register_actor(&router, "avatar_sahte_uzanti").await;

    // Uzantı ve `Content-Type` "görsel" diyor ama baytlar düz metin —
    // `actos_core::media::process_image`'ın magic-byte tespiti (`infer`)
    // ikisine de değil, gerçek içeriğe bakıyor.
    let fake = b"this is definitely not an image, just plain text pretending to be one";

    let (status, body, _) = send(
        &router,
        multipart_avatar_req(&api_key, "avatar.png", "image/png", fake),
    )
    .await;

    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    assert_eq!(body["code"], "UNSUPPORTED_MEDIA");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn avatar_degistirmek_eski_nesneyi_siler(pool: PgPool) {
    let router = build_router(pool);
    let (_, api_key) = register_actor(&router, "avatar_degistiren").await;
    let storage = Storage::new(&test_config().storage);
    let public_base_url = test_config().storage.public_base_url;

    let (status, body, _) = send(
        &router,
        multipart_avatar_req(&api_key, "ilk.png", "image/png", &small_png()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let first_url = body["avatar_url"].as_str().expect("{body}").to_owned();
    let first_key = object_key_from_url(&first_url, &public_base_url);
    assert!(
        object_exists(&storage, &first_key).await,
        "ilk avatar nesnesi depolamada olmalı"
    );

    // İkinci bir görselle değiştir — farklı piksel verisi, farklı (rastgele
    // üretilen) `object_key`, bkz. `actos_core::attachment::
    // object_key_uret`.
    let (status, body, _) = send(
        &router,
        multipart_avatar_req(&api_key, "ikinci.png", "image/png", &noisy_png(32, 32)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let second_url = body["avatar_url"].as_str().expect("{body}").to_owned();
    let second_key = object_key_from_url(&second_url, &public_base_url);
    assert_ne!(
        first_key, second_key,
        "yeni yükleme farklı bir anahtar almalı"
    );

    assert!(
        object_exists(&storage, &second_key).await,
        "yeni avatar nesnesi depolamada olmalı"
    );
    assert!(
        !object_exists(&storage, &first_key).await,
        "eski avatar nesnesi silinmiş olmalı — bkz. actos_core::avatar::set_avatar"
    );
}

// --- DELETE /actors/me/avatar ----------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn delete_avatari_temizler(pool: PgPool) {
    let router = build_router(pool);
    let (username, api_key) = register_actor(&router, "avatar_silinen").await;
    let storage = Storage::new(&test_config().storage);
    let public_base_url = test_config().storage.public_base_url;

    let (status, body, _) = send(
        &router,
        multipart_avatar_req(&api_key, "avatar.png", "image/png", &small_png()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let avatar_url = body["avatar_url"].as_str().expect("{body}").to_owned();
    let key = object_key_from_url(&avatar_url, &public_base_url);
    assert!(object_exists(&storage, &key).await);

    let (status, body, _) = send(&router, auth_req("DELETE", "/actors/me/avatar", &api_key)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, profile_body, _) =
        send(&router, empty_req("GET", &format!("/actors/{username}"))).await;
    assert_eq!(status, StatusCode::OK, "{profile_body}");
    assert!(
        profile_body["actor"]["avatar_url"].is_null(),
        "silindikten sonra avatar_url null olmalı: {profile_body}"
    );
    assert!(
        !object_exists(&storage, &key).await,
        "silindikten sonra nesne depolamada kalmamalı"
    );

    // Tekrar `DELETE`: idempotent, hâlâ `204` — bkz.
    // `actos_core::avatar::clear_avatar` dokümanı.
    let (status, body, _) = send(&router, auth_req("DELETE", "/actors/me/avatar", &api_key)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}
