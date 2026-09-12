//! Uçtan uca senaryo: kayıt → post → yorum → oy → skor → şikayet →
//! moderasyon kuyruğu → admin silme → maskeleme → bildirimler.
//!
//! Faz 18.B'nin ikinci görevi: platformun "gerçek bir kullanım" akışını,
//! her adımda **gerçek HTTP istekleriyle** (doğrudan `actos_core`
//! fonksiyonu çağırmadan), tek bir `#[sqlx::test]` içinde uçtan uca
//! yürütür. Kurulum yardımcıları `tests/admin_api.rs` ile aynı desen —
//! ayrı bir entegrasyon test binary'si olduğu için paylaşılan bir modül
//! olmadan tekrar tanımlanıyor.
//!
//! **One exception, deliberately outside HTTP:** the first admin's role —
//! `POST /admin/roles` itself already requires an admin (chicken-and-egg),
//! so the first admin is written directly to the database with
//! `actos_core::auth::grant_role` (see the same pattern as `rol_ver` in
//! `tests/admin_api.rs`). Every role/ban/delete operation after that is a
//! real HTTP request.
//!
//! ## Doğrulanan sözleşme ayrıntıları
//!
//! - **The score is now a flat sum of votes** (`actos_core::interaction::
//!   set_vote`) — trust level and vote weight were removed (see
//!   REFACTOR.md §3), every vote counts at full weight. Below, both actors
//!   cast a `value = 1` vote, giving a final `score = 2`.
//! - **Silinmiş post `410 Gone`, silinmiş yorum `200` + maskelenmiş gövde**
//!   (`actos_core::content`/`actos_core::comment` modül dokümanları) —
//!   iş parçacığı bütünlüğü için bilinçli bir asimetri: bir yorumun
//!   çocukları yaşamaya devam ettiği için düğümün kendisi erişilebilir
//!   kalmalı.
//! - **Post ve yorum aynı ID uzayını paylaşır** (`c_...` prefix'i,
//!   `actos_core::id::Content`) — bildirimde ayrımı `kind` alanı yapar,
//!   `target_type` ikisi için de `"content"`.
//! - **Moderasyon silmesi de bir bildirim üretir**
//!   (`actos_core::moderation::moderate_delete_content` →
//!   `NotificationKind::ModerationAction`, içeriğin **yazarına**) — bu
//!   senaryoda hem post yazarı hem yorum yazarı kendi içeriklerinin
//!   silindiğine dair ayrı birer bildirim alıyor, `comment_on_post`
//!   bildiriminden `kind` alanıyla ayrışıyor.

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    auth::{self as core_auth, AdminRole},
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

// --- Kurulum yardımcıları (bkz. `tests/admin_api.rs` — aynı desen) --------

#[allow(clippy::expect_used)]
fn test_config() -> Config {
    let mut rate_limits = LimitTable::from_env().expect("varsayılan limit tablosu geçerli olmalı");
    // Bu senaryo tek bir "IP"den (test istemcisi) art arda 7 actor
    // kaydediyor — varsayılan IP başına kayıt kotası (3/saat, bkz.
    // `actos_core::config::LimitTable::from_env`) bunun için tasarlanmadı
    // (kötüye kullanımı sınırlamak için var). Testin sınadığı şey akışın
    // kendisi, hız sınırı değil, o yüzden yalnızca bu kovayı büyütüyoruz —
    // diğer her kova (post/comment/vote/...) varsayılanında kalıyor, bu
    // senaryo onları aşacak kadar istek atmıyor.
    rate_limits.anonymous.register.capacity = 50;
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
        // Bu senaryo yükleme yapmıyor, gerçek MinIO'ya ihtiyacı yok — bkz.
        // `tests/admin_api.rs`'teki aynı erişilemez uç noktası deseni.
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
        rate_limits,
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
fn auth_req(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
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

/// Gerçek bir `POST /auth/register` isteği atar, döner: `(username, api_key)`.
#[allow(clippy::expect_used)]
async fn register(router: &Router, username: &str) -> (String, String) {
    let (status, body, _) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/auth/register")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "username": username, "actor_type": "human", "display_name": null })
                    .to_string(),
            ))
            .expect("istek kurulabilmeli"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "kayıt başarısız oldu: {body}");
    (
        username.to_owned(),
        body["api_key"].as_str().expect("api_key").to_owned(),
    )
}

/// İlk admin'in rolü — bkz. dosya başındaki modül dokümanının 1. maddesi.
#[allow(clippy::expect_used)]
async fn bootstrap_ilk_admin(pool: &PgPool, username: &str) {
    let actor_id: i64 =
        sqlx::query_scalar!(r#"SELECT id FROM actors WHERE username = $1"#, username,)
            .fetch_one(pool)
            .await
            .expect("actor bulunabilmeli");
    core_auth::grant_role(pool, actor_id, AdminRole::Admin, None)
        .await
        .expect("ilk admin rolü verilebilmeli");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
#[allow(clippy::too_many_lines)]
async fn kayittan_bildirime_uctan_uca_senaryo(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    // === 1) Kayıt: birkaç actor ==========================================
    let (_, yazar_key) = register(&router, "e2e_yazar").await;
    let (_, yorumcu_key) = register(&router, "e2e_yorumcu").await;
    let (_, oycu_bir_key) = register(&router, "e2e_oycu_bir").await;
    let (_, oycu_iki_key) = register(&router, "e2e_oycu_iki").await;
    let (_, sikayetci_key) = register(&router, "e2e_sikayetci").await;
    let (_, moderator_key) = register(&router, "e2e_moderator").await;
    let (_, admin_key) = register(&router, "e2e_admin").await;

    // İlk admin'i bootstrap et, sonra gerçek `POST /admin/roles` HTTP
    // isteğiyle moderatörü ata — buradan itibaren her rol/ban/silme işlemi
    // gerçek bir istek.
    bootstrap_ilk_admin(&raw_pool, "e2e_admin").await;
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/admin/roles",
            &admin_key,
            json!({ "username": "e2e_moderator", "role": "moderator" }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "moderatör rolü verilemedi: {body}"
    );

    // === 2) Post at =======================================================
    let (status, post_body, headers) = send(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &yazar_key,
            json!({
                "title": "Actos'ta yetki matrisi neden önemli",
                "body": "Uçtan uca senaryo testinin ana postu.",
                "tags": ["e2e"],
            }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "post oluşturulamadı: {post_body}"
    );
    let post_id = post_body["id"].as_str().expect("post id").to_owned();
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some(format!("/posts/{post_id}").as_str()),
    );

    // === 3) Yorum yap =====================================================
    let (status, comment_body, headers) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            &yorumcu_key,
            json!({ "body": "Katılıyorum, özellikle ban kontrolünün yalnızca güvenli olmayan metotlarda çalışması ilginç." }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "yorum oluşturulamadı: {comment_body}"
    );
    let comment_id = comment_body["id"].as_str().expect("comment id").to_owned();
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some(format!("/comments/{comment_id}").as_str()),
    );

    // Post ve yorum aynı ID uzayını (`c_...`) paylaşıyor.
    assert!(
        post_id.starts_with("c_"),
        "post id 'c_' ile başlamalı: {post_id}"
    );
    assert!(
        comment_id.starts_with("c_"),
        "comment id 'c_' ile başlamalı: {comment_id}"
    );
    assert_ne!(post_id, comment_id);

    // === 4) Oy ver ========================================================
    let (status, oy_body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{post_id}/vote"),
            &oycu_bir_key,
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ilk oy başarısız: {oy_body}");

    let (status, oy_body, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{post_id}/vote"),
            &oycu_iki_key,
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ikinci oy başarısız: {oy_body}");
    // Bu tekil yanıt da içeriğin güncel sayaçlarını taşıyor — burada da
    // doğrulanabilir, ama asıl doğrulama aşağıda `GET /posts/{id}` ile.
    assert_eq!(oy_body["upvotes"], 2, "{oy_body}");
    assert_eq!(oy_body["score"], 2, "iki tam ağırlıklı +1 oy: {oy_body}");

    // === 5) Skoru doğrula =================================================
    let (status, post_after_vote, _) =
        send(&router, empty_req("GET", &format!("/posts/{post_id}"))).await;
    assert_eq!(status, StatusCode::OK, "{post_after_vote}");
    assert_eq!(post_after_vote["upvotes"], 2, "{post_after_vote}");
    assert_eq!(post_after_vote["downvotes"], 0, "{post_after_vote}");
    assert_eq!(
        post_after_vote["score"], 2,
        "skor artık düz oy toplamı, her iki oy da tam ağırlıklı sayılmalı: {post_after_vote}"
    );

    // === 6) Şikayet et ====================================================
    let (status, rapor_body, _) = send(
        &router,
        auth_json_req(
            "POST",
            "/reports",
            &sikayetci_key,
            json!({ "target_type": "post", "target_id": post_id, "reason": "uygunsuz içerik (e2e testi)" }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "şikayet oluşturulamadı: {rapor_body}"
    );
    let rapor_id = rapor_body["id"].as_str().expect("rapor id").to_owned();
    assert_eq!(rapor_body["status"], "pending", "{rapor_body}");
    assert_eq!(rapor_body["target_id"], post_id, "{rapor_body}");

    // === 7) Moderasyon kuyruğunda gör =====================================
    let (status, kuyruk_body, _) = send(
        &router,
        auth_req("GET", "/admin/reports?status=pending", &moderator_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{kuyruk_body}");
    let kuyruk = kuyruk_body["reports"]
        .as_array()
        .expect("reports dizi olmalı");
    assert!(
        kuyruk.iter().any(|r| r["id"] == rapor_id),
        "şikayet moderasyon kuyruğunda görünmüyor: {kuyruk_body}"
    );

    // === 8) Admin içeriği sil =============================================
    // Post: `410 Gone` bekleniyor.
    let (status, _, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{post_id}"),
            &admin_key,
            json!({ "reason": "moderasyon kararı (e2e testi)" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Yorum: ayrıca (post'un silinmesi yorumu otomatik silmiyor — ikisi
    // bağımsız `contents` satırları) admin tarafından silinip `200` +
    // maskelenmiş gövde asimetrisi gösteriliyor.
    let (status, _, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{comment_id}"),
            &admin_key,
            json!({ "reason": "moderasyon kararı (e2e testi)" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Aynı zamanda şikayeti de çöz — kuyruğun yaşayan bir kaydı kalmasın.
    let (status, _, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/admin/reports/{rapor_id}"),
            &moderator_key,
            json!({ "status": "resolved", "notes": "içerik kaldırıldı" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // === 9) Silinmiş içeriğin doğru maskelendiğini doğrula ================
    // Post: 410 Gone, makine-okunur kod GONE.
    let (status, post_deleted_body, _) =
        send(&router, empty_req("GET", &format!("/posts/{post_id}"))).await;
    assert_eq!(status, StatusCode::GONE, "{post_deleted_body}");
    assert_eq!(post_deleted_body["code"], "GONE", "{post_deleted_body}");

    // Yorum: 200 + `deleted: true` + `[deleted]` gövdesi — 410 DEĞİL.
    let (status, comment_deleted_body, _) = send(
        &router,
        empty_req("GET", &format!("/comments/{comment_id}")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "silinmiş yorum 410 değil 200 dönmeli (iş parçacığı bütünlüğü): {comment_deleted_body}"
    );
    assert_eq!(
        comment_deleted_body["comment"]["deleted"], true,
        "{comment_deleted_body}"
    );
    assert_eq!(
        comment_deleted_body["comment"]["body"], "[deleted]",
        "{comment_deleted_body}"
    );

    // === 10) Bildirimlerin /me/inbox'a düştüğünü doğrula ==================
    // Yazar: hem "birisi postuna yorum yaptı" (comment_on_post, target =
    // yorumun kendisi) hem de "postun silindi" (moderation_action, target =
    // postun kendisi) bildirimi almalı — `kind` alanı ikisini ayırıyor,
    // `target_type` ikisinde de "content".
    let (status, yazar_inbox, _) = send(&router, auth_req("GET", "/me/inbox", &yazar_key)).await;
    assert_eq!(status, StatusCode::OK, "{yazar_inbox}");
    let yazar_bildirimleri = yazar_inbox["notifications"]
        .as_array()
        .expect("dizi olmalı");

    let yorum_bildirimi = yazar_bildirimleri
        .iter()
        .find(|n| n["kind"] == "comment_on_post")
        .unwrap_or_else(|| panic!("yazarın gelen kutusunda comment_on_post yok: {yazar_inbox}"));
    assert_eq!(
        yorum_bildirimi["target_type"], "content",
        "{yorum_bildirimi}"
    );
    assert_eq!(
        yorum_bildirimi["target_id"], comment_id,
        "{yorum_bildirimi}"
    );

    let silme_bildirimi = yazar_bildirimleri
        .iter()
        .find(|n| n["kind"] == "moderation_action" && n["target_id"] == post_id.as_str())
        .unwrap_or_else(|| {
            panic!("yazarın gelen kutusunda postun silindiğine dair bildirim yok: {yazar_inbox}")
        });
    assert_eq!(
        silme_bildirimi["target_type"], "content",
        "{silme_bildirimi}"
    );

    // Yorumcu: kendi yorumunun silindiğine dair ayrı bir moderation_action
    // bildirimi almalı, target = yorumun kendisi.
    let (status, yorumcu_inbox, _) =
        send(&router, auth_req("GET", "/me/inbox", &yorumcu_key)).await;
    assert_eq!(status, StatusCode::OK, "{yorumcu_inbox}");
    let yorumcu_bildirimleri = yorumcu_inbox["notifications"]
        .as_array()
        .expect("dizi olmalı");
    let yorum_silme_bildirimi = yorumcu_bildirimleri
        .iter()
        .find(|n| n["kind"] == "moderation_action" && n["target_id"] == comment_id.as_str())
        .unwrap_or_else(|| {
            panic!(
                "yorumcunun gelen kutusunda yorumun silindiğine dair bildirim yok: {yorumcu_inbox}"
            )
        });
    assert_eq!(
        yorum_silme_bildirimi["target_type"], "content",
        "{yorum_silme_bildirimi}"
    );

    // `unread_count` toplam okunmamışı yansıtıyor (bu sayfadaki öğe sayısı
    // değil) — yazar için en az iki okunmamış bildirim olmalı.
    assert!(
        yazar_inbox["unread_count"].as_i64().unwrap_or(0) >= 2,
        "{yazar_inbox}"
    );
}
