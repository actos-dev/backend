//! `crates/actos-api/routes/communities.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/posts_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.

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
    http::{HeaderMap, Request, StatusCode, header},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt as _;

// --- Kurulum yardımcıları (bkz. `tests/posts_api.rs` — aynı desen) -------

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
async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value, HeaderMap) {
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

/// `/auth/register`'ın hız sınırını görmeden doğrudan domain katmanından
/// bir actor oluşturur, döner: `(actor_id, api_key)`.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> (i64, String) {
    let reg = core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    (reg.actor.id, reg.api_key)
}

/// Bir topluluk oluşturur, `(status, body, headers)` döner.
#[allow(clippy::expect_used)]
async fn create_community(
    router: &Router,
    token: &str,
    name: &str,
    description: &str,
) -> (StatusCode, Value, HeaderMap) {
    send(
        router,
        auth_json_req(
            "POST",
            "/communities",
            token,
            json!({ "name": name, "description": description }),
        ),
    )
    .await
}

// --- Oluşturma ---------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_olusturma_201_location_ve_sahip_uye(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "community_owner").await;

    let (status, body, headers) =
        create_community(&router, &api_key, "rust_turkiye", "Türkçe Rust topluluğu").await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "rust_turkiye");
    assert_eq!(body["description"], "Türkçe Rust topluluğu");
    assert_eq!(body["visibility"], "public");
    assert_eq!(body["member_count"], 1);
    assert_eq!(body["post_count"], 0);
    assert_eq!(body["is_member"], true, "{body}");
    assert_eq!(body["owner"]["username"], "community_owner");
    assert!(
        body["id"].as_str().is_some_and(|s| s.starts_with("m_")),
        "topluluk id'si m_ önekiyle başlamalı: {body}"
    );

    let location = headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("Location header'ı olmalı");
    assert_eq!(location, "/communities/rust_turkiye");

    // Sahip `community_members`'a yazıldı mı? (Locations/response dışında
    // tek doğruluk kaynağı üyelik tablosu.)
    let member_count: i64 =
        sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM community_members"#,)
            .fetch_one(&raw_pool)
            .await
            .expect("üye sayısı sorgulanabilmeli");
    assert_eq!(member_count, 1);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ayni_isimle_ikinci_topluluk_409(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "dup_owner").await;

    let (status, _, _) = create_community(&router, &api_key, "kampus", "birinci").await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body, _) = create_community(&router, &api_key, "kampus", "ikinci").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "CONFLICT");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn rezerve_isim_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "reserved_owner").await;

    let (status, body, _) = create_community(&router, &api_key, "moderator", "açıklama").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bozuk_isim_formati_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "format_owner").await;

    // Büyük harf + boşluk + tire: üçü de format dışı.
    let (status, body, _) = create_community(&router, &api_key, "Bad Name", "açıklama").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Alt sınır: 2 karakter.
    let (status, _, _) = create_community(&router, &api_key, "ab", "açıklama").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sahiplik_siniri_dorduncude_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "cap_owner").await;

    for name in ["kapi_bir", "kapi_iki", "kapi_uc"] {
        let (status, body, _) = create_community(&router, &api_key, name, "açıklama").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let (status, body, _) = create_community(&router, &api_key, "kapi_dort", "açıklama").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

// --- Dizin -------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn dizin_en_yeni_once_ve_sayfalaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "dir_owner").await;

    for name in ["dizin_bir", "dizin_iki", "dizin_uc"] {
        let (status, _, _) = create_community(&router, &api_key, name, "açıklama").await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (status, body, _) = send(&router, empty_req("GET", "/communities?limit=2")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page1: Vec<&str> = body["communities"]
        .as_array()
        .expect("communities dizi olmalı")
        .iter()
        .map(|c| c["name"].as_str().expect("isim string"))
        .collect();
    assert_eq!(page1, vec!["dizin_uc", "dizin_iki"], "{body}");
    // Dizinde `is_member` her zaman false: sorulacak bir actor yok.
    assert_eq!(body["communities"][0]["is_member"], false);

    let cursor = body["next_cursor"]
        .as_str()
        .expect("ilk sayfadan sonra cursor olmalı")
        .to_owned();

    let (status, body2, _) = send(
        &router,
        empty_req("GET", &format!("/communities?limit=2&cursor={cursor}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body2}");
    let page2: Vec<&str> = body2["communities"]
        .as_array()
        .expect("communities dizi olmalı")
        .iter()
        .map(|c| c["name"].as_str().expect("isim string"))
        .collect();
    assert_eq!(page2, vec!["dizin_bir"], "{body2}");
    assert!(body2["next_cursor"].is_null(), "{body2}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bilinmeyen_topluluk_404(pool: PgPool) {
    let router = build_router(pool);

    let (status, body, _) = send(&router, empty_req("GET", "/communities/yok_boyle")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "NOT_FOUND");
}

// --- Katılma / ayrılma -------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn katilma_ve_ayrilma_idempotent(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "join_owner").await;
    let (_, other_key) = seed_actor(&raw_pool, "join_member").await;

    create_community(&router, &owner_key, "katilma_kulubu", "açıklama").await;

    for _ in 0..2 {
        let (status, body, _) = send(
            &router,
            auth_req("POST", "/communities/katilma_kulubu/join", &other_key),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    // Üyelik gerçekten kuruldu mu?
    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/katilma_kulubu", &other_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_member"], true, "{body}");
    assert_eq!(body["member_count"], 2, "{body}");

    for _ in 0..2 {
        let (status, body, _) = send(
            &router,
            auth_req("DELETE", "/communities/katilma_kulubu/join", &other_key),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/katilma_kulubu", &other_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_member"], false, "{body}");
    assert_eq!(body["member_count"], 1, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sahip_ayrilamaz(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "leave_owner").await;

    create_community(&router, &owner_key, "sahip_kulubu", "açıklama").await;

    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/sahip_kulubu/join", &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

// --- Gönderi: üyelik şartı ---------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluga_gonderi_uyelik_gerektirir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "post_owner").await;
    let (_, outsider_key) = seed_actor(&raw_pool, "post_outsider").await;

    create_community(&router, &owner_key, "gonderi_kulubu", "açıklama").await;

    // Üye olmayan 403.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &outsider_key,
            json!({
                "title": "izinsiz",
                "body": "gövde",
                "community": "gonderi_kulubu",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "FORBIDDEN");

    // Katıldıktan sonra 201 + community alanı dolu.
    let (status, _, _) = send(
        &router,
        auth_req("POST", "/communities/gonderi_kulubu/join", &outsider_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &outsider_key,
            json!({
                "title": "üye gönderisi",
                "body": "gövde",
                "community": "gonderi_kulubu",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["community"]["name"], "gonderi_kulubu", "{body}");
    assert!(
        body["community"]["id"]
            .as_str()
            .is_some_and(|s| s.starts_with("m_")),
        "community.id m_ önekiyle başlamalı: {body}"
    );

    // Topluluksuz gönderide alan `null`.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &outsider_key,
            json!({ "title": "bağımsız", "body": "gövde" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["community"].is_null(), "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn olmayan_topluluga_gonderi_404(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "missing_community_author").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "başlık", "body": "gövde", "community": "hic_olmadi" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

// --- Topluluk akışı ----------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_akisi_yalnizca_o_toplulugun_postlari(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "feed_owner").await;

    create_community(&router, &owner_key, "akis_bir", "açıklama").await;
    create_community(&router, &owner_key, "akis_iki", "açıklama").await;

    for (title, community) in [
        ("birinci", "akis_bir"),
        ("ikinci", "akis_bir"),
        ("baskasi", "akis_iki"),
    ] {
        let (status, body, _) = send(
            &router,
            auth_json_req(
                "POST",
                "/posts",
                &owner_key,
                json!({ "title": title, "body": "gövde", "community": community }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    // Topluluksuz bir post da karışmamalı.
    let (status, _, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &owner_key,
            json!({ "title": "bagimsiz", "body": "gövde" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body, _) = send(&router, empty_req("GET", "/communities/akis_bir/posts")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let titles: Vec<&str> = body["posts"]
        .as_array()
        .expect("posts dizi olmalı")
        .iter()
        .map(|p| p["title"].as_str().expect("başlık string"))
        .collect();
    assert_eq!(titles, vec!["ikinci", "birinci"], "{body}");

    let (status, body, _) = send(&router, empty_req("GET", "/communities/akis_iki/posts")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let titles: Vec<&str> = body["posts"]
        .as_array()
        .expect("posts dizi olmalı")
        .iter()
        .map(|p| p["title"].as_str().expect("başlık string"))
        .collect();
    assert_eq!(titles, vec!["baskasi"], "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_akisi_uc_siralamayi_destekliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "sort_owner").await;
    let (_, voter1_key) = seed_actor(&raw_pool, "sort_voter1").await;
    let (_, voter2_key) = seed_actor(&raw_pool, "sort_voter2").await;

    create_community(&router, &owner_key, "siralama_kulubu", "açıklama").await;

    // Üç post; ilki en eski ama en yüksek oyu alacak (top/hot farkını
    // gösterebilmek için).
    let mut ids = Vec::new();
    for title in ["dusuk", "ortalama", "yuksek"] {
        let (status, body, _) = send(
            &router,
            auth_json_req(
                "POST",
                "/posts",
                &owner_key,
                json!({ "title": title, "body": "gövde", "community": "siralama_kulubu" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        ids.push(body["id"].as_str().expect("id string").to_owned());
    }

    // Oy: "dusuk" iki farklı aktörden (skor 2), "ortalama" bir aktörden
    // (skor 1). Aynı aktörün ikinci oyu idempotent olduğu için skoru
    // artırmaz — bu yüzden iki ayrı voter gerekiyor. (Kimse kendi içeriğine
    // oy veremez.)
    for (token, id) in [
        (&voter1_key, &ids[0]),
        (&voter2_key, &ids[0]),
        (&voter1_key, &ids[1]),
    ] {
        let (status, body, _) = send(
            &router,
            auth_json_req(
                "PUT",
                &format!("/contents/{id}/vote"),
                token,
                json!({ "value": 1 }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let titles_for = |body: &Value| -> Vec<String> {
        body["posts"]
            .as_array()
            .expect("posts dizi olmalı")
            .iter()
            .map(|p| p["title"].as_str().expect("başlık string").to_owned())
            .collect()
    };

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/communities/siralama_kulubu/posts?sort=new"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        titles_for(&body),
        vec!["yuksek", "ortalama", "dusuk"],
        "{body}"
    );

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/communities/siralama_kulubu/posts?sort=top"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        titles_for(&body),
        vec!["dusuk", "ortalama", "yuksek"],
        "{body}"
    );

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/communities/siralama_kulubu/posts?sort=hot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // hot_score oy anında güncelleniyor (bkz. `crate::interaction::set_vote`)
    // ve formülün zaman terimi geri kalanı eşit tuttuğu için en yüksek oy
    // en üstte olmalı.
    assert_eq!(
        titles_for(&body),
        vec!["dusuk", "ortalama", "yuksek"],
        "{body}"
    );
}

// --- Güncelleme --------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sahibi_olmayan_guncelleyemez_403(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "patch_owner").await;
    let (_, other_key) = seed_actor(&raw_pool, "patch_other").await;

    create_community(&router, &owner_key, "guncelleme_kulubu", "eski").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/guncelleme_kulubu",
            &other_key,
            json!({ "description": "yeni" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "FORBIDDEN");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sahibi_guncelleyebilir_200(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "patch_ok_owner").await;

    create_community(&router, &owner_key, "guncel_kulup", "eski açıklama").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/guncel_kulup",
            &owner_key,
            json!({ "description": "yeni açıklama" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["description"], "yeni açıklama", "{body}");

    // Kalıcı mı?
    let (status, body, _) = send(&router, empty_req("GET", "/communities/guncel_kulup")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["description"], "yeni açıklama", "{body}");
}

// --- Üye listesi -------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn uyeler_en_kidemli_once(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "uzun_uye").await;
    let (_, second_key) = seed_actor(&raw_pool, "ikinci_uye").await;
    let (_, third_key) = seed_actor(&raw_pool, "ucuncu_uye").await;

    create_community(&router, &owner_key, "uyeler_kulubu", "açıklama").await;

    // Sıralamayı joined_at belirliyor; aynı mikrosaniyeye düşerlerse
    // actor_id tiebreak'i yine doğru sırayı verir ama niyeti netleştirmek
    // için araya küçük bir gecikme koyuyoruz.
    for token in [&second_key, &third_key] {
        let (status, _, _) = send(
            &router,
            auth_req("POST", "/communities/uyeler_kulubu/join", token),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/communities/uyeler_kulubu/members"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let usernames: Vec<&str> = body["members"]
        .as_array()
        .expect("members dizi olmalı")
        .iter()
        .map(|m| {
            m["actor"]["username"]
                .as_str()
                .expect("kullanıcı adı string")
        })
        .collect();
    assert_eq!(
        usernames,
        vec!["uzun_uye", "ikinci_uye", "ucuncu_uye"],
        "{body}"
    );
    assert!(body["members"][0]["joined_at"].as_str().is_some());
}

// --- İzin (Phase 1 altyapısı) ------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn global_community_edit_izni_guncelleyebilir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "perm_owner").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "perm_admin").await;

    core_auth::grant_permission(
        &raw_pool,
        admin_id,
        core_auth::Permission::CommunityEdit,
        core_auth::PermissionScope::Global,
        None,
        None,
    )
    .await
    .expect("izin verilebilmeli");

    create_community(&router, &owner_key, "izin_kulubu", "eski").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/izin_kulubu",
            &admin_key,
            json!({ "description": "moderatör düzenledi" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["description"], "moderatör düzenledi", "{body}");
    // Global editör üye değildir; `is_member` doğruyu söylemeli.
    assert_eq!(body["is_member"], false, "{body}");
}

// --- Kimliksiz erişim --------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kimliksiz_yazma_401(pool: PgPool) {
    let router = build_router(pool);

    let (status, body, _) = send(
        &router,
        json_req(
            "POST",
            "/communities",
            json!({ "name": "kimliksiz", "description": "açıklama" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}
