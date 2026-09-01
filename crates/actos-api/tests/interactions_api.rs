//! `crates/actos-api/routes/interactions.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/posts_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! Oy sayaçlarının tutarlılığı, eşzamanlılık ve `hot_score` hesabı burada
//! değil, `crates/actos-core/tests/interaction.rs`'te: onlar domain
//! davranışı, HTTP'ye ihtiyaç duymuyorlar. Buradaki testler HTTP katmanına
//! özgü olanlar — durum kodları, kimlik zorunluluğu, dış id çözümü ve
//! query parametrelerinin ayrıştırılması.

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    auth::{self as core_auth, ActorType},
    config::{
        DatabaseConfig, LimitTable, RedisConfig, SecurityConfig, ServerConfig, StorageConfig,
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
            max_body_bytes: 1024 * 1024,
            trusted_proxy_hops: 0,
            // Testler router'ı doğrudan çağırıyor, periyodik iş hiç
            // başlatılmıyor; alan yalnızca `Config`'in parçası olduğu
            // için dolduruluyor.
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
    // Aynı benzersiz önek hem rate limiter hem idempotency deposu için:
    // ikisi de aynı gerçek Redis'i (`127.0.0.1:3102`) paylaşan paralel
    // testler arasında izolasyon istiyor (bkz. `actos_core::idempotency`
    // ve `actos_core::ratelimit` modüllerindeki "yalnızca testler için"
    // gerekçesi) — farklı anahtar isim uzayları (`rl:` / `idem:`) zaten
    // ayrık olduğu için aynı öneki paylaşmaları çakışmaya yol açmaz.
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

/// Kimliksiz JSON isteği — `PUT /contents/{id}/vote`'un 401 döndüğünü
/// doğrulayan test için.
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
fn empty_req(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("istek kurulabilmeli")
}

/// `auth_req` + JSON gövde — bkz. `tests/actors_api.rs`'teki aynı isimli
/// yardımcı üzerindeki yorum.
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

// --- PUT /contents/{id}/vote ----------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn oy_verme_200_ve_sayaclari_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_oy_yazar").await;
    let (_, oylayan_key) = seed_actor(&raw_pool, "http_oy_veren").await;
    let post = seed_post(&router, &yazar_key, "oylanacak").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{post}/vote"),
            &oylayan_key,
            json!({ "value": 1 }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["value"], 1, "{body}");
    assert_eq!(body["score"], 1, "{body}");
    assert_eq!(body["upvotes"], 1, "{body}");
    assert_eq!(body["downvotes"], 0, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kendi_postuna_oy_403_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "http_kendi_oy").await;
    let post = seed_post(&router, &api_key, "kendi postum").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{post}/vote"),
            &api_key,
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gecersiz_oy_degeri_400_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_gecersiz_yazar").await;
    let (_, oylayan_key) = seed_actor(&raw_pool, "http_gecersiz_oylayan").await;
    let post = seed_post(&router, &yazar_key, "post").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{post}/vote"),
            &oylayan_key,
            json!({ "value": 7 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kimliksiz_oy_401_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "http_kimliksiz").await;
    let post = seed_post(&router, &api_key, "post").await;

    let (status, body, _) = send(
        &router,
        json_req(
            "PUT",
            &format!("/contents/{post}/vote"),
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

// --- GET /me/votes ---------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn toplu_oy_sorgusu_harita_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "toplu_http_yazar").await;
    let (_, oylayan_key) = seed_actor(&raw_pool, "toplu_http_oylayan").await;

    let a = seed_post(&router, &yazar_key, "a").await;
    let b = seed_post(&router, &yazar_key, "b").await;
    let c = seed_post(&router, &yazar_key, "c").await;

    for (id, value) in [(&a, 1), (&b, -1)] {
        let (status, body, _) = send(
            &router,
            auth_json_req(
                "PUT",
                &format!("/contents/{id}/vote"),
                &oylayan_key,
                json!({ "value": value }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let (status, body, _) = send(
        &router,
        auth_req(
            "GET",
            &format!("/me/votes?content_ids={a},{b},{c}"),
            &oylayan_key,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["votes"][&a], 1, "{body}");
    assert_eq!(body["votes"][&b], -1, "{body}");
    assert!(
        body["votes"].get(&c).is_none(),
        "oy verilmemiş içerik haritada olmamalı: {body}"
    );
}

/// Çözülemeyen bir id bütün sorguyu düşürmemeli — bu bir toplu arama ucu.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn toplu_oy_sorgusu_bozuk_idyi_atliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "bozuk_id_yazar").await;
    let (_, oylayan_key) = seed_actor(&raw_pool, "bozuk_id_oylayan").await;
    let a = seed_post(&router, &yazar_key, "a").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{a}/vote"),
            &oylayan_key,
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body, _) = send(
        &router,
        auth_req(
            "GET",
            &format!("/me/votes?content_ids={a},bozuk_id,c_zzzz"),
            &oylayan_key,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["votes"][&a], 1, "{body}");
}

// --- Takip -----------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn takip_204_ve_takipci_listesinde_gorunuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, a_key) = seed_actor(&raw_pool, "http_takipci").await;
    seed_actor(&raw_pool, "http_takip_edilen").await;

    let (status, body, _) = send(
        &router,
        auth_req("PUT", "/actors/http_takip_edilen/follow", &a_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/http_takip_edilen/followers"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["actors"][0]["username"], "http_takipci", "{body}");

    // Takibi bırak: liste boşalmalı.
    let (status, _, _) = send(
        &router,
        auth_req("DELETE", "/actors/http_takip_edilen/follow", &a_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/http_takip_edilen/followers"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["actors"].as_array().expect("dizi").is_empty(),
        "{body}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn olmayan_kullaniciyi_takip_404(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, a_key) = seed_actor(&raw_pool, "http_takip_404").await;

    let (status, body, _) = send(&router, auth_req("PUT", "/actors/hicyok/follow", &a_key)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kendini_takip_400_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, a_key) = seed_actor(&raw_pool, "http_kendini").await;

    let (status, body, _) = send(
        &router,
        auth_req("PUT", "/actors/http_kendini/follow", &a_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

// --- Kaydetme --------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kaydetme_ve_me_saves_listesi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "http_kayit_yazar").await;
    let (_, kaydeden_key) = seed_actor(&raw_pool, "http_kaydeden").await;
    let post = seed_post(&router, &yazar_key, "kaydedilecek").await;

    let (status, body, _) = send(
        &router,
        auth_req("PUT", &format!("/contents/{post}/save"), &kaydeden_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/me/saves", &kaydeden_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["saves"][0]["id"], post, "{body}");
    assert_eq!(body["saves"][0]["title"], "kaydedilecek", "{body}");

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/contents/{post}/save"), &kaydeden_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(&router, auth_req("GET", "/me/saves", &kaydeden_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["saves"].as_array().expect("dizi").is_empty(), "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn me_saves_fields_ile_filtreleniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "kayit_fields_yazar").await;
    let (_, kaydeden_key) = seed_actor(&raw_pool, "kayit_fields_kaydeden").await;
    let post = seed_post(&router, &yazar_key, "başlık").await;

    let (status, _, _) = send(
        &router,
        auth_req("PUT", &format!("/contents/{post}/save"), &kaydeden_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/me/saves?fields=id,title", &kaydeden_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ilk = &body["saves"][0];
    assert!(ilk.get("id").is_some(), "{body}");
    assert!(ilk.get("title").is_some(), "{body}");
    assert!(ilk.get("score").is_none(), "{body}");
}

/// Yorumlar da kaydedilebilir: `saves.content_id` tür ayrımı yapmıyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yorum_da_kaydedilebiliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "yorum_kayit_yazar").await;
    let (_, kaydeden_key) = seed_actor(&raw_pool, "yorum_kaydeden").await;
    let post = seed_post(&router, &yazar_key, "post").await;

    let (status, yorum, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post}/comments"),
            &yazar_key,
            json!({ "body": "kaydedilecek yorum" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{yorum}");
    let yorum_id = yorum["id"].as_str().expect("yorum id").to_owned();

    let (status, _, _) = send(
        &router,
        auth_req("PUT", &format!("/contents/{yorum_id}/save"), &kaydeden_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(&router, auth_req("GET", "/me/saves", &kaydeden_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["saves"][0]["content_type"], "comment", "{body}");
    assert_eq!(body["saves"][0]["body"], "kaydedilecek yorum", "{body}");
}
