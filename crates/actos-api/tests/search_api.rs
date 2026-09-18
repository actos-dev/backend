//! `crates/actos-api/routes/search.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/tags_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! Domain katmanının (sıralama formülü, `unaccent` davranışı, cursor
//! mekanizması) daha ayrıntılı testleri `crates/actos-core/tests/search.rs`'te;
//! burada yalnızca HTTP sözleşmesi (uç, query parametreleri, `?fields=`,
//! hata kodları) sınanıyor.

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

// --- Kurulum yardımcıları (bkz. `tests/tags_api.rs` — aynı desen) --------

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

/// `/auth/register`'ın hız sınırını görmeden doğrudan domain katmanından
/// bir actor oluşturur, döner: `(actor_id, api_key)`.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> (i64, String) {
    let reg = core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    (reg.actor.id, reg.api_key)
}

/// Bir post oluşturup dış id'sini döner.
#[allow(clippy::expect_used)]
async fn seed_post(router: &Router, api_key: &str, title: &str, body: &str) -> String {
    let (status, resp, _) = send(
        router,
        auth_json_req(
            "POST",
            "/posts",
            api_key,
            json!({ "title": title, "body": body }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post oluşturulamadı: {resp}");
    resp["id"].as_str().expect("post id").to_owned()
}

/// Verilen post'a bir yorum ekler, dış id'sini döner.
#[allow(clippy::expect_used)]
async fn seed_comment(router: &Router, api_key: &str, post_id: &str, body: &str) -> String {
    let (status, resp, _) = send(
        router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            api_key,
            json!({ "body": body }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "yorum oluşturulamadı: {resp}");
    resp["id"].as_str().expect("comment id").to_owned()
}

async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
    let (status, body, _) = send(router, empty_req("GET", uri)).await;
    (status, body)
}

/// Yanıttaki `results` dizisinin `id` alanlarını sırasıyla döner.
#[allow(clippy::expect_used)]
fn result_ids(body: &Value) -> Vec<&str> {
    body["results"]
        .as_array()
        .expect("results dizi olmalı")
        .iter()
        .map(|r| r["id"].as_str().expect("id string olmalı"))
        .collect()
}

// --- `type` doğrulaması ------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn type_verilmezse_400(pool: PgPool) {
    let router = build_router(pool);
    let (status, body) = get_json(&router, "/search?q=nvidia").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gecersiz_type_400(pool: PgPool) {
    let router = build_router(pool);
    let (status, body) = get_json(&router, "/search?q=nvidia&type=video").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

// --- `type=post` / `type=comment` / `type=actor` ---------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn type_post_yalnizca_postlari_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "aramaci_bir").await;

    let post_id = seed_post(&router, &api_key, "roket bilimi", "gövde").await;
    let other_post = seed_post(&router, &api_key, "ilgisiz", "ilgisiz gövde").await;
    seed_comment(&router, &api_key, &other_post, "roket ile ilgili yorum").await;

    let (status, body) = get_json(&router, "/search?q=roket&type=post").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(result_ids(&body), vec![post_id.as_str()], "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn type_comment_yalnizca_yorumlari_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "aramaci_iki").await;

    let post_id = seed_post(&router, &api_key, "roket bilimi", "gövde").await;
    let comment_id = seed_comment(&router, &api_key, &post_id, "roket ile ilgili yorum").await;

    let (status, body) = get_json(&router, "/search?q=roket&type=comment").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(result_ids(&body), vec![comment_id.as_str()], "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn type_actor_yalnizca_actorleri_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    // Sorgu kelimesinden (`roketci`) trigram'la çakışmasın diye alakasız bir
    // yazar adı (bkz. `crates/actos-core/tests/search.rs`'teki aynı gerekçe).
    let (_, api_key) = seed_actor(&raw_pool, "baskayazar").await;
    seed_actor(&raw_pool, "roketci_kisi").await;

    seed_post(&router, &api_key, "roketci hakkında", "gövde").await;

    let (status, body) = get_json(&router, "/search?q=roketci&type=actor").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sonuclar = body["results"].as_array().expect("dizi");
    assert_eq!(sonuclar.len(), 1, "{body}");
    assert_eq!(sonuclar[0]["username"], "roketci_kisi", "{body}");
}

// --- Türkçe aksan-duyarsızlığı --------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn turkce_aksan_duyarsizligi_http_uzerinden(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "aksanhttp").await;

    let aksanli = seed_post(&router, &api_key, "yeni sürücü çıktı", "gövde").await;

    let (status, body) = get_json(&router, "/search?q=surucu&type=post").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(result_ids(&body), vec![aksanli.as_str()], "{body}");
}

// --- Silinmiş içerik --------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_post_aramada_cikmiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "silmehttp").await;

    let post_id = seed_post(&router, &api_key, "silinecek arama konusu", "gövde").await;
    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{post_id}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = get_json(&router, "/search?q=silinecek&type=post").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(result_ids(&body).is_empty(), "{body}");
}

// --- Cursor'lu sayfalama ----------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn cursorlu_sayfalama_tekrar_atlama_yok(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "sayfalamahttp").await;

    let mut beklenen = Vec::new();
    for i in 0..3 {
        beklenen
            .push(seed_post(&router, &api_key, &format!("sayfalama konusu {i}"), "gövde").await);
    }
    beklenen.sort();

    let mut gorulen: Vec<String> = Vec::new();
    let mut uri = "/search?q=sayfalama&type=post&limit=1".to_owned();
    loop {
        let (status, body) = get_json(&router, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let sonuclar = body["results"].as_array().expect("dizi");
        assert_eq!(sonuclar.len(), 1, "{body}");
        gorulen.push(sonuclar[0]["id"].as_str().expect("id").to_owned());

        match body["next_cursor"].as_str() {
            Some(cursor) => {
                uri = format!("/search?q=sayfalama&type=post&limit=1&cursor={cursor}");
            }
            None => break,
        }
        assert!(gorulen.len() <= 3, "beklenenden fazla sayfa döndü");
    }

    gorulen.sort();
    assert_eq!(
        gorulen, beklenen,
        "sayfalar arasında tekrar eden ya da atlanan kayıt var"
    );
}

// --- Boş `q` -------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bos_q_bos_liste_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "bosaramahttp").await;
    seed_post(&router, &api_key, "herhangi bir başlık", "gövde").await;

    let (status, body) = get_json(&router, "/search?type=post").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(result_ids(&body).is_empty(), "{body}");
}

// --- `?fields=` -----------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn fields_ile_filtreleniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "alanlihttp").await;
    seed_post(&router, &api_key, "alanli arama konusu", "gövde").await;

    let (status, body) = get_json(&router, "/search?q=alanli&type=post&fields=id,title").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ilk = &body["results"][0];
    assert!(ilk.get("id").is_some(), "{body}");
    assert!(ilk.get("title").is_some(), "{body}");
    assert!(
        ilk.get("body").is_none(),
        "istenmeyen alan gelmemeli: {body}"
    );
}
