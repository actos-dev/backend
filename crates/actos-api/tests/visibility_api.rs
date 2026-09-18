//! Görünürlük kapısı (COMMUNITY_PLAN.md Faz 4A) uçtan uca testleri.
//!
//! **Özel topluluk nasıl kuruluyor:** API, Faz 4A boyunca `private`
//! oluşturmayı reddediyor (`community::create_community`). Bu dosyadaki
//! [`create_private_community`] önce public bir topluluk açıp ardından
//! `visibility`'yi doğrudan bir `UPDATE` ile `private`'a çeviriyor. 4B
//! anahtarı çevirdiğinde bu yardımcı, API çağrısının kendisini kullanacak
//! şekilde sadeleşecek; o zamana kadar kapının doğruluğunu test etmenin tek
//! yolu bu.
//!
//! Kurulum yardımcıları `tests/communities_api.rs` ile aynı desen — Rust her
//! `tests/*.rs` dosyasını bağımsız derlediği için paylaşılan bir modül
//! olmadan tekrar tanımlanıyor.

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

// --- Kurulum yardımcıları (bkz. `tests/communities_api.rs`) --------------

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
fn empty_req(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("istek kurulabilmeli")
}

/// `/auth/register`'ın hız sınırını görmeden doğrudan domain katmanından bir
/// actor oluşturur, döner: `(actor_id, api_key)`.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> (i64, String) {
    let reg = core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    (reg.actor.id, reg.api_key)
}

/// Public bir topluluk açar (API `private`'ı reddettiği için tek yol).
#[allow(clippy::expect_used)]
async fn create_community(router: &Router, token: &str, name: &str) {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            "/communities",
            token,
            json!({ "name": name, "description": "açıklama" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "topluluk açılamadı: {body}");
}

/// Açar ve `visibility`'yi doğrudan SQL ile `private`'a çevirir — gerekçe
/// dosya başındaki modül dokümanı.
#[allow(clippy::expect_used)]
async fn create_private_community(router: &Router, pool: &PgPool, token: &str, name: &str) -> i64 {
    create_community(router, token, name).await;
    sqlx::query!(
        r#"UPDATE communities
           SET visibility = 'private'::community_visibility
           WHERE name = $1"#,
        name,
    )
    .execute(pool)
    .await
    .expect("topluluk private'a çevrilebilmeli");
    community_id(pool, name).await
}

#[allow(clippy::expect_used)]
async fn community_id(pool: &PgPool, name: &str) -> i64 {
    core_community::resolve_id_by_name(pool, name)
        .await
        .expect("topluluk bulunabilmeli")
}

/// Özel topluluğa doğrudan SQL ile üye ekler: `join` ucu private'ı
/// reddediyor (Faz 4B'nin davet/başvuru akışı), ama kapıyı test etmek için
/// üyelik şart.
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

/// `POST /posts`; `community` verilirse topluluk postu. Gövdeyi döner.
#[allow(clippy::expect_used)]
async fn create_post(
    router: &Router,
    token: &str,
    title: &str,
    body: &str,
    tags: &[&str],
    community: Option<&str>,
) -> Value {
    let mut payload = json!({ "title": title, "body": body, "tags": tags });
    if let Some(community) = community {
        payload["community"] = json!(community);
    }
    let (status, body, _) = send(router, auth_json_req("POST", "/posts", token, payload)).await;
    assert_eq!(status, StatusCode::CREATED, "post açılamadı: {body}");
    body
}

/// Bir post'a yorum ekler; gövdeyi döner.
#[allow(clippy::expect_used)]
async fn create_comment(router: &Router, token: &str, post_id: &str, body: &str) -> Value {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            token,
            json!({ "body": body }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "yorum açılamadı: {body}");
    body
}

fn strings_in(body: &Value, array_key: &str, field: &str) -> Vec<String> {
    body[array_key]
        .as_array()
        .unwrap_or_else(|| panic!("{array_key} dizi olmalı: {body}"))
        .iter()
        .map(|item| item[field].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn titles(body: &Value, array_key: &str) -> Vec<String> {
    strings_in(body, array_key, "title")
}

fn bodies(body: &Value, array_key: &str) -> Vec<String> {
    strings_in(body, array_key, "body")
}

/// `GET /posts/{id}/comments` düz bir liste değil, her öğesi
/// `ContentSummary`'nin alanları (`#[serde(flatten)]`) + `replies` olan bir
/// ağaç döner.
fn comment_node_bodies(body: &Value) -> Vec<String> {
    body["comments"]
        .as_array()
        .unwrap_or_else(|| panic!("comments dizi olmalı: {body}"))
        .iter()
        .map(|node| node["body"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn assert_has(actual: &[String], expected: &str, label: &str) {
    assert!(
        actual.iter().any(|value| value == expected),
        "{label}: beklenen {expected:?} bulunamadı, gelen: {actual:?}"
    );
}

fn assert_lacks(actual: &[String], forbidden: &str, label: &str) {
    assert!(
        !actual.iter().any(|value| value == forbidden),
        "{label}: özel içerik {forbidden:?} görünmemeli, gelen: {actual:?}"
    );
}

// --- Public yüzeyler: özel içerik kimseye görünmez ------------------------

/// Ana akış, takip akışı, arama, etiket sayfası, profilin post/yorum
/// listeleri ve profil istatistikleri özel içeriği **hiç kimseye** —
/// üyeye bile — göstermez (COMMUNITY_PLAN.md §9).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn public_yuzeyler_ozel_icerigi_gostermiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "pub_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "pub_member").await;
    let (_, outsider_key) = seed_actor(&raw_pool, "pub_outsider").await;

    create_private_community(&router, &raw_pool, &owner_key, "pub_gizli").await;
    add_member(
        &raw_pool,
        community_id(&raw_pool, "pub_gizli").await,
        member_id,
    )
    .await;

    // Public bağımsız post/yorum (kontrol grubu).
    let public_post =
        create_post(&router, &owner_key, "public_post", "açık gövde", &[], None).await;
    let public_post_id = public_post["id"].as_str().expect("id").to_owned();
    create_comment(&router, &owner_key, &public_post_id, "public_comment").await;

    // Özel topluluk postu/yorumu — benzersiz arama/etiket belirteçleriyle.
    let private_post = create_post(
        &router,
        &owner_key,
        "zzprivate_post",
        "gizli gövde",
        &["zzprivatetag"],
        Some("pub_gizli"),
    )
    .await;
    let private_post_id = private_post["id"].as_str().expect("id").to_owned();
    create_comment(&router, &owner_key, &private_post_id, "zzprivate_comment").await;

    // member üye olduğu hâlde public yüzeyler yine de dışlar.
    for (label, token) in [
        ("member", Some(member_key.as_str())),
        ("outsider", Some(outsider_key.as_str())),
        ("anonymous", None),
    ] {
        // Ana akış.
        let req = match token {
            Some(token) => auth_req("GET", "/feed", token),
            None => empty_req("GET", "/feed"),
        };
        let (status, body, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::OK, "feed {label}: {body}");
        assert_lacks(
            &titles(&body, "posts"),
            "zzprivate_post",
            &format!("feed/{label}"),
        );

        // Arama.
        let req = match token {
            Some(token) => auth_req("GET", "/search?q=zzprivate&type=post", token),
            None => empty_req("GET", "/search?q=zzprivate&type=post"),
        };
        let (status, body, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::OK, "search {label}: {body}");
        assert!(
            strings_in(&body, "results", "title").is_empty(),
            "search/{label}: özel post sonuçta olmamalı: {body}"
        );

        // Etiket sayfası.
        let req = match token {
            Some(token) => auth_req("GET", "/tags/zzprivatetag/posts", token),
            None => empty_req("GET", "/tags/zzprivatetag/posts"),
        };
        let (status, body, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::OK, "tag {label}: {body}");
        assert_lacks(
            &titles(&body, "posts"),
            "zzprivate_post",
            &format!("tag/{label}"),
        );

        // Etiket otomatik tamamlama (ek yol).
        let (status, body, _) =
            send(&router, empty_req("GET", "/tags/search?q=zzprivatetag")).await;
        assert_eq!(status, StatusCode::OK, "tag search {label}: {body}");
        assert!(
            strings_in(&body, "tags", "name").is_empty(),
            "tag-search/{label}: yalnızca özel postta geçen etiket önerilmemeli: {body}"
        );

        // Profil post listesi.
        let req = match token {
            Some(token) => auth_req("GET", "/actors/pub_owner/posts", token),
            None => empty_req("GET", "/actors/pub_owner/posts"),
        };
        let (status, body, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::OK, "profile posts {label}: {body}");
        assert_has(
            &titles(&body, "posts"),
            "public_post",
            &format!("profile-posts/{label}"),
        );
        assert_lacks(
            &titles(&body, "posts"),
            "zzprivate_post",
            &format!("profile-posts/{label}"),
        );

        // Profil yorum listesi.
        let req = match token {
            Some(token) => auth_req("GET", "/actors/pub_owner/comments", token),
            None => empty_req("GET", "/actors/pub_owner/comments"),
        };
        let (status, body, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::OK, "profile comments {label}: {body}");
        assert_has(
            &bodies(&body, "comments"),
            "public_comment",
            &format!("profile-comments/{label}"),
        );
        assert_lacks(
            &bodies(&body, "comments"),
            "zzprivate_comment",
            &format!("profile-comments/{label}"),
        );

        // Profil istatistikleri: yalnızca public içerik sayılır.
        let req = match token {
            Some(token) => auth_req("GET", "/actors/pub_owner", token),
            None => empty_req("GET", "/actors/pub_owner"),
        };
        let (status, body, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::OK, "profile stats {label}: {body}");
        assert_eq!(body["stats"]["post_count"], 1, "stats/{label}: {body}");
        assert_eq!(body["stats"]["comment_count"], 1, "stats/{label}: {body}");
        assert_eq!(body["stats"]["total_score"], 0, "stats/{label}: {body}");
    }

    // Takip akışı: member, owner'ı takip etse bile özel post görünmez.
    let (status, body, _) = send(
        &router,
        auth_req("PUT", "/actors/pub_owner/follow", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/feed/following", &member_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_lacks(
        &titles(&body, "posts"),
        "zzprivate_post",
        "following/member",
    );
}

// --- Tekil okuma: üye görür, üye olmayan 404 -------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tekil_post_uye_200_uye_olmayan_404(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "one_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "one_member").await;
    let (_, outsider_key) = seed_actor(&raw_pool, "one_outsider").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "one_gizli").await;
    add_member(&raw_pool, community, member_id).await;

    let post = create_post(
        &router,
        &owner_key,
        "gizli",
        "gövde",
        &[],
        Some("one_gizli"),
    )
    .await;
    let post_id = post["id"].as_str().expect("id");

    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post_id}"), &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "üye okuyabilmeli: {body}");

    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post_id}"), &outsider_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "üye olmayan 404 almalı: {body}"
    );

    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{post_id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "anonim 404 almalı: {body}");
}

// --- Yorum ağacı / tekil yorum --------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yorum_agaci_ve_tekil_yorum_uyelik_gerektirir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "cmt_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "cmt_member").await;
    let (_, outsider_key) = seed_actor(&raw_pool, "cmt_outsider").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "cmt_gizli").await;
    add_member(&raw_pool, community, member_id).await;

    let post = create_post(
        &router,
        &owner_key,
        "gizli",
        "gövde",
        &[],
        Some("cmt_gizli"),
    )
    .await;
    let post_id = post["id"].as_str().expect("id").to_owned();
    let comment = create_comment(&router, &owner_key, &post_id, "gizli yorum").await;
    let comment_id = comment["id"].as_str().expect("id").to_owned();

    // Ağaç: üye 200, üye olmayan 404.
    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post_id}/comments"), &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "üye ağacı okuyabilmeli: {body}");
    assert_has(&comment_node_bodies(&body), "gizli yorum", "tree/member");

    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post_id}/comments"), &outsider_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "üye olmayan ağaçta 404: {body}"
    );

    // Tekil yorum: üye 200, üye olmayan 404.
    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/comments/{comment_id}"), &member_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "üye tekil yorumu okuyabilmeli: {body}"
    );

    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/comments/{comment_id}"), &outsider_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "üye olmayan tekil yorumda 404: {body}"
    );
}

// --- Kaydedilenler --------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kayitlar_uyelik_bitince_ozel_icerigi_gizliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "save_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "save_member").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "save_gizli").await;
    add_member(&raw_pool, community, member_id).await;

    let post = create_post(
        &router,
        &owner_key,
        "gizli",
        "gövde",
        &[],
        Some("save_gizli"),
    )
    .await;
    let post_id = post["id"].as_str().expect("id").to_owned();

    // Üye kaydediyor.
    let (status, body, _) = send(
        &router,
        auth_req("PUT", &format!("/contents/{post_id}/save"), &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/me/saves", &member_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        titles(&body, "saves").len(),
        1,
        "üye kaydını görmeli: {body}"
    );

    // Üyelikten ayrılınca kayıt listeden düşer (§9).
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/save_gizli/join", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/me/saves", &member_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        strings_in(&body, "saves", "id").is_empty(),
        "üyelik bitince kayıt gizlenmeli: {body}"
    );
}

// --- Gelen kutusu ---------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gelen_kutusu_uyelik_bitince_bildirimi_gizliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "inbox_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "inbox_member").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "inbox_gizli").await;
    add_member(&raw_pool, community, member_id).await;

    // Üye özel toplulukta post açıyor; sahip ona yorum yapıyor →
    // bildirimin hedefi özel topluluktaki bir içerik.
    let post = create_post(
        &router,
        &member_key,
        "gizli",
        "gövde",
        &[],
        Some("inbox_gizli"),
    )
    .await;
    let post_id = post["id"].as_str().expect("id");
    create_comment(&router, &owner_key, post_id, "yanıt").await;

    let (status, body, _) = send(&router, auth_req("GET", "/me/inbox", &member_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body["notifications"].as_array().expect("dizi").is_empty(),
        "üye bildirimi görmeli: {body}"
    );
    assert!(
        body["unread_count"].as_i64().expect("sayı") >= 1,
        "okunmamış sayısı en az bir olmalı: {body}"
    );

    // Üyelikten ayrılınca bildirim ve sayaç birlikte gizlenir.
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/inbox_gizli/join", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/me/inbox", &member_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["notifications"].as_array().expect("dizi").is_empty(),
        "üyelik bitince bildirim gizlenmeli: {body}"
    );
    assert_eq!(
        body["unread_count"], 0,
        "sayaç da gizli bildirimi saymamalı: {body}"
    );
}

// --- Oy geçmişi -----------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn oy_gecmisi_uyelik_bitince_ozel_icerigi_dondurmuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "vote_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "vote_member").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "vote_gizli").await;
    add_member(&raw_pool, community, member_id).await;

    let post = create_post(
        &router,
        &owner_key,
        "gizli",
        "gövde",
        &[],
        Some("vote_gizli"),
    )
    .await;
    let post_id = post["id"].as_str().expect("id").to_owned();

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{post_id}/vote"),
            &member_key,
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "üye oy verebilmeli: {body}");

    let uri = format!("/me/votes?content_ids={post_id}");
    let (status, body, _) = send(&router, auth_req("GET", &uri, &member_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["votes"][post_id.as_str()],
        1,
        "üye kendi oyunu görmeli: {body}"
    );

    // Üyelik bitince oy değeri de dönmez.
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/vote_gizli/join", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", &uri, &member_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["votes"].as_object().expect("nesne").is_empty(),
        "üyelik bitince oy görünmemeli: {body}"
    );
}

// --- Yazma hedefleri: görünmeyen hedef 404 --------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn uye_olmayan_ozel_icerige_yazamaz_404(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "w_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "w_member").await;
    let (_, outsider_key) = seed_actor(&raw_pool, "w_outsider").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "w_gizli").await;
    add_member(&raw_pool, community, member_id).await;

    let post = create_post(&router, &owner_key, "gizli", "gövde", &[], Some("w_gizli")).await;
    let post_id = post["id"].as_str().expect("id").to_owned();
    let comment = create_comment(&router, &owner_key, &post_id, "gizli yorum").await;
    let comment_id = comment["id"].as_str().expect("id").to_owned();

    // Kaydetme.
    for target in [&post_id, &comment_id] {
        let (status, body, _) = send(
            &router,
            auth_req("PUT", &format!("/contents/{target}/save"), &outsider_key),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "save {target}: {body}");
    }

    // Oy.
    for target in [&post_id, &comment_id] {
        let (status, body, _) = send(
            &router,
            auth_json_req(
                "PUT",
                &format!("/contents/{target}/vote"),
                &outsider_key,
                json!({ "value": 1 }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "vote {target}: {body}");
    }

    // Şikayet.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/reports",
            &outsider_key,
            json!({ "target_type": "post", "target_id": post_id, "reason": "x" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "report: {body}");

    // Yorum.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            &outsider_key,
            json!({ "body": "olmamalı" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "comment: {body}");

    // Kontrol: üye aynı hedeflere yazabilir.
    let (status, body, _) = send(
        &router,
        auth_req("PUT", &format!("/contents/{post_id}/save"), &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "üye kaydedebilmeli: {body}");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{post_id}/vote"),
            &member_key,
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "üye oy verebilmeli: {body}");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            &member_key,
            json!({ "body": "üye yorumu" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "üye yorum yazabilmeli: {body}");
}

// --- Güncelleme: üyelik bitince hedef de görünmez -------------------------

/// Yazma lookup'ı da kapıdan geçer: üyeliği biten bir yazar kendi özel
/// içeriğini düzenlemeye çalışırsa `404` alır ve **hiçbir değişiklik
/// uygulanmaz** (önce lookup, sonra mutasyon).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn guncelleme_uyelik_bitince_404_ve_degisiklik_yok(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "upd_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "upd_member").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "upd_gizli").await;
    add_member(&raw_pool, community, member_id).await;

    // Üye kendi özel postunu açıp üyeyken düzenleyebiliyor.
    let post = create_post(
        &router,
        &member_key,
        "eski",
        "gövde",
        &[],
        Some("upd_gizli"),
    )
    .await;
    let post_id = post["id"].as_str().expect("id").to_owned();

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/posts/{post_id}"),
            &member_key,
            json!({ "title": "yeni" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "üye düzenleyebilmeli: {body}");
    assert_eq!(body["title"], "yeni", "{body}");

    // Üyelikten ayrılınca düzenleme 404 ve gövde değişmemeli.
    let (status, body, _) = send(
        &router,
        auth_req("DELETE", "/communities/upd_gizli/join", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/posts/{post_id}"),
            &member_key,
            json!({ "title": "olmamalı" }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "üyelik bitince düzenleme 404: {body}"
    );

    // Sahip (üye) içeriği okuyup değişmediğini doğruluyor.
    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post_id}"), &owner_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["title"], "yeni",
        "başarısız PATCH uygulanmamalı: {body}"
    );
}

// --- Topluluk sayfası -----------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ozel_topluluk_sayfasi_uye_olmayana_403(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "cp_owner").await;
    let (member_id, member_key) = seed_actor(&raw_pool, "cp_member").await;
    let (_, outsider_key) = seed_actor(&raw_pool, "cp_outsider").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "cp_gizli").await;
    add_member(&raw_pool, community, member_id).await;
    create_post(&router, &owner_key, "gizli", "gövde", &[], Some("cp_gizli")).await;

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/cp_gizli/posts", &outsider_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "üye olmayan post listesi: {body}"
    );

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/cp_gizli/posts", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "üye post listesi: {body}");
    assert_has(&titles(&body, "posts"), "gizli", "community-posts/member");

    // Üye listesi de public değil (ek yol).
    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/cp_gizli/members", &outsider_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "üye olmayan üye listesi: {body}"
    );

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/cp_gizli/members", &member_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "üye üye listesi: {body}");
}

// --- Kapsamlı izin sahibi görebilir ---------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_kapsamli_izin_sahibi_ozel_icerigi_gorur(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "mod_owner").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "mod_moderator").await;

    let community = create_private_community(&router, &raw_pool, &owner_key, "mod_gizli").await;
    grant_scoped(&raw_pool, mod_id, Permission::ContentDelete, community).await;

    let post = create_post(
        &router,
        &owner_key,
        "gizli",
        "gövde",
        &[],
        Some("mod_gizli"),
    )
    .await;
    let post_id = post["id"].as_str().expect("id").to_owned();
    let comment = create_comment(&router, &owner_key, &post_id, "gizli yorum").await;
    let comment_id = comment["id"].as_str().expect("id").to_owned();

    // Topluluk kapsamlı izni olan moderatör tekil okuma yapabilir.
    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post_id}"), &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "moderatör postu görmeli: {body}");

    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/comments/{comment_id}"), &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "moderatör yorumu görmeli: {body}");

    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post_id}/comments"), &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "moderatör ağacı görmeli: {body}");

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/mod_gizli/posts", &mod_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "moderatör topluluk akışını görmeli: {body}"
    );

    // Public yüzeyler moderatöre de özel içerik göstermez (§9).
    let (status, body, _) = send(
        &router,
        auth_req("GET", "/actors/mod_owner/posts", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_lacks(&titles(&body, "posts"), "gizli", "profile/mod");
}

/// Platform geneli bir moderatör (`content.delete` global) şikayet edilen
/// özel içeriği tekil okumada ve topluluk akışında görebilir (§7), ama
/// public yüzeylerde yine görmez (§9: ana akış/profil `'{}'` geçer).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn global_moderator_ozel_icerigi_tekil_okumada_gorur(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, owner_key) = seed_actor(&raw_pool, "gm_owner").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "gm_moderator").await;

    let _community = create_private_community(&router, &raw_pool, &owner_key, "gm_gizli").await;

    core_auth::grant_permission(
        &raw_pool,
        mod_id,
        Permission::ContentDelete,
        PermissionScope::Global,
        None,
        None,
    )
    .await
    .expect("global izin verilebilmeli");

    let post = create_post(&router, &owner_key, "gizli", "gövde", &[], Some("gm_gizli")).await;
    let post_id = post["id"].as_str().expect("id").to_owned();

    let (status, body, _) = send(
        &router,
        auth_req("GET", &format!("/posts/{post_id}"), &mod_key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "global moderatör görebilmeli: {body}"
    );

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/communities/gm_gizli/posts", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Public yüzey: global moderatör bile profilde özel içeriği görmez.
    let (status, body, _) =
        send(&router, auth_req("GET", "/actors/gm_owner/posts", &mod_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_lacks(&titles(&body, "posts"), "gizli", "profil/global_mod");
}
