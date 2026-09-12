//! `crates/actos-api/routes/posts.rs` entegrasyon testleri.
//!
//! Kurulum (`test_config`/`build_router`/`send`/`json_req`/`auth_req`/
//! `auth_json_req`/`register`) `tests/actors_api.rs` ile birebir aynı desen
//! — ayrı bir entegrasyon test binary'si olduğu için (Rust her `tests/*.rs`
//! dosyasını bağımsız derler) paylaşılan bir modül olmadan tekrar
//! tanımlanıyor.

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    auth::{self as core_auth, ActorType, AdminRole},
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
            max_body_bytes: 1024 * 1024,
            max_upload_bytes: 8 * 1024 * 1024,
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

/// `auth_json_req` + `Idempotency-Key` header'ı — idempotency testleri için.
#[allow(clippy::expect_used)]
fn auth_json_req_idem(
    method: &str,
    uri: &str,
    token: &str,
    idempotency_key: &str,
    body: Value,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("Idempotency-Key", idempotency_key)
        .body(Body::from(body.to_string()))
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

// --- POST /posts -------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn post_olusturma_201_location_ve_govde_dogru(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "post_author").await;

    let (status, body, headers) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({
                "title": "Merhaba dünya",
                "body": "Bu bir gövde metni.",
                "tags": ["rust", "actos"],
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["title"], "Merhaba dünya");
    assert_eq!(body["body"], "Bu bir gövde metni.");
    assert_eq!(body["content_type"], "post");
    assert_eq!(body["body_format"], "markdown");
    assert_eq!(body["author_deleted"], false);
    assert_eq!(body["deleted"], false);
    assert_eq!(body["score"], 0);
    assert_eq!(body["comment_count"], 0);
    assert!(body["edited_at"].is_null());
    assert!(
        body["id"].as_str().is_some_and(|s| s.starts_with("c_")),
        "içerik id'si c_ önekiyle başlamalı: {body}"
    );

    // Tag'ler normalize edilmiş (lowercase) ve sıralı dönmeli.
    let tags: Vec<&str> = body["tags"]
        .as_array()
        .expect("tags dizi olmalı")
        .iter()
        .map(|v| v.as_str().expect("tag string olmalı"))
        .collect();
    assert_eq!(tags, vec!["actos", "rust"], "{body}");

    let location = headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("Location header'ı olmalı");
    assert_eq!(location, format!("/posts/{}", body["id"].as_str().unwrap()));
}

/// **Not:** `text::validate_tag_name` yalnızca zaten küçük harfli girdiyi
/// kabul ediyor (bkz. `crates/actos-core/src/text.rs` — kullanıcı adında
/// olduğu gibi büyük harf sessizce küçültülmüyor, format hatası olarak
/// reddediliyor). Yani API üzerinden iki farklı harf durumuyla aynı
/// etiketi göndermek zaten mümkün değil; bu test `tags.name`'in `citext`
/// (case-insensitive) olmasını değil, aynı normalize edilmiş etiketin
/// iki farklı post'ta **aynı satırı** paylaştığını (ikinci `INSERT`'in
/// `ON CONFLICT DO NOTHING` ile sessizce atlandığını) doğruluyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tag_tekrar_kullanimi_ayni_tag_satirini_paylasir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "tag_author").await;

    let (status, body1, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "Birinci", "body": "gövde bir", "tags": ["nvidia"] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body1}");

    let (status, body2, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "İkinci", "body": "gövde iki", "tags": ["nvidia"] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body2}");

    let tag_count: i64 =
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM tags WHERE name = 'nvidia'"#)
            .fetch_one(&raw_pool)
            .await
            .expect("tag sayısı sorgulanabilmeli");
    assert_eq!(
        tag_count, 1,
        "aynı etiket ikinci postta yeniden kullanılmalı, ikinci satır oluşmamalı"
    );

    let content_tag_count: i64 =
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM content_tags"#)
            .fetch_one(&raw_pool)
            .await
            .expect("content_tags sayısı sorgulanabilmeli");
    assert_eq!(
        content_tag_count, 2,
        "her iki post da aynı tag'e bağlanmalı"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn post_olusturma_kimliksiz_401_doner(pool: PgPool) {
    let router = build_router(pool);

    let (status, body, _) = send(
        &router,
        json_req(
            "POST",
            "/posts",
            json!({ "title": "başlık", "body": "gövde", "tags": [] }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn post_olusturma_bos_baslik_400_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "empty_title_author").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "", "body": "gövde", "tags": [] }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn post_olusturma_fazla_tag_400_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "too_many_tags_author").await;

    let tags: Vec<String> = (0..11).map(|i| format!("tag{i}")).collect();
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "başlık", "body": "gövde", "tags": tags }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

// --- GET /posts/{id} -----------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_canli_200_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "get_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "Okunacak post", "body": "içerik", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı");

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{id}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "Okunacak post");
    assert_eq!(body["author"]["username"], "get_author");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_olmayan_id_404_doner(pool: PgPool) {
    let router = build_router(pool);

    // Yapısal olarak geçerli ama var olmayan bir content id (a_ ile
    // başlayan bir actor id'sini c_ önekiyle taklit etmek yerine, hiç var
    // olmayan bir base62 gövdesi kullanıyoruz).
    let (status, body, _) = send(&router, empty_req("GET", "/posts/c_zzzzzzzzzzz")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "NOT_FOUND");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_bozuk_id_404_doner(pool: PgPool) {
    let router = build_router(pool);

    let (status, body, _) = send(&router, empty_req("GET", "/posts/boyle-bir-id-yok")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_post_govde_sizdirmadan_410_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "deleted_post_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({
                "title": "Gizli kalması gereken başlık",
                "body": "Gizli kalması gereken gövde",
                "tags": [],
            }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{id}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{id}"))).await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["code"], "GONE");

    let raw = body.to_string();
    assert!(
        !raw.contains("Gizli kalması gereken"),
        "silinmiş post'un başlık/gövdesi 410 yanıtına sızmamalı: {raw}"
    );
}

// --- PATCH /posts/{id} ---------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_post_sahibi_200_ve_edit_history_yaziliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "patch_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "Eski başlık", "body": "Eski gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();
    let internal_id: i64 =
        sqlx::query_scalar!(r#"SELECT id FROM contents WHERE title = 'Eski başlık'"#)
            .fetch_one(&raw_pool)
            .await
            .expect("post satırı bulunmalı");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/posts/{id}"),
            &api_key,
            json!({ "title": "Yeni başlık" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "Yeni başlık");
    assert_eq!(
        body["body"], "Eski gövde",
        "gönderilmeyen body alanına dokunulmamalı: {body}"
    );
    assert!(
        !body["edited_at"].is_null(),
        "edited_at set edilmeli: {body}"
    );

    let history = sqlx::query!(
        r#"SELECT previous_title, previous_body FROM edit_history WHERE content_id = $1"#,
        internal_id,
    )
    .fetch_all(&raw_pool)
    .await
    .expect("edit_history sorgulanabilmeli");
    assert_eq!(history.len(), 1, "tam olarak bir geçmiş satırı yazılmalı");
    assert_eq!(history[0].previous_title.as_deref(), Some("Eski başlık"));
    assert_eq!(history[0].previous_body, "Eski gövde");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_post_yabanci_403_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "patch_owner").await;
    let (_, stranger_key) = seed_actor(&raw_pool, "patch_stranger").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &owner_key,
            json!({ "title": "Sahibin postu", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/posts/{id}"),
            &stranger_key,
            json!({ "title": "Ele geçirme girişimi" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "FORBIDDEN");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_post_olmayan_id_404_silinmis_410_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "patch_notfound_author").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/posts/c_zzzzzzzzzzz",
            &api_key,
            json!({ "title": "fark etmez" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "silinecek", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{id}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/posts/{id}"),
            &api_key,
            json!({ "title": "fark etmez" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
}

// --- DELETE /posts/{id} ---------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn delete_post_sahibi_204_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "delete_owner").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "silinecek post", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{id}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn delete_post_moderator_204_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "mod_delete_owner").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "mod_delete_mod").await;
    core_auth::grant_role(&raw_pool, mod_id, AdminRole::Moderator, None)
        .await
        .expect("rol atanabilmeli");

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &owner_key,
            json!({ "title": "moderatör silecek", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{id}"), &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, _, _) = send(&router, empty_req("GET", &format!("/posts/{id}"))).await;
    assert_eq!(status, StatusCode::GONE);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn delete_post_yabanci_403_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, owner_key) = seed_actor(&raw_pool, "delete_owner_2").await;
    let (_, stranger_key) = seed_actor(&raw_pool, "delete_stranger").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &owner_key,
            json!({ "title": "yabancıdan korunacak", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{id}"), &stranger_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

// --- Silinmiş yazarın içeriği maskeleniyor ---------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_yazarin_postu_maskeli_gorunur(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (author_id, api_key) = seed_actor(&raw_pool, "soon_deleted_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "yazarı silinecek post", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    // Avatar gerçekten set edilmiş bir hesap silinsin ki aşağıdaki
    // `avatar_url` iddiası "zaten hep None'du" değil, gerçek bir maskeleme
    // sınasın (bkz. Faz 18.A — `crate::routes::posts::masked_actor_summary`
    // dokümanı: silinmiş bir hesabın avatarı görünmeye devam etmemeli).
    sqlx::query!(
        r#"UPDATE actors SET avatar_object_key = 'silinecek/avatar.webp' WHERE id = $1"#,
        author_id,
    )
    .execute(&raw_pool)
    .await
    .expect("avatar_object_key yazılabilmeli");

    // Actor'ü doğrudan domain katmanından soft-delete ediyoruz (kurtarma
    // kodu akışını burada tekrar test etmeye gerek yok, bkz.
    // `actors_api.rs`'teki ilgili test).
    sqlx::query!(
        r#"UPDATE actors SET deleted_at = now() WHERE id = $1"#,
        author_id,
    )
    .execute(&raw_pool)
    .await
    .expect("actor silinebilmeli");

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{id}"))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "post kendisi silinmedi, 200 dönmeli: {body}"
    );
    assert_eq!(body["author_deleted"], true);
    assert_eq!(body["author"]["username"], "[deleted]");
    assert!(body["author"]["display_name"].is_null());
    assert!(body["author"]["bio"].is_null());
    assert!(
        body["author"]["avatar_url"].is_null(),
        "silinmiş yazarın avatarı maskelenmeli: {body}"
    );
    // Post'un kendi gövdesi/başlığı yazarın silinmesinden etkilenmemeli.
    assert_eq!(body["title"], "yazarı silinecek post");
    assert_eq!(body["deleted"], false);
}

// --- Idempotency-Key -------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn idempotency_ikinci_istek_yeni_post_yaratmaz_ayni_yaniti_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "idem_author").await;

    let idem_key = "retry-key-1";
    let body = json!({ "title": "idempotent post", "body": "gövde", "tags": ["rust"] });

    let (status1, body1, headers1) = send(
        &router,
        auth_json_req_idem("POST", "/posts", &api_key, idem_key, body.clone()),
    )
    .await;
    assert_eq!(status1, StatusCode::CREATED, "{body1}");
    let location1 = headers1
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let (status2, body2, headers2) = send(
        &router,
        auth_json_req_idem("POST", "/posts", &api_key, idem_key, body),
    )
    .await;
    assert_eq!(status2, StatusCode::CREATED, "{body2}");
    assert_eq!(
        body1, body2,
        "aynı Idempotency-Key ile ikinci istek birincinin gövdesini aynen dönmeli"
    );
    let location2 = headers2
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    assert_eq!(location1, location2, "Location header'ı da aynı olmalı");

    let count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM contents WHERE title = 'idempotent post'"#
    )
    .fetch_one(&raw_pool)
    .await
    .expect("sayım sorgulanabilmeli");
    assert_eq!(count, 1, "ikinci istek yeni bir post oluşturmamalı");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn idempotency_farkli_actor_ayni_key_ayri_postlar_uretir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, key_a) = seed_actor(&raw_pool, "idem_actor_a").await;
    let (_, key_b) = seed_actor(&raw_pool, "idem_actor_b").await;

    // Bilerek AYNI Idempotency-Key — anahtar kapsamının actor'ü içerdiğini
    // doğruluyoruz (bkz. `actos_core::idempotency` modül dokümantasyonu
    // "Anahtar kapsamı" bölümü): iki farklı actor aynı değeri gönderse de
    // birbirinin postunu geri almamalı.
    let idem_key = "shared-key-across-actors";

    let (status_a, body_a, _) = send(
        &router,
        auth_json_req_idem(
            "POST",
            "/posts",
            &key_a,
            idem_key,
            json!({ "title": "A'nın postu", "body": "gövde a", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status_a, StatusCode::CREATED, "{body_a}");

    let (status_b, body_b, _) = send(
        &router,
        auth_json_req_idem(
            "POST",
            "/posts",
            &key_b,
            idem_key,
            json!({ "title": "B'nin postu", "body": "gövde b", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status_b, StatusCode::CREATED, "{body_b}");

    assert_ne!(
        body_a["id"], body_b["id"],
        "aynı Idempotency-Key farklı actor'lerin postlarını birbirine karıştırmamalı"
    );
    assert_eq!(body_a["title"], "A'nın postu");
    assert_eq!(body_b["title"], "B'nin postu");

    let count: i64 = sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM contents"#)
        .fetch_one(&raw_pool)
        .await
        .expect("sayım sorgulanabilmeli");
    assert_eq!(count, 2, "iki ayrı post oluşmalı");
}

/// Aynı anahtarla eşzamanlı iki istek: `tokio::join!` iki isteği aynı anda
/// başlatıyor, ikisi de Redis/Postgres'e giden `await` noktalarında
/// birbirine kesişiyor — bu, `SET NX` atomikliğinin gerçekten çalıştığını
/// (yalnızca biri yer tutucuyu koyabiliyor) doğrulayan gerçek bir yarış.
/// Asıl doğrulanan değişmez: sonuçta **tam olarak bir** post satırı
/// oluşuyor, iki isteğin hangisinin `Start`/`InProgress` aldığı
/// (zamanlamaya bağlı, deterministik değil) değil.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn idempotency_eszamanli_cift_istek_tek_post_uretir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "idem_concurrent_author").await;

    let idem_key = "concurrent-key-1";
    let body = json!({ "title": "eşzamanlı post", "body": "gövde", "tags": [] });

    let req1 = auth_json_req_idem("POST", "/posts", &api_key, idem_key, body.clone());
    let req2 = auth_json_req_idem("POST", "/posts", &api_key, idem_key, body);

    let (r1, r2) = tokio::join!(send(&router, req1), send(&router, req2));

    for (status, body, _) in [&r1, &r2] {
        assert!(
            *status == StatusCode::CREATED || *status == StatusCode::CONFLICT,
            "beklenmeyen durum kodu {status}: {body}"
        );
    }
    assert!(
        r1.0 == StatusCode::CREATED || r2.0 == StatusCode::CREATED,
        "en az bir istek başarıyla post oluşturmalı: {:?} / {:?}",
        r1.0,
        r2.0
    );
    if r1.0 == StatusCode::CREATED && r2.0 == StatusCode::CREATED {
        assert_eq!(r1.1, r2.1, "iki 201 aynı gövdeyi taşımalı");
    }

    let count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM contents WHERE title = 'eşzamanlı post'"#
    )
    .fetch_one(&raw_pool)
    .await
    .expect("sayım sorgulanabilmeli");
    assert_eq!(
        count, 1,
        "eşzamanlı iki istek yalnızca bir post satırı üretmeli"
    );
}

// --- Alan seçimi (`?fields=`) ----------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_fields_istenen_alanlari_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "fields_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "alan seçimi", "body": "gövde", "tags": ["rust"] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        empty_req("GET", &format!("/posts/{id}?fields=id,title,score")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let obj = body.as_object().expect("nesne olmalı");
    assert_eq!(obj.len(), 3, "yalnızca istenen 3 alan olmalı: {body}");
    assert_eq!(obj["id"], id);
    assert_eq!(obj["title"], "alan seçimi");
    assert_eq!(obj["score"], 0);
    assert!(
        !obj.contains_key("body"),
        "istenmeyen alan sızmamalı: {body}"
    );
    assert!(
        !obj.contains_key("tags"),
        "istenmeyen alan sızmamalı: {body}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_fields_gecersiz_alan_400_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "fields_invalid_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "geçersiz alan", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        empty_req("GET", &format!("/posts/{id}?fields=id,boyle_bir_alan_yok")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

// --- GET /actors/{username}/posts (Faz 7'den devir) ------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_postlari_cursorlu_ikinci_sayfa_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "list_posts_author").await;

    for i in 0..3 {
        let (status, body, _) = send(
            &router,
            auth_json_req(
                "POST",
                "/posts",
                &api_key,
                json!({ "title": format!("post {i}"), "body": "gövde", "tags": [] }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/list_posts_author/posts?limit=2"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let posts = body["posts"].as_array().expect("posts dizi olmalı");
    assert_eq!(posts.len(), 2, "{body}");
    // En yeni önce: son eklenen "post 2" ilk sayfada olmalı.
    assert_eq!(posts[0]["title"], "post 2", "{body}");
    assert_eq!(posts[1]["title"], "post 1", "{body}");
    let next_cursor = body["next_cursor"]
        .as_str()
        .expect("ilk sayfada sonraki cursor olmalı")
        .to_owned();

    let (status2, body2, _) = send(
        &router,
        empty_req(
            "GET",
            &format!("/actors/list_posts_author/posts?limit=2&cursor={next_cursor}"),
        ),
    )
    .await;
    assert_eq!(status2, StatusCode::OK, "{body2}");
    let posts2 = body2["posts"].as_array().expect("posts dizi olmalı");
    assert_eq!(posts2.len(), 1, "{body2}");
    assert_eq!(posts2[0]["title"], "post 0", "{body2}");
    assert!(
        body2["next_cursor"].is_null(),
        "ikinci sayfa son sayfa olmalı: {body2}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_postlari_silinmis_post_listede_gorunmuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "list_deleted_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "silinecek", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "kalacak", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{id}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/list_deleted_author/posts"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let posts = body["posts"].as_array().expect("posts dizi olmalı");
    assert_eq!(posts.len(), 1, "silinmiş post listede görünmemeli: {body}");
    assert_eq!(posts[0]["title"], "kalacak");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_postlari_silinmis_actor_410_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (author_id, _) = seed_actor(&raw_pool, "list_gone_author").await;

    sqlx::query!(
        r#"UPDATE actors SET deleted_at = now() WHERE id = $1"#,
        author_id,
    )
    .execute(&raw_pool)
    .await
    .expect("actor silinebilmeli");

    let (status, body, _) = send(&router, empty_req("GET", "/actors/list_gone_author/posts")).await;
    assert_eq!(status, StatusCode::GONE, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_postlari_fields_listede_ogelere_uygulaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "list_fields_author").await;

    let (status, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "alan filtreli", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/list_fields_author/posts?fields=id,title"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let posts = body["posts"].as_array().expect("posts dizi olmalı");
    assert_eq!(posts.len(), 1, "{body}");
    let obj = posts[0].as_object().expect("öğe nesne olmalı");
    assert_eq!(obj.len(), 2, "yalnızca istenen 2 alan olmalı: {body}");
    assert!(obj.contains_key("id"));
    assert!(obj.contains_key("title"));
    // Sarmalayıcı (`next_cursor`) filtreden etkilenmemeli — bkz.
    // `crate::fields` modül dokümantasyonu "Liste yanıtlarında" bölümü.
    assert!(
        body.get("next_cursor").is_some(),
        "sarmalayıcı alanı hâlâ orada olmalı: {body}"
    );
}

// --- `body_html` (Faz 18.A, bkz. NOTES.md §8.3) -----------------------------

/// `tests/comments_api.rs`'teki `test_id_codec` ile birebir aynı desen —
/// ayrı bir entegrasyon test binary'si olduğu için yeniden tanımlanıyor
/// (bkz. dosya başı doküman yorumu).
#[allow(clippy::expect_used)]
fn test_id_codec() -> IdCodec {
    IdCodec::new(&test_config().security.id_obfuscation_key).expect("geçerli anahtar")
}

/// `text.rs`'teki `script_etiketi_çıktıda_yok` / `javascript_şemalı_link_reddediliyor`
/// birim testlerinin uç üzerinden de doğrulanması: `render_markdown`
/// üretimde hiçbir yerden çağrılmıyordu (bkz. görev tanımı kök nedeni) —
/// bu test artık `GET /posts/{id}`'in gerçekten sanitize edilmiş HTML
/// döndürdüğünü kanıtlıyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_body_html_xss_temizleniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "xss_author").await;

    let gövde = "zararlı <script>alert(1)</script> ve [tıkla](javascript:alert(1))";
    let (status, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "xss testi", "body": gövde, "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{id}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Ham `body` sanitize edilmemiş kalmalı — sanitizasyon `body_html`'e
    // özgü, `body` istemcinin gönderdiğini aynen taşımaya devam ediyor.
    assert_eq!(body["body"], gövde, "{body}");

    let html = body["body_html"].as_str().expect("body_html string olmalı");
    assert!(
        !html.contains("<script"),
        "script etiketi body_html'e sızmamalı: {html}"
    );
    assert!(
        !html.contains("javascript:"),
        "javascript: şeması body_html'e sızmamalı: {html}"
    );
    assert!(html.contains("zararlı"), "zararsız metin korunmalı: {html}");
}

/// `?fields=` `body_html`'ten bağımsız hesaplansa da (tekil uç, bkz. görev
/// tanımı madde 4), `apply_fields` yine de yalnızca istenen anahtarı
/// bırakmalı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_fields_body_html_secilebilir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "body_html_fields_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "başlık", "body": "**kalın** metin", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        empty_req("GET", &format!("/posts/{id}?fields=body_html")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let obj = body.as_object().expect("nesne olmalı");
    assert_eq!(obj.len(), 1, "yalnızca body_html olmalı: {body}");
    let html = obj["body_html"].as_str().expect("string olmalı");
    assert!(html.contains("<strong>kalın</strong>"), "{html}");
}

/// Tekil uç: `body_html` `?fields=` hiç verilmese de her zaman dolu döner
/// (bkz. `actos_types::content::ContentSummary::body_html` "Nerede dolu
/// döner").
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_body_html_fields_olmadan_da_dolu(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "body_html_default_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "başlık", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{id}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["body_html"].is_string(),
        "?fields= verilmeden de body_html dolu olmalı: {body}"
    );
}

/// `body_format == "plain"` iken markdown render EDİLMEZ — yalnızca
/// HTML-escape edilir (bkz. görev tanımı madde 3). Bugünkü API `POST
/// /posts` ile yalnızca `markdown` üretebiliyor (bkz.
/// `actos_core::content::create_post`'un sabit `'markdown'::body_format`'ı),
/// bu yüzden `plain`'i doğrudan veritabanında kuruyoruz.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_post_body_html_plain_formatta_markdown_render_edilmiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let codec = test_id_codec();
    let (_, api_key) = seed_actor(&raw_pool, "plain_author").await;

    let (_, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "düz metin", "body": "*yıldız* düz kalmalı", "tags": [] }),
        ),
    )
    .await;
    let id = created["id"].as_str().expect("id olmalı").to_owned();

    let internal_id = codec
        .decode::<actos_core::id::Content>(&id)
        .expect("id çözülebilmeli");
    sqlx::query!(
        r#"UPDATE contents SET body_format = 'plain'::body_format WHERE id = $1"#,
        internal_id,
    )
    .execute(&raw_pool)
    .await
    .expect("body_format yazılabilmeli");

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{id}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["body_format"], "plain", "{body}");

    let html = body["body_html"].as_str().expect("body_html string olmalı");
    assert!(
        !html.contains("<em>"),
        "plain formatta markdown render edilmemeli (yıldızlar italik olmamalı): {html}"
    );
    assert!(
        html.contains("*yıldız*"),
        "yıldızlar olduğu gibi (escape edilmiş) kalmalı: {html}"
    );
}

/// Liste uçlarında `body_html` varsayılan olarak hesaplanmaz — gövde
/// boyutu 25 katına çıkmasın diye (görev tanımı madde 4).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_postlari_body_html_varsayilan_hesaplanmiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "list_body_html_author").await;

    let (status, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "liste testi", "body": "**kalın**", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/list_body_html_author/posts"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let posts = body["posts"].as_array().expect("posts dizi olmalı");
    assert_eq!(posts.len(), 1, "{body}");
    assert!(
        posts[0]["body_html"].is_null(),
        "?fields= ile açıkça istenmeden liste öğesinde body_html hesaplanmamalı: {body}"
    );
}

/// `?fields=body_html` liste uçlarında da hesaplamayı açık şekilde tetikler.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_postlari_fields_body_html_ile_hesaplaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "list_body_html_fields_author").await;

    let (status, created, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &api_key,
            json!({ "title": "liste testi", "body": "**kalın**", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, body, _) = send(
        &router,
        empty_req(
            "GET",
            "/actors/list_body_html_fields_author/posts?fields=id,body_html",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let posts = body["posts"].as_array().expect("posts dizi olmalı");
    assert_eq!(posts.len(), 1, "{body}");
    let html = posts[0]["body_html"]
        .as_str()
        .expect("?fields=body_html ile string dönmeli");
    assert!(html.contains("<strong>kalın</strong>"), "{html}");
}
