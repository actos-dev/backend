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
    },
    cursor::CursorCodec,
    id::IdCodec,
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
    let rate_limiter = RateLimiter::with_prefix(
        redis.clone(),
        config.rate_limits,
        format!("test:{}:", uuid::Uuid::new_v4()),
    );

    let state = AppState::new(
        config,
        pool,
        redis,
        storage,
        id_codec,
        cursor_codec,
        rate_limiter,
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
    assert_eq!(body["author"]["username"], "[silindi]");
    assert!(body["author"]["display_name"].is_null());
    assert!(body["author"]["bio"].is_null());
    // Post'un kendi gövdesi/başlığı yazarın silinmesinden etkilenmemeli.
    assert_eq!(body["title"], "yazarı silinecek post");
    assert_eq!(body["deleted"], false);
}
