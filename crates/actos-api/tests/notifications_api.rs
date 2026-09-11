//! `crates/actos-api/routes/notifications.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/interactions_api.rs` ile aynı desen — ayrı
//! bir entegrasyon test binary'si olduğu için (Rust her `tests/*.rs`
//! dosyasını bağımsız derler) paylaşılan bir modül olmadan tekrar
//! tanımlanıyor.
//!
//! Fan-out kuralının kendisi (kök yazarı + doğrudan ebeveyn, kendine
//! bildirim yok, idempotent okundu işaretleme) burada değil,
//! `crates/actos-core/tests/notification.rs`'te — domain davranışı, HTTP'ye
//! ihtiyaç duymuyor. Buradaki testler HTTP katmanına özgü: durum kodları,
//! kimlik zorunluluğu, `?unread=` ayrıştırması ve cursor'ın query'den doğru
//! taşınması.

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    auth::{self as core_auth, ActorType},
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

// --- Kurulum yardımcıları (bkz. `tests/interactions_api.rs` — aynı desen) -

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
            orphan_cleanup_interval: std::time::Duration::ZERO,
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
async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value, axum::http::HeaderMap) {
    let response = router
        .clone()
        .oneshot(req)
        .await
        .expect("istek işlenirken panik olmamalı");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
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
fn auth_json_req(method: &str, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
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

#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> (i64, String) {
    let reg = core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    (reg.actor.id, reg.api_key)
}

/// Bir post oluşturup dış id'sini döner.
#[allow(clippy::expect_used)]
async fn seed_post(router: &Router, api_key: &str, title: &str) -> String {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            "/posts",
            api_key,
            json!({ "title": title, "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post oluşturulamadı: {body}");
    body["id"].as_str().expect("post id").to_owned()
}

/// `parent_id` verilirse (`c_...`) o yoruma yanıt; verilmezse post'a doğrudan
/// yorum. Dış yorum id'sini döner.
#[allow(clippy::expect_used)]
async fn seed_comment(
    router: &Router,
    api_key: &str,
    post: &str,
    parent_id: Option<&str>,
) -> String {
    let mut gövde = json!({ "body": "yorum gövdesi" });
    if let Some(parent) = parent_id {
        gövde["parent_id"] = json!(parent);
    }
    let (status, body, _) = send(
        router,
        auth_json_req("POST", &format!("/posts/{post}/comments"), api_key, gövde),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "yorum oluşturulamadı: {body}");
    body["id"].as_str().expect("yorum id").to_owned()
}

// --- GET /me/inbox -----------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kimliksiz_inbox_401_doner(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/me/inbox")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yorum_gelince_post_yazarinin_inboxunda_gorunur(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_inbox_yazar").await;
    let (_, yorumcu_key) = seed_actor(&raw_pool, "http_inbox_yorumcu").await;
    let post = seed_post(&router, &yazar_key, "post").await;

    let yorum = seed_comment(&router, &yorumcu_key, &post, None).await;

    let (status, body, _) = send(&router, auth_req("GET", "/me/inbox", &yazar_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["unread_count"], 1, "{body}");
    let notifications = body["notifications"].as_array().expect("dizi");
    assert_eq!(notifications.len(), 1, "{body}");
    assert_eq!(notifications[0]["kind"], "comment_on_post", "{body}");
    assert!(
        notifications[0]["id"].as_str().unwrap().starts_with("n_"),
        "bildirim id'si n_ öneki taşımalı: {body}"
    );
    assert_eq!(
        notifications[0]["target_id"], yorum,
        "target_id yorumun eklendiği postu değil YENİ YORUMU işaret etmeli"
    );
    assert_eq!(
        notifications[0]["target_type"], "content",
        "post ve yorum aynı ID uzayını paylaşıyor; ayrımı kind alanı yapıyor: {body}"
    );
}

/// Kendi yorumcunun kutusu boş kalmalı — kimse ona bildirim üretmedi.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yorum_yazan_kendi_inboxunda_bildirim_gormez(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_inbox_kendi_yazar").await;
    let (_, yorumcu_key) = seed_actor(&raw_pool, "http_inbox_kendi_yorumcu").await;
    let post = seed_post(&router, &yazar_key, "post").await;

    seed_comment(&router, &yorumcu_key, &post, None).await;

    let (status, body, _) = send(&router, auth_req("GET", "/me/inbox", &yorumcu_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["unread_count"], 0, "{body}");
    assert_eq!(body["notifications"].as_array().unwrap().len(), 0, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn unread_true_yalniz_okunmamislari_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_unread_yazar").await;
    let (_, a_key) = seed_actor(&raw_pool, "http_unread_a").await;
    let (_, b_key) = seed_actor(&raw_pool, "http_unread_b").await;
    let post = seed_post(&router, &yazar_key, "post").await;

    seed_comment(&router, &a_key, &post, None).await;
    seed_comment(&router, &b_key, &post, None).await;

    let (_, body, _) = send(&router, auth_req("GET", "/me/inbox", &yazar_key)).await;
    let notifications = body["notifications"].as_array().expect("dizi");
    assert_eq!(notifications.len(), 2, "{body}");
    let ilk_id = notifications[1]["id"].as_str().expect("id").to_owned(); // en eski

    let (status, _, _) = send(
        &router,
        empty_req("PATCH", &format!("/me/inbox/{ilk_id}/read")),
    )
    .await;
    // Kimliksiz PATCH -> 401, sadece yardımcı istek biçimini doğruluyoruz;
    // asıl işaretleme aşağıda kimlikli yapılıyor.
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = send(
        &router,
        auth_req("PATCH", &format!("/me/inbox/{ilk_id}/read"), &yazar_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/me/inbox?unread=true", &yazar_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let unread = body["notifications"].as_array().expect("dizi");
    assert_eq!(unread.len(), 1, "{body}");
    assert_eq!(body["unread_count"], 1, "{body}");
}

// --- PATCH /me/inbox/{id}/read ------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tekil_okundu_isaretleme_idempotent_http(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_tekil_yazar").await;
    let (_, yorumcu_key) = seed_actor(&raw_pool, "http_tekil_yorumcu").await;
    let post = seed_post(&router, &yazar_key, "post").await;
    seed_comment(&router, &yorumcu_key, &post, None).await;

    let (_, body, _) = send(&router, auth_req("GET", "/me/inbox", &yazar_key)).await;
    let notif_id = body["notifications"][0]["id"]
        .as_str()
        .expect("id")
        .to_owned();

    for _ in 0..2 {
        let (status, body, _) = send(
            &router,
            auth_req("PATCH", &format!("/me/inbox/{notif_id}/read"), &yazar_key),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    let (_, body, _) = send(&router, auth_req("GET", "/me/inbox", &yazar_key)).await;
    assert_eq!(body["unread_count"], 0, "{body}");
    assert!(body["notifications"][0]["read_at"].is_string(), "{body}");
}

/// Başka bir actor'ün bildirimini okundu işaretlemeye çalışmak `404` dönmeli.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn baskasinin_bildirimi_404_doner_http(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_baskasi_yazar").await;
    let (_, yorumcu_key) = seed_actor(&raw_pool, "http_baskasi_yorumcu").await;
    let (_, yabanci_key) = seed_actor(&raw_pool, "http_baskasi_yabanci").await;
    let post = seed_post(&router, &yazar_key, "post").await;
    seed_comment(&router, &yorumcu_key, &post, None).await;

    let (_, body, _) = send(&router, auth_req("GET", "/me/inbox", &yazar_key)).await;
    let notif_id = body["notifications"][0]["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let (status, body, _) = send(
        &router,
        auth_req("PATCH", &format!("/me/inbox/{notif_id}/read"), &yabanci_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

// --- POST /me/inbox/read (toplu) ----------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn toplu_okundu_isaretleme_gövdesiz_post_hepsini_isaretler(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_toplu_yazar").await;
    let (_, a_key) = seed_actor(&raw_pool, "http_toplu_a").await;
    let (_, b_key) = seed_actor(&raw_pool, "http_toplu_b").await;
    let post = seed_post(&router, &yazar_key, "post").await;
    seed_comment(&router, &a_key, &post, None).await;
    seed_comment(&router, &b_key, &post, None).await;

    let (_, body, _) = send(&router, auth_req("GET", "/me/inbox", &yazar_key)).await;
    assert_eq!(body["unread_count"], 2, "{body}");

    // Gövdesiz, cursor'suz `POST` — tüm okunmamışları işaretlemeli.
    let (status, body, _) = send(&router, auth_req("POST", "/me/inbox/read", &yazar_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["marked"], 2, "{body}");

    let (_, body, _) = send(&router, auth_req("GET", "/me/inbox", &yazar_key)).await;
    assert_eq!(body["unread_count"], 0, "{body}");

    // İkinci çağrı (idempotent) yeni bir şey bulamamalı.
    let (status, body, _) = send(&router, auth_req("POST", "/me/inbox/read", &yazar_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["marked"], 0, "{body}");
}

// --- Cursor tutarlılığı ---------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn inbox_cursor_sayfalari_tekrarsiz_geziyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_cursor_yazar").await;
    let (_, a_key) = seed_actor(&raw_pool, "http_cursor_a").await;
    let (_, b_key) = seed_actor(&raw_pool, "http_cursor_b").await;
    let post = seed_post(&router, &yazar_key, "post").await;
    seed_comment(&router, &a_key, &post, None).await;
    seed_comment(&router, &b_key, &post, None).await;

    let (status, ilk_sayfa, _) =
        send(&router, auth_req("GET", "/me/inbox?limit=1", &yazar_key)).await;
    assert_eq!(status, StatusCode::OK, "{ilk_sayfa}");
    let ilk_öğeler = ilk_sayfa["notifications"].as_array().expect("dizi");
    assert_eq!(ilk_öğeler.len(), 1, "{ilk_sayfa}");
    let next_cursor = ilk_sayfa["next_cursor"]
        .as_str()
        .expect("ikinci sayfa olmalı");

    let (status, ikinci_sayfa, _) = send(
        &router,
        auth_req(
            "GET",
            &format!("/me/inbox?limit=1&cursor={next_cursor}"),
            &yazar_key,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ikinci_sayfa}");
    let ikinci_öğeler = ikinci_sayfa["notifications"].as_array().expect("dizi");
    assert_eq!(ikinci_öğeler.len(), 1, "{ikinci_sayfa}");

    assert_ne!(
        ilk_öğeler[0]["id"], ikinci_öğeler[0]["id"],
        "iki sayfa aynı bildirimi tekrar döndürmemeli"
    );
    assert!(
        ikinci_sayfa["next_cursor"].is_null(),
        "iki bildirimin tamamı gezildikten sonra üçüncü sayfa olmamalı: {ikinci_sayfa}"
    );
}
