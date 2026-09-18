//! `crates/actos-api/routes/admin.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/posts_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! Odak: **yetki sınırları**. Planın açık maddesi "yetkisiz erişim her
//! admin endpoint'inde 403 mü" burada tablo hâlinde sınanıyor
//! (`yetkisiz_erisim_tum_admin_uclarinda_403`), ayrıca banlı actor'ün
//! yazamayıp okuyabildiği ve her admin eyleminin denetim izine düştüğü
//! doğrulanıyor.

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

/// Bir actor'e tek bir global izin verir.
#[allow(clippy::expect_used)]
async fn izin_ver(pool: &PgPool, actor_id: i64, permission: Permission) {
    core_auth::grant_permission(
        pool,
        actor_id,
        permission,
        PermissionScope::Global,
        None,
        None,
    )
    .await
    .expect("izin verilebilmeli");
}

/// Eski rol modelinin izin kümesi — moderatör beş, admin sekiz global izin
/// tutuyordu (bkz. `migrations/0028_permissions.up.sql` veri göçü).
#[allow(clippy::expect_used)]
async fn rol_ver(pool: &PgPool, actor_id: i64, rol: &str) {
    let izinler: &[Permission] = match rol {
        "moderator" => &[
            Permission::ContentDelete,
            Permission::MemberBan,
            Permission::ReportView,
            Permission::ReportResolve,
            Permission::AuditView,
        ],
        "admin" => &[
            Permission::ContentDelete,
            Permission::CommunityEdit,
            Permission::CommunityClose,
            Permission::MemberBan,
            Permission::RoleGrant,
            Permission::ReportView,
            Permission::ReportResolve,
            Permission::AuditView,
        ],
        other => panic!("bilinmeyen rol: {other}"),
    };
    for permission in izinler {
        izin_ver(pool, actor_id, *permission).await;
    }
}

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

/// Denetim izindeki kayıt sayısı.
#[allow(clippy::expect_used)]
async fn iz_sayisi(pool: &PgPool) -> i64 {
    sqlx::query_scalar!(r#"SELECT COUNT(*) AS "n!" FROM admin_actions_log"#)
        .fetch_one(pool)
        .await
        .expect("iz sayılabilmeli")
}

// --- POST /reports ---------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sikayet_201_ve_ayni_hedef_ikinci_kez_409(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "sikayet_yazar").await;
    let (_, sikayetci_key) = seed_actor(&raw_pool, "sikayetci").await;
    let post = seed_post(&router, &yazar_key, "şikayet edilecek").await;

    let istek = json!({ "target_type": "post", "target_id": post, "reason": "spam" });

    let (status, body, _) = send(
        &router,
        auth_json_req("POST", "/reports", &sikayetci_key, istek.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["status"], "pending", "{body}");
    assert_eq!(body["target_type"], "post", "{body}");
    assert_eq!(body["target_id"], post, "{body}");

    // Şikayet edenin kimliği yanıtta olmamalı.
    assert!(
        body.get("reporter_actor_id").is_none() && body.get("reporter").is_none(),
        "şikayet edenin kimliği sızmamalı: {body}"
    );

    let (status, body, _) = send(
        &router,
        auth_json_req("POST", "/reports", &sikayetci_key, istek),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

/// Hedef türü içeriğin gerçek türüyle uyuşmalı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sikayet_hedef_turu_uyusmazsa_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, key) = seed_actor(&raw_pool, "tur_uyusmaz").await;
    let post = seed_post(&router, &key, "post").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/reports",
            &key,
            json!({ "target_type": "comment", "target_id": post, "reason": "yanlış tür" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

// --- Yetki sınırları --------------------------------------------------------

/// **Planın açık maddesi:** her admin ucunda yetkisiz erişim `403` dönmeli.
///
/// Sıradan (rolsüz) bir actor'ün kimliğiyle bütün admin uçları taranıyor.
/// `401` değil `403` bekleniyor: kimlik geçerli, yetki yok.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yetkisiz_erisim_tum_admin_uclarinda_403(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, sirada_key) = seed_actor(&raw_pool, "rolsuz_actor").await;

    let uclar: Vec<(&str, &str, Option<Value>)> = vec![
        ("GET", "/admin/reports", None),
        (
            "PATCH",
            "/admin/reports/r_zzzz",
            Some(json!({ "status": "resolved" })),
        ),
        (
            "DELETE",
            "/admin/contents/c_zzzz",
            Some(json!({ "reason": "x" })),
        ),
        (
            "POST",
            "/admin/bans",
            Some(json!({ "username": "biri", "reason": "x" })),
        ),
        ("DELETE", "/admin/bans/biri", None),
        (
            "PUT",
            "/admin/permissions",
            Some(json!({ "username": "biri", "permission": "content.delete" })),
        ),
        (
            "DELETE",
            "/admin/permissions",
            Some(json!({ "username": "biri", "permission": "content.delete" })),
        ),
        ("GET", "/admin/actions", None),
    ];

    for (method, path, govde) in uclar {
        let istek = match govde {
            Some(g) => auth_json_req(method, path, &sirada_key, g),
            None => auth_req(method, path, &sirada_key),
        };
        let (status, body, _) = send(&router, istek).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {path} yetkisiz erişimde 403 dönmeli: {body}"
        );
    }
}

/// Kimliksiz erişim `401` — `403`'ten farklı: kimlik hiç sunulmamış.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kimliksiz_admin_erisimi_401(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/admin/reports")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

/// Moderatör izin verme yetkisine (`role.grant`) sahip değil — yalnızca admin.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn moderator_izin_yonetimine_erisemiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (mod_id, mod_key) = seed_actor(&raw_pool, "sadece_moderator").await;
    seed_actor(&raw_pool, "hedef_kullanici").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;

    // Moderatör kuyruğu görebiliyor...
    let (status, body, _) = send(&router, auth_req("GET", "/admin/reports", &mod_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // ...ama izin veremiyor (`role.grant` yok).
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &mod_key,
            json!({ "username": "hedef_kullanici", "permission": "content.delete" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn admin_kendi_izinini_degistiremiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (admin_id, admin_key) = seed_actor(&raw_pool, "kendi_rol_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &admin_key,
            json!({ "username": "kendi_rol_admin", "permission": "content.delete" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

// --- Ban davranışı ----------------------------------------------------------

/// **Planın kararı:** banlı actor yazamaz ama okuyabilir.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn banli_actor_yazamiyor_ama_okuyabiliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (mod_id, mod_key) = seed_actor(&raw_pool, "banlayan_mod").await;
    let (_, kurban_key) = seed_actor(&raw_pool, "banlanacak").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;

    // Ban öncesi yazabiliyor.
    let post = seed_post(&router, &kurban_key, "ban öncesi").await;
    assert!(!post.is_empty());

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &mod_key,
            json!({ "username": "banlanacak", "reason": "kural ihlali" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["username"], "banlanacak", "{body}");
    assert!(body["expires_at"].is_null(), "kalıcı ban: {body}");

    // Yazma artık kapalı.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &kurban_key,
            json!({ "title": "ban sonrası", "body": "gövde", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "banlı actor yazamamalı: {body}"
    );
    assert_eq!(body["code"], "BANNED", "{body}");

    // Okuma serbest.
    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &kurban_key)).await;
    assert_eq!(status, StatusCode::OK, "banlı actor okuyabilmeli: {body}");
    assert_eq!(body["actor"]["username"], "banlanacak", "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/me/saves", &kurban_key)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "banlı actor kendi kayıtlarını görebilmeli: {body}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ban_kaldirilinca_yazma_geri_geliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (mod_id, mod_key) = seed_actor(&raw_pool, "ban_kaldiran").await;
    let (_, kurban_key) = seed_actor(&raw_pool, "ban_kalkacak").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;

    let (status, _, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &mod_key,
            json!({ "username": "ban_kalkacak", "reason": "geçici" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", "/admin/bans/ban_kalkacak", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Tekrar kaldırmak da hatasız (idempotent).
    let (status, _, _) = send(
        &router,
        auth_req("DELETE", "/admin/bans/ban_kalkacak", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let post = seed_post(&router, &kurban_key, "ban sonrası tekrar").await;
    assert!(!post.is_empty(), "ban kalkınca yazma geri gelmeli");
}

// --- Denetim izi ------------------------------------------------------------

/// Her admin eylemi ize düşmeli — kayıt handler'da elle yazılmıyor,
/// domain fonksiyonunun kendi transaction'ında (bkz.
/// `actos_core::moderation` modül dokümantasyonu).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn her_admin_eylemi_denetim_izine_dusuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (admin_id, admin_key) = seed_actor(&raw_pool, "iz_admin").await;
    let (_, yazar_key) = seed_actor(&raw_pool, "iz_yazar").await;
    seed_actor(&raw_pool, "iz_hedef").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    assert_eq!(iz_sayisi(&raw_pool).await, 0, "başta iz boş olmalı");

    let post = seed_post(&router, &yazar_key, "silinecek").await;

    // 1) İçerik silme
    let (status, _, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{post}"),
            &admin_key,
            json!({ "reason": "kural ihlali" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 2) Ban
    let (status, _, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &admin_key,
            json!({ "username": "iz_hedef", "reason": "spam" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // 3) Ban kaldırma
    let (status, _, _) = send(
        &router,
        auth_req("DELETE", "/admin/bans/iz_hedef", &admin_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 4) İzin verme
    let (status, _, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &admin_key,
            json!({ "username": "iz_hedef", "permission": "content.delete" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(iz_sayisi(&raw_pool).await, 4, "dört eylem de ize düşmeli");

    let (status, body, _) = send(&router, auth_req("GET", "/admin/actions", &admin_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let eylemler = body["actions"].as_array().expect("actions dizi");
    assert_eq!(eylemler.len(), 4, "{body}");
    // En yeni önce.
    assert_eq!(eylemler[0]["action_type"], "permission_grant", "{body}");
    assert_eq!(eylemler[3]["action_type"], "content_delete", "{body}");
    assert_eq!(eylemler[3]["reason"], "kural ihlali", "{body}");
    assert_eq!(eylemler[0]["admin_username"], "iz_admin", "{body}");
}

/// Denetim izi append-only: `forbid_mutation` trigger'ı silmeyi reddediyor,
/// yani bir moderatör kendi izini temizleyemez.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn denetim_izi_silinemiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (mod_id, mod_key) = seed_actor(&raw_pool, "iz_silmeye_calisan").await;
    seed_actor(&raw_pool, "iz_silme_hedefi").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;

    let (status, _, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &mod_key,
            json!({ "username": "iz_silme_hedefi", "reason": "test" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let sonuc = sqlx::query!(r#"DELETE FROM admin_actions_log"#)
        .execute(&raw_pool)
        .await;

    assert!(sonuc.is_err(), "denetim izi silinebilmemeli");
    assert_eq!(iz_sayisi(&raw_pool).await, 1, "kayıt yerinde durmalı");
}

// --- Moderasyon kuyruğu -----------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kuyruk_status_filtresi_ve_cozme_akisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (mod_id, mod_key) = seed_actor(&raw_pool, "kuyruk_mod").await;
    let (_, yazar_key) = seed_actor(&raw_pool, "kuyruk_yazar").await;
    let (_, sikayetci_key) = seed_actor(&raw_pool, "kuyruk_sikayetci").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;

    let post = seed_post(&router, &yazar_key, "kuyruğa girecek").await;

    let (status, rapor, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/reports",
            &sikayetci_key,
            json!({ "target_type": "post", "target_id": post, "reason": "spam" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{rapor}");
    let rapor_id = rapor["id"].as_str().expect("rapor id").to_owned();

    // Bekleyenler listesinde.
    let (status, body, _) = send(
        &router,
        auth_req("GET", "/admin/reports?status=pending", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["reports"].as_array().expect("dizi").len(), 1, "{body}");

    // Çöz.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/admin/reports/{rapor_id}"),
            &mod_key,
            json!({ "status": "resolved", "notes": "içerik kaldırıldı" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "resolved", "{body}");
    assert_eq!(body["notes"], "içerik kaldırıldı", "{body}");
    assert!(
        !body["resolved_at"].is_null(),
        "resolved_at dolmalı: {body}"
    );

    // Bekleyenlerde artık yok.
    let (status, body, _) = send(
        &router,
        auth_req("GET", "/admin/reports?status=pending", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["reports"].as_array().expect("dizi").is_empty(),
        "{body}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gecersiz_status_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (mod_id, mod_key) = seed_actor(&raw_pool, "gecersiz_status_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/admin/reports?status=belirsiz", &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// Moderatör silmesi sahiplik aramıyor ve gerekçeyi ize yazıyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn moderator_baskasinin_icerigini_silebiliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (mod_id, mod_key) = seed_actor(&raw_pool, "silen_mod").await;
    let (_, yazar_key) = seed_actor(&raw_pool, "silinen_yazar").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;

    let post = seed_post(&router, &yazar_key, "başkasının postu").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{post}"),
            &mod_key,
            json!({ "reason": "telif ihlali" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // İçerik artık 410.
    let (status, body, _) = send(&router, empty_req("GET", &format!("/posts/{post}"))).await;
    assert_eq!(status, StatusCode::GONE, "{body}");

    // Gerekçesiz silme reddedilmeli.
    let post2 = seed_post(&router, &yazar_key, "ikinci").await;
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{post2}"),
            &mod_key,
            json!({ "reason": "" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "gerekçe zorunlu: {body}");
}

// --- Kapsamlı izin modeli ---------------------------------------------------

/// İzin verildikten sonra `whoami` onu global kapsamda, topluluksuz gösterir.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn izin_verilince_whoami_kapsami_gosteriyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (admin_id, admin_key) = seed_actor(&raw_pool, "whoami_admin").await;
    let (_, hedef_key) = seed_actor(&raw_pool, "whoami_hedef").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &admin_key,
            json!({ "username": "whoami_hedef", "permission": "content.delete" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &hedef_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let permissions = body["permissions"].as_array().expect("permissions dizi");
    assert_eq!(permissions.len(), 1, "{body}");
    assert_eq!(permissions[0]["permission"], "content.delete", "{body}");
    assert_eq!(permissions[0]["scope"], "global", "{body}");
    assert!(permissions[0]["community"].is_null(), "{body}");
}

/// Kaldırma idempotent: izin hiç verilmemişken, verildikten sonra ve tekrar
/// kaldırıldığında her seferinde `204`.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn izin_kaldirma_idempotent(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (admin_id, admin_key) = seed_actor(&raw_pool, "revoke_admin").await;
    seed_actor(&raw_pool, "revoke_hedef").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    let govde = json!({ "username": "revoke_hedef", "permission": "content.delete" });

    // Hiç verilmemişken kaldırma da başarı döner.
    let (status, body, _) = send(
        &router,
        auth_json_req("DELETE", "/admin/permissions", &admin_key, govde.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Ver, kaldır, tekrar kaldır — ikisi de 204.
    let (status, _, _) = send(
        &router,
        auth_json_req("PUT", "/admin/permissions", &admin_key, govde.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for _ in 0..2 {
        let (status, body, _) = send(
            &router,
            auth_json_req("DELETE", "/admin/permissions", &admin_key, govde.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }
}

/// Bilinmeyen bir izin dizesi `400` (DB enum'una düşmeden doğrulanır).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bilinmeyen_izin_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (admin_id, admin_key) = seed_actor(&raw_pool, "bilinmeyen_izin_admin").await;
    seed_actor(&raw_pool, "bilinmeyen_izin_hedef").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &admin_key,
            json!({ "username": "bilinmeyen_izin_hedef", "permission": "does.not.exist" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// Var olmayan bir topluluk adıyla kapsamlı izin istemek `404` (kapsam
/// doğrulamasından önce id çözümü yapılıyor).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn olmayan_topluluk_404(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (admin_id, admin_key) = seed_actor(&raw_pool, "topluluk_admin").await;
    seed_actor(&raw_pool, "topluluk_hedef").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &admin_key,
            json!({
                "username": "topluluk_hedef",
                "permission": "content.delete",
                "community": "hic_olmadi",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "NOT_FOUND", "{body}");
}

/// Topluluk kapsamlı bir izin verilebiliyor ve `whoami` onu topluluk adıyla
/// birlikte `community` kapsamında gösteriyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_kapsamli_izin_whoami_de_isimle_gorunuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (owner_id, _) = seed_actor(&raw_pool, "kapsam_sahibi").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "kapsam_admin").await;
    let (_, hedef_key) = seed_actor(&raw_pool, "kapsam_hedef").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    core_community::create_community(
        &raw_pool,
        owner_id,
        "kapsam_kulubu",
        "açıklama",
        core_community::CommunityVisibility::Public,
    )
    .await
    .expect("topluluk oluşturulabilmeli");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &admin_key,
            json!({
                "username": "kapsam_hedef",
                "permission": "member.ban",
                "community": "kapsam_kulubu",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &hedef_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let permissions = body["permissions"].as_array().expect("permissions dizi");
    assert_eq!(permissions.len(), 1, "{body}");
    assert_eq!(permissions[0]["permission"], "member.ban", "{body}");
    assert_eq!(permissions[0]["scope"], "community", "{body}");
    assert_eq!(permissions[0]["community"], "kapsam_kulubu", "{body}");
}

/// Yalnızca topluluk kapsamında anlamlı olan `member.kick` global verilemez.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_izni_global_verilemez(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (admin_id, admin_key) = seed_actor(&raw_pool, "kick_admin").await;
    seed_actor(&raw_pool, "kick_hedef").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &admin_key,
            json!({ "username": "kick_hedef", "permission": "member.kick" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// Yalnızca `content.delete` tutan bir aktör başkasına izin veremez ve
/// denetim izini göremez.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn icerik_silme_izni_olan_izin_veremez_ve_izi_goremez(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (silici_id, silici_key) = seed_actor(&raw_pool, "sadece_silici").await;
    seed_actor(&raw_pool, "izin_hedefi").await;
    izin_ver(&raw_pool, silici_id, Permission::ContentDelete).await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &silici_key,
            json!({ "username": "izin_hedefi", "permission": "content.delete" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/admin/actions", &silici_key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// `content.delete` başkasının postunu silmeye yeter ama ban için ayrı
/// `member.ban` gerekir.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn icerik_silme_izni_olan_silebilir_ama_banlayamaz(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (silici_id, silici_key) = seed_actor(&raw_pool, "silici_actor").await;
    let (_, yazar_key) = seed_actor(&raw_pool, "silinecek_yazar").await;
    seed_actor(&raw_pool, "banlanacak_actor").await;
    izin_ver(&raw_pool, silici_id, Permission::ContentDelete).await;

    let post = seed_post(&router, &yazar_key, "silici hedefi").await;
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{post}"),
            &silici_key,
            json!({ "reason": "yetki testi" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &silici_key,
            json!({ "username": "banlanacak_actor", "reason": "test" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

// --- Faz 3: rapor yönlendirme ------------------------------------------

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

/// Sahibi verilen aktör olan bir topluluk oluşturur.
#[allow(clippy::expect_used)]
async fn create_community(pool: &PgPool, owner_id: i64, name: &str) -> i64 {
    core_community::create_community(
        pool,
        owner_id,
        name,
        "açıklama",
        core_community::CommunityVisibility::Public,
    )
    .await
    .expect("topluluk oluşturulabilmeli")
    .id
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

/// Topluluk moderatörü yalnızca kendi topluluğunun raporlarını görür ve
/// çözer; global yetkili hepsini görür.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluk_moderatoru_yalnizca_kendi_raporlarini_gorur(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (a_sahibi, a_key) = seed_actor(&raw_pool, "rapor_a_sahibi").await;
    let (b_sahibi, b_key) = seed_actor(&raw_pool, "rapor_b_sahibi").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "rapor_moderator").await;
    let (_, sikayetci_key) = seed_actor(&raw_pool, "rapor_sikayetci").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "rapor_global_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;

    let a_id = create_community(&raw_pool, a_sahibi, "rapor_bir").await;
    create_community(&raw_pool, b_sahibi, "rapor_iki").await;
    grant_scoped(&raw_pool, mod_id, Permission::ReportView, a_id).await;
    grant_scoped(&raw_pool, mod_id, Permission::ReportResolve, a_id).await;

    let pa = post_in_community(&router, &a_key, "rapor_bir", "a postu").await;
    let pb = post_in_community(&router, &b_key, "rapor_iki", "b postu").await;
    let pi = seed_post(&router, &a_key, "bağımsız").await;

    for target in [&pa, &pb, &pi] {
        let (status, body, _) = send(
            &router,
            auth_json_req(
                "POST",
                "/reports",
                &sikayetci_key,
                json!({ "target_type": "post", "target_id": target, "reason": "spam" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    // Moderatör yalnızca A'nın raporunu görür ve community alanı doludur.
    let (status, body, _) = send(&router, auth_req("GET", "/admin/reports", &mod_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reports = body["reports"].as_array().expect("reports dizi");
    assert_eq!(reports.len(), 1, "yalnızca kendi raporunu görmeli: {body}");
    assert_eq!(reports[0]["target_id"], pa, "{body}");
    assert_eq!(reports[0]["community"], "rapor_bir", "{body}");
    let a_report_id = reports[0]["id"].as_str().expect("rapor id").to_owned();

    // Global yetkili üçünü de görür.
    let (status, body, _) = send(&router, auth_req("GET", "/admin/reports", &admin_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["reports"].as_array().expect("reports dizi").len(), 3);

    // B'nin raporunun id'sini global listeden bul.
    let b_report_id = body["reports"]
        .as_array()
        .expect("reports dizi")
        .iter()
        .find(|r| r["target_id"] == pb)
        .and_then(|r| r["id"].as_str())
        .expect("B raporu bulunmalı")
        .to_owned();

    // Moderatör B'yi çözemez, kendini çözebilir.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/admin/reports/{b_report_id}"),
            &mod_key,
            json!({ "status": "resolved" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/admin/reports/{a_report_id}"),
            &mod_key,
            json!({ "status": "resolved" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// İçerik silme yetkisi içeriğin topluluğunun kapsamında sorulur.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn icerik_silme_topluluk_kapsamina_bagli(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (a_sahibi, a_key) = seed_actor(&raw_pool, "sil_a_sahibi").await;
    let (b_sahibi, b_key) = seed_actor(&raw_pool, "sil_b_sahibi").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "sil_moderator").await;

    let a_id = create_community(&raw_pool, a_sahibi, "sil_bir").await;
    create_community(&raw_pool, b_sahibi, "sil_iki").await;
    grant_scoped(&raw_pool, mod_id, Permission::ContentDelete, a_id).await;

    let pa = post_in_community(&router, &a_key, "sil_bir", "a postu").await;
    let pb = post_in_community(&router, &b_key, "sil_iki", "b postu").await;
    let pi = seed_post(&router, &a_key, "bağımsız").await;

    let sil = |id: String| {
        let router = router.clone();
        let mod_key = mod_key.clone();
        async move {
            send(
                &router,
                auth_json_req(
                    "DELETE",
                    &format!("/admin/contents/{id}"),
                    &mod_key,
                    json!({ "reason": "kapsam testi" }),
                ),
            )
            .await
        }
    };
    // Kendi topluluğunun içeriğini silebilir.
    let (status, body, _) = sil(pa).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Başka topluluğunki `403`.
    let (status, body, _) = sil(pb).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Bağımsız içerik de global `content.delete` gerektirdiği için `403`.
    let (status, body, _) = sil(pi).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
