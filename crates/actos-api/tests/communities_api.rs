//! `crates/actos-api/routes/communities.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/posts_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    auth::{self as core_auth, ActorType, Permission, PermissionScope},
    community as core_community,
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

/// [`seed_actor`] + hesap silme için ilk kurtarma kodunu da döner.
#[allow(clippy::expect_used)]
async fn seed_actor_with_recovery(pool: &PgPool, username: &str) -> (i64, String, String) {
    let reg = core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    let recovery_code = reg
        .recovery_codes
        .first()
        .expect("kurtarma kodu olmalı")
        .clone();
    (reg.actor.id, reg.api_key, recovery_code)
}

/// `visibility` verilerek topluluk açar.
#[allow(clippy::expect_used)]
async fn create_community_with_visibility(
    router: &Router,
    token: &str,
    name: &str,
    description: &str,
    visibility: &str,
) -> (StatusCode, Value, HeaderMap) {
    send(
        router,
        auth_json_req(
            "POST",
            "/communities",
            token,
            json!({ "name": name, "description": description, "visibility": visibility }),
        ),
    )
    .await
}

/// Bir topluluğa doğrudan SQL ile üye ekler — private topluluğa `join`
/// reddedildiği için (davet/başvuru ayrı iş) testte tek yol.
#[allow(clippy::expect_used)]
async fn add_member(pool: &PgPool, community_id: i64, actor_id: i64) {
    sqlx::query!(
        r#"INSERT INTO community_members (community_id, actor_id)
           VALUES ($1, $2)
           ON CONFLICT DO NOTHING"#,
        community_id,
        actor_id,
    )
    .execute(pool)
    .await
    .expect("üyelik eklenebilmeli");
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

/// Bir aktöre topluluk kapsamlı tek bir izin verir.
#[allow(clippy::expect_used)]
async fn grant_scoped(pool: &PgPool, actor_id: i64, permission: Permission, community_id: i64) {
    core_auth::grant_permission(
        pool,
        actor_id,
        permission,
        PermissionScope::Community,
        Some(community_id),
        None,
    )
    .await
    .expect("topluluk kapsamlı izin verilebilmeli");
}

/// Topluluğun iç kimliğini isimden çözer.
#[allow(clippy::expect_used)]
async fn community_id(pool: &PgPool, name: &str) -> i64 {
    core_community::resolve_id_by_name(pool, name)
        .await
        .expect("topluluk bulunabilmeli")
}

/// Bir toplulukta post açar, dış id'sini döner.
#[allow(clippy::expect_used)]
async fn post_in_community(router: &Router, token: &str, community: &str, title: &str) -> String {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            "/posts",
            token,
            json!({ "title": title, "body": "gövde", "community": community }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post açılamadı: {body}");
    body["id"].as_str().expect("post id").to_owned()
}

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
async fn sahip_ayrilinca_devralicisiz_topluluk_kapanir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "leave_owner").await;

    create_community(&router, &owner_key, "sahip_kulubu", "açıklama").await;

    // Devralıcı ve ikinci bir moderatör yok: ayrılma topluluğu kapatır (§4).
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/sahip_kulubu/join", &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, empty_req("GET", "/communities/sahip_kulubu")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "kapalı topluluk 404: {body}");
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

/// Sahibin atadığı bir aktör, **topluluk kapsamlı** `community.edit` ile
/// açıklamayı güncelleyebilir. Düzenleme yetkisi global olmak zorunda
/// değil: kapsam kontrolü hedefe göre yapılır (`authz::has_for`).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_kapsamli_edit_izni_guncelleyebilir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "kapsam_sahibi").await;
    let (editor_id, editor_key) = seed_actor(&raw_pool, "kapsam_editoru").await;

    create_community(&router, &owner_key, "kapsam_kulubu", "eski").await;
    let community = core_community::get_community(&raw_pool, "kapsam_kulubu")
        .await
        .expect("topluluk okunabilmeli");

    core_auth::grant_permission(
        &raw_pool,
        editor_id,
        Permission::CommunityEdit,
        PermissionScope::Community,
        Some(community.id),
        None,
    )
    .await
    .expect("izin verilebilmeli");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/kapsam_kulubu",
            &editor_key,
            json!({ "description": "kapsamlı editör düzenledi" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["description"], "kapsamlı editör düzenledi", "{body}");
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

// --- Faz 3: sahiplik = izin --------------------------------------------

/// Topluluk oluşturmak sahibe on topluluk kapsamlı izni gerçek satırlar
/// olarak verir; `whoami` bunları kapsam ve topluluk adıyla gösterir.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sahip_tum_topluluk_izinlerini_alir_ve_whoami_de_gorunur(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "izin_sahibi").await;

    create_community(&router, &owner_key, "izin_kulubu", "açıklama").await;

    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &owner_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let permissions = body["permissions"]
        .as_array()
        .expect("permissions dizi olmalı");
    assert_eq!(
        permissions.len(),
        core_community::OWNER_PERMISSIONS.len(),
        "sahip tam olarak OWNER_PERMISSIONS kadar izin tutmalı: {body}"
    );
    for p in permissions {
        assert_eq!(p["scope"], "community", "{body}");
        assert_eq!(p["community"], "izin_kulubu", "{body}");
    }
    assert!(
        permissions.iter().any(|p| p["permission"] == "member.ban"),
        "sahip member.ban tutmalı: {body}"
    );
}

// --- Faz 3: topluluk ban'ı ---------------------------------------------

/// Topluluk ban'ı post/yorum/katılmayı engeller, global yazmayı engellemez
/// ve kaldırılınca her şey geri gelir.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_bani_yazmayi_engeller_unban_gecirir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "ban_sahibi").await;
    let (_, kurban_key) = seed_actor(&raw_pool, "ban_kurbani").await;

    create_community(&router, &owner_key, "ban_kulubu", "açıklama").await;

    // Kurban üye olup toplulukta bir post açıyor.
    let (status, _, _) = send(
        &router,
        auth_req("POST", "/communities/ban_kulubu/join", &kurban_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let post = post_in_community(&router, &kurban_key, "ban_kulubu", "ban öncesi").await;

    // Sahip (member.ban'ı var) kurbanı topluluktan banlıyor.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &owner_key,
            json!({ "username": "ban_kurbani", "reason": "kural ihlali", "community": "ban_kulubu" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["community"], "ban_kulubu", "{body}");

    // Yazma yolları kapandı.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &kurban_key,
            json!({ "title": "ban sonrası", "body": "gövde", "community": "ban_kulubu" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "BANNED", "{body}");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post}/comments"),
            &kurban_key,
            json!({ "body": "ban sonrası yorum" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "BANNED", "{body}");

    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/ban_kulubu/join", &kurban_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "BANNED", "{body}");

    // Global yazma etkilenmedi: topluluk ban'ı platform ban'ı değil.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &kurban_key,
            json!({ "title": "bağımsız hâlâ çalışıyor", "body": "gövde" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Ban kaldırılınca katılma ve yazma geri gelir.
    let (status, body, _) = send(
        &router,
        auth_req(
            "DELETE",
            "/admin/bans/ban_kurbani?community=ban_kulubu",
            &owner_key,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, _, _) = send(
        &router,
        auth_req("POST", "/communities/ban_kulubu/join", &kurban_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let _ = post_in_community(&router, &kurban_key, "ban_kulubu", "ban sonrası tekrar").await;
}

/// Topluluk moderatörü kendi topluluğunda ban atar; başka bir toplulukta
/// ya da global olarak atamaz (`403`).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn moderator_baska_toplulukta_ve_globalde_ban_atamaz(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "bm_sahibi").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "bm_moderator").await;
    seed_actor(&raw_pool, "bm_kurban").await;

    create_community(&router, &owner_key, "bm_bir", "açıklama").await;
    create_community(&router, &owner_key, "bm_iki", "açıklama").await;
    let bir_id = community_id(&raw_pool, "bm_bir").await;
    grant_scoped(&raw_pool, mod_id, Permission::MemberBan, bir_id).await;

    // Kendi topluluğunda ban atabiliyor.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &mod_key,
            json!({ "username": "bm_kurban", "reason": "x", "community": "bm_bir" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Başka toplulukta atamıyor.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &mod_key,
            json!({ "username": "bm_kurban", "reason": "x", "community": "bm_iki" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Global atamıyor.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &mod_key,
            json!({ "username": "bm_kurban", "reason": "x" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

// --- Faz 3: banla-ve-sil kuyruğu ---------------------------------------

/// `delete_posts: true` bir `moderation_jobs` satırı yazar; worker onu
/// işleyip kurbanın o topluluktaki canlı içeriğini soft-delete eder.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ban_with_delete_kuyruga_yazilir_worker_siler(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "bwd_sahibi").await;
    let (_, kurban_key) = seed_actor(&raw_pool, "bwd_kurban").await;

    create_community(&router, &owner_key, "bwd_kulubu", "açıklama").await;
    let (status, _, _) = send(
        &router,
        auth_req("POST", "/communities/bwd_kulubu/join", &kurban_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let topluluk_post = post_in_community(&router, &kurban_key, "bwd_kulubu", "silinecek").await;
    let bagimsiz = {
        let (status, body, _) = send(
            &router,
            auth_json_req(
                "POST",
                "/posts",
                &kurban_key,
                json!({ "title": "kalacak", "body": "gövde" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().expect("post id").to_owned()
    };

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &owner_key,
            json!({
                "username": "bwd_kurban",
                "reason": "kural ihlali",
                "community": "bwd_kulubu",
                "delete_posts": true,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let bekleyen: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM moderation_jobs WHERE processed_at IS NULL"#,
    )
    .fetch_one(&raw_pool)
    .await
    .expect("iş sayılabilmeli");
    assert_eq!(bekleyen, 1, "bir moderasyon işi kuyruklanmalı");

    let islenen = actos_core::moderation::run_pending_jobs(&raw_pool)
        .await
        .expect("worker çalışabilmeli");
    assert_eq!(islenen, 1);

    // Topluluk post'u artık `410` (soft-delete), bağımsız post hâlâ `200`.
    let (status, body, _) = send(
        &router,
        empty_req("GET", &format!("/posts/{topluluk_post}")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GONE,
        "topluluk post'u silinmeli: {body}"
    );

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{bagimsiz}"))).await;
    assert_eq!(status, StatusCode::OK, "bağımsız post kalmalı: {body}");

    // İş işlendi olarak işaretlenmiş olmalı.
    let bekleyen: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM moderation_jobs WHERE processed_at IS NULL"#,
    )
    .fetch_one(&raw_pool)
    .await
    .expect("iş sayılabilmeli");
    assert_eq!(bekleyen, 0, "iş processed_at ile kapatılmalı");
}

/// Yorum, kök post'un topluluğunu kendi satırına yazar.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_yorumu_kok_postun_toplulugunu_alisiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "yc_sahibi").await;
    let (_, yorumcu_key) = seed_actor(&raw_pool, "yc_yorumcu").await;

    create_community(&router, &owner_key, "yc_kulubu", "açıklama").await;
    let post = post_in_community(&router, &owner_key, "yc_kulubu", "başlık").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post}/comments"),
            &yorumcu_key,
            json!({ "body": "topluluk yorumu" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["community"]["name"], "yc_kulubu", "{body}");

    let kok_id = community_id(&raw_pool, "yc_kulubu").await;
    let yorum_community: Option<i64> = sqlx::query_scalar!(
        r#"SELECT community_id FROM contents
           WHERE content_type = 'comment'::content_type
           ORDER BY id DESC LIMIT 1"#,
    )
    .fetch_one(&raw_pool)
    .await
    .expect("yorum okunabilmeli");
    assert_eq!(yorum_community, Some(kok_id));
}

// --- Faz 3: kick --------------------------------------------------------

/// `member.kick` kapsamlı moderatör kendi topluluğunda atabilir; sahip
/// atılamaz, üye olmayan `404`, başka toplulukta `403`.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kick_kendi_toplulugunda_calisir_sinirlari_dogru(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "kick_sahibi").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "kick_moderator").await;
    let (_, uye_key) = seed_actor(&raw_pool, "kick_uye").await;

    create_community(&router, &owner_key, "kick_bir", "açıklama").await;
    create_community(&router, &owner_key, "kick_iki", "açıklama").await;
    let bir_id = community_id(&raw_pool, "kick_bir").await;
    grant_scoped(&raw_pool, mod_id, Permission::MemberKick, bir_id).await;

    let (status, _, _) = send(
        &router,
        auth_req("POST", "/communities/kick_bir/join", &uye_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Başka toplulukta atamaz.
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/kick_iki/members/kick_uye", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Kendi topluluğunda atabilir.
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/kick_bir/members/kick_uye", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Artık üye değil.
    let (status, body, _) = send(&router, auth_req("GET", "/communities/kick_bir", &uye_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_member"], false, "{body}");

    // Üye olmayanı tekrar atmak `404`.
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/kick_bir/members/kick_uye", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Sahip atılamaz.
    let (status, body, _) = send(
        &router,
        auth_req(
            "DELETE",
            "/communities/kick_bir/members/kick_sahibi",
            &mod_key,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

// --- Faz 4B-1: private görünürlük ve kapak ------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn private_topluluk_olusturulur_dizinde_yok_kapak_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "priv_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "priv_member").await;

    let (status, body, _) = create_community_with_visibility(
        &router,
        &owner_key,
        "gizli_kulup",
        "özel açıklama",
        "private",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["visibility"], "private", "{body}");

    // Dizin yalnızca public listeler.
    let (status, body, _) = send(&router, empty_req("GET", "/communities")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<&str> = body["communities"]
        .as_array()
        .expect("communities dizi olmalı")
        .iter()
        .map(|c| c["name"].as_str().expect("isim string"))
        .collect();
    assert!(
        !names.contains(&"gizli_kulup"),
        "private dizinde görünmemeli: {body}"
    );

    // Üye olmayan: kapak — ad/açıklama var, sayaçlar sıfır.
    let (status, cover, _) = send(
        &router,
        auth_req("GET", "/communities/gizli_kulup", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cover}");
    assert_eq!(cover["name"], "gizli_kulup", "{cover}");
    assert_eq!(cover["description"], "özel açıklama", "{cover}");
    assert_eq!(cover["visibility"], "private", "{cover}");
    assert_eq!(cover["is_member"], false, "{cover}");
    assert_eq!(cover["member_count"], 0, "{cover}");
    assert_eq!(cover["post_count"], 0, "{cover}");

    // Anonim de kapak alır.
    let (status, cover, _) = send(&router, empty_req("GET", "/communities/gizli_kulup")).await;
    assert_eq!(status, StatusCode::OK, "{cover}");
    assert_eq!(cover["member_count"], 0, "{cover}");

    // Üye olunca tam özet.
    let cid = community_id(&raw_pool, "gizli_kulup").await;
    add_member(&raw_pool, cid, member_id).await;
    let (status, full, _) = send(
        &router,
        auth_req("GET", "/communities/gizli_kulup", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{full}");
    assert_eq!(full["is_member"], true, "{full}");
    assert_eq!(full["member_count"], 2, "{full}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gorunurluk_sahipleri_ozel_toplulugu_tam_gorur(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "cap_owner").await;
    let (scoped_mod_id, scoped_mod_key) = seed_actor(&raw_pool, "cap_scoped").await;
    let (global_mod_id, global_mod_key) = seed_actor(&raw_pool, "cap_global").await;

    create_community_with_visibility(
        &router,
        &owner_key,
        "kapak_gorunurluk",
        "açıklama",
        "private",
    )
    .await;
    let cid = community_id(&raw_pool, "kapak_gorunurluk").await;

    grant_scoped(&raw_pool, scoped_mod_id, Permission::CommunityEdit, cid).await;
    core_auth::grant_permission(
        &raw_pool,
        global_mod_id,
        Permission::ContentDelete,
        PermissionScope::Global,
        None,
        None,
    )
    .await
    .expect("global izin verilebilmeli");

    // Topluluk kapsamlı izin sahibi üye değildir ama içeriyi görür.
    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/kapak_gorunurluk", &scoped_mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_member"], false, "{body}");
    assert_eq!(body["member_count"], 1, "kapak değil tam özet: {body}");

    // Global moderatör (Faz 4A istisnası) de tam görür.
    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/kapak_gorunurluk", &global_mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["member_count"], 1,
        "global moderatör tam görür: {body}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn public_private_tek_yonlu(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "viz_owner").await;

    create_community(&router, &owner_key, "yon_kulubu", "açıklama").await;

    // public → private serbest.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/yon_kulubu",
            &owner_key,
            json!({ "description": "açıklama", "visibility": "private" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["visibility"], "private", "{body}");

    // private → public yasak (§2).
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/yon_kulubu",
            &owner_key,
            json!({ "description": "açıklama", "visibility": "public" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");

    // Aynı değeri tekrar göndermek etkisizdir, hata değil.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/yon_kulubu",
            &owner_key,
            json!({ "description": "güncel", "visibility": "private" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["visibility"], "private", "{body}");

    // Geçersiz değer 400.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/yon_kulubu",
            &owner_key,
            json!({ "description": "açıklama", "visibility": "gizli" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn private_topluluga_dogrudan_katilma_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "privjoin_owner").await;
    let (_, other_key) = seed_actor(&raw_pool, "privjoin_other").await;

    create_community_with_visibility(&router, &owner_key, "kapali_kulup", "açıklama", "private")
        .await;

    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/kapali_kulup/join", &other_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

// --- Faz 4B-1: kapanış --------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn public_kapaninca_postlar_bagimsiz_kalir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "kapanan_owner").await;

    create_community(&router, &owner_key, "kapanan_kulup", "açıklama").await;
    let post = post_in_community(&router, &owner_key, "kapanan_kulup", "kalacak").await;

    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/kapanan_kulup/close", &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Dizinden düştü, uçlar 404.
    let (status, body, _) = send(&router, empty_req("GET", "/communities")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<&str> = body["communities"]
        .as_array()
        .expect("communities dizi olmalı")
        .iter()
        .map(|c| c["name"].as_str().expect("isim string"))
        .collect();
    assert!(!names.contains(&"kapanan_kulup"), "{body}");

    let (status, body, _) = send(&router, empty_req("GET", "/communities/kapanan_kulup")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/communities/kapanan_kulup/posts"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Post bağımsız oldu: hâlâ okunur, community alanı null.
    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{post}"))).await;
    assert_eq!(status, StatusCode::OK, "post bağımsız kalmalı: {body}");
    assert!(body["community"].is_null(), "{body}");

    // Kapanan topluluğa katılma 404.
    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/kapanan_kulup/join", &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn private_kapaninca_postlar_gizlenir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "pclose_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "pclose_member").await;

    create_community_with_visibility(&router, &owner_key, "pclose_kulup", "açıklama", "private")
        .await;
    let cid = community_id(&raw_pool, "pclose_kulup").await;
    add_member(&raw_pool, cid, member_id).await;
    let post = post_in_community(&router, &member_key, "pclose_kulup", "gizli post").await;

    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/pclose_kulup/close", &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Topluluk ucu kapandı.
    let (status, body, _) = send(&router, empty_req("GET", "/communities/pclose_kulup")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Üye olmayan göremez: görünmezlik silinmişlikten önce gelir (404).
    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{post}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "yabancıya 404: {body}");

    // Eski üye hâlâ görür ama içerik silinmiş: 410.
    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post}"), &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "üyeye 410: {body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yetkisiz_kapatma_403(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "noclose_owner").await;
    let (_, other_key) = seed_actor(&raw_pool, "noclose_other").await;

    create_community(&router, &owner_key, "noclose_kulubu", "açıklama").await;

    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/noclose_kulubu/close", &other_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Hâlâ açık.
    let (status, body, _) = send(&router, empty_req("GET", "/communities/noclose_kulubu")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kapali_topluluk_her_ucta_404(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "closed_owner").await;

    create_community(&router, &owner_key, "closed_kulup", "açıklama").await;

    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/closed_kulup/close", &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Zaten kapalıyı kapatmak 404.
    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/closed_kulup/close", &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    for (method, uri, body) in [
        ("GET", "/communities/closed_kulup", None),
        ("GET", "/communities/closed_kulup/members", None),
        ("GET", "/communities/closed_kulup/posts", None),
        ("POST", "/communities/closed_kulup/join", None),
        (
            "PATCH",
            "/communities/closed_kulup",
            Some(json!({ "description": "yeni" })),
        ),
        (
            "PUT",
            "/communities/closed_kulup/successor",
            Some(json!({ "username": "closed_owner" })),
        ),
    ] {
        let req = match body {
            Some(value) => auth_json_req(method, uri, &owner_key, value),
            None => auth_req(method, uri, &owner_key),
        };
        let (status, resp, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {resp}");
    }
}

// --- Faz 4B-1: devralma -------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn devralici_atanir_ve_hesap_silinince_sahip_olur(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key, recovery) = seed_actor_with_recovery(&raw_pool, "devir_owner").await;
    let (_, heir_key) = seed_actor(&raw_pool, "devir_halef").await;

    create_community(&router, &owner_key, "devir_kulubu", "eski açıklama").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/communities/devir_kulubu/successor",
            &owner_key,
            json!({ "username": "devir_halef" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Sahip hesabını siler; devralıcı geçer.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            "/actors/me",
            &owner_key,
            json!({ "recovery_code": recovery }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/devir_kulubu", &heir_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["owner"]["username"], "devir_halef",
        "sahiplik devralıcıya geçmeli: {body}"
    );
    assert_eq!(body["is_member"], true, "devralan üye olmalı: {body}");

    // Devralan artık gerçekten düzenleyip kapatabilir.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/devir_kulubu",
            &heir_key,
            json!({ "description": "yeni sahip düzenledi" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["description"], "yeni sahip düzenledi", "{body}");

    let (status, body, _) = send(
        &router,
        auth_req("POST", "/communities/devir_kulubu/close", &heir_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sahip_ayrilinca_devralici_sahiplenir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (owner_id, owner_key) = seed_actor(&raw_pool, "leave2_owner").await;
    let _ = seed_actor(&raw_pool, "leave2_halef").await;

    create_community(&router, &owner_key, "leave2_kulubu", "açıklama").await;

    let (status, _, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/communities/leave2_kulubu/successor",
            &owner_key,
            json!({ "username": "leave2_halef" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Sahip ayrılır → devralıcı sahiplenir, sahip üyelikten düşer.
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/leave2_kulubu/join", &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, empty_req("GET", "/communities/leave2_kulubu")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["owner"]["username"], "leave2_halef", "{body}");

    let still_member = core_community::is_member(
        &raw_pool,
        community_id(&raw_pool, "leave2_kulubu").await,
        owner_id,
    )
    .await
    .expect("üyelik sorgulanabilmeli");
    assert!(!still_member, "ayrılan sahip artık üye olmamalı");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn devralicisiz_en_kidemli_moderator_sahiplenir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key, recovery) = seed_actor_with_recovery(&raw_pool, "kidem_owner").await;
    let (mod1_id, mod1_key) = seed_actor(&raw_pool, "kidem_bir").await;
    let (mod2_id, _) = seed_actor(&raw_pool, "kidem_iki").await;

    create_community(&router, &owner_key, "kidem_kulubu", "açıklama").await;
    let cid = community_id(&raw_pool, "kidem_kulubu").await;

    // İki moderatör; birincinin izni daha eski (kıdem = eski granted_at).
    grant_scoped(&raw_pool, mod1_id, Permission::CommunityEdit, cid).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    grant_scoped(&raw_pool, mod2_id, Permission::CommunityEdit, cid).await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            "/actors/me",
            &owner_key,
            json!({ "recovery_code": recovery }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, empty_req("GET", "/communities/kidem_kulubu")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["owner"]["username"], "kidem_bir",
        "en kıdemli moderatör sahiplenmeli: {body}"
    );

    // Yeni sahip düzenleyebilir.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/communities/kidem_kulubu",
            &mod1_key,
            json!({ "description": "kıdemli sahip düzenledi" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn devralicisiz_ve_moderatorsuz_silinme_toplulugu_kapatir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key, recovery) = seed_actor_with_recovery(&raw_pool, "bosaltan_owner").await;

    create_community(&router, &owner_key, "bos_kulup", "açıklama").await;
    let post = post_in_community(&router, &owner_key, "bos_kulup", "bağımsız olacak").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            "/actors/me",
            &owner_key,
            json!({ "recovery_code": recovery }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Devralıcısız ve moderatörsüz: topluluk kapandı, public post bağımsız.
    let (status, body, _) = send(&router, empty_req("GET", "/communities/bos_kulup")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "topluluk kapanmalı: {body}");

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{post}"))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "public post bağımsız kalmalı: {body}"
    );
    assert!(body["community"].is_null(), "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn devralici_yalnizca_sahip_ve_hedef_canli(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "succ_owner").await;
    let (_, other_key) = seed_actor(&raw_pool, "succ_other").await;

    create_community(&router, &owner_key, "succ_kulubu", "açıklama").await;

    // Sahip olmayan atayamaz.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/communities/succ_kulubu/successor",
            &other_key,
            json!({ "username": "succ_owner" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Bilinmeyen hedef 404.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/communities/succ_kulubu/successor",
            &owner_key,
            json!({ "username": "hic_olmayan" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
