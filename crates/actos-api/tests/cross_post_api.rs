//! Çapraz-gönderi uçtan uca testleri (COMMUNITY_PLAN.md §8, Faz 5).
//!
//! Kurulum yardımcıları `tests/visibility_api.rs` ile aynı desen — Rust her
//! `tests/*.rs` dosyasını bağımsız derlediği için paylaşılan bir modül
//! olmadan tekrar tanımlanıyor.
//!
//! **Kapsanan sözleşme:** çapraz-gönderi bir kopya değil referanstır; kaynak
//! okuma anında okuyucunun izinleriyle çözülür. Üç kural: özel topluluktan
//! hiçbir şey çıkmaz, erişilemez kaynak mezar taşıdır (sebep açıklanmaz),
//! derinlik tek seviyedir.

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    auth::{self as core_auth, ActorType},
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

// --- Kurulum yardımcıları (bkz. `tests/visibility_api.rs`) ----------------

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

/// `POST /communities`; `visibility` verilirse onunla. Gövdeyi döner.
#[allow(clippy::expect_used)]
async fn create_community(router: &Router, token: &str, name: &str, visibility: &str) -> Value {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            "/communities",
            token,
            json!({ "name": name, "description": "açıklama", "visibility": visibility }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "topluluk açılamadı ({name}): {body}"
    );
    body
}

#[allow(clippy::expect_used)]
async fn community_id(pool: &PgPool, name: &str) -> i64 {
    core_community::resolve_id_by_name(pool, name)
        .await
        .expect("topluluk bulunabilmeli")
}

/// Özel topluluğa doğrudan SQL ile üye ekler (join ucu private'ı reddeder).
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

/// `POST /posts`; `community` verilirse topluluk postu. Gövdeyi döner.
#[allow(clippy::expect_used)]
async fn create_post(
    router: &Router,
    token: &str,
    title: &str,
    body: &str,
    community: Option<&str>,
) -> Value {
    let mut payload = json!({ "title": title, "body": body, "tags": [] });
    if let Some(community) = community {
        payload["community"] = json!(community);
    }
    let (status, body, _) = send(router, auth_json_req("POST", "/posts", token, payload)).await;
    assert_eq!(status, StatusCode::CREATED, "post açılamadı: {body}");
    body
}

/// `POST /posts` ile çapraz-gönderi; başarı **beklemeden** ham sonucu döner
/// (hata durumlarını test etmek için).
#[allow(clippy::expect_used)]
async fn try_cross_post(
    router: &Router,
    token: &str,
    source_id: &str,
    community: Option<&str>,
) -> (StatusCode, Value) {
    let mut payload = json!({ "title": "", "body": "", "cross_post_source": source_id });
    if let Some(community) = community {
        payload["community"] = json!(community);
    }
    let (status, body, _) = send(router, auth_json_req("POST", "/posts", token, payload)).await;
    (status, body)
}

#[allow(clippy::expect_used)]
async fn cross_post(
    router: &Router,
    token: &str,
    source_id: &str,
    community: Option<&str>,
) -> Value {
    let (status, body) = try_cross_post(router, token, source_id, community).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "çapraz-gönderi açılamadı: {body}"
    );
    body
}

#[allow(clippy::expect_used)]
async fn vote(router: &Router, token: &str, id: &str, value: i16) -> Value {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "PUT",
            &format!("/contents/{id}/vote"),
            token,
            json!({ "value": value }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "oy verilemedi: {body}");
    body
}

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

#[allow(clippy::expect_used)]
async fn get_post(router: &Router, token: Option<&str>, id: &str) -> (StatusCode, Value) {
    let req = match token {
        Some(token) => auth_req("GET", &format!("/posts/{id}"), token),
        None => empty_req("GET", &format!("/posts/{id}")),
    };
    let (status, body, _) = send(router, req).await;
    (status, body)
}

#[allow(clippy::expect_used)]
async fn patch_visibility(router: &Router, token: &str, name: &str, visibility: &str) -> Value {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "PATCH",
            &format!("/communities/{name}"),
            token,
            json!({ "visibility": visibility }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "görünürlük değiştirilemedi: {body}");
    body
}

/// Bir feed/liste yanıtındaki dizide, verilen post id'sini bulur.
fn find_by_id(body: &Value, array_key: &str, id: &str) -> Option<Value> {
    body[array_key]
        .as_array()
        .unwrap_or_else(|| panic!("{array_key} dizi olmalı: {body}"))
        .iter()
        .find(|item| item["id"] == json!(id))
        .cloned()
}

// --- Temel çapraz-gönderi -----------------------------------------------------------------

/// Bağımsız bir public post'tan çapraz-gönderi: kaynak önizlemesi dolu,
/// çapraz-gönderinin kendi başlığı `null`/gövdesi boş. Kendi oyu ve kendi
/// yorum thread'i kaynaktan **bağımsız**.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bagimsiz_paylastan_capraz_gonderi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, yazar_key) = seed_actor(&raw_pool, "cp_ana_yazar").await;
    let (_, okuyucu_key) = seed_actor(&raw_pool, "cp_ana_okuyucu").await;

    let kaynak = create_post(&router, &yazar_key, "kaynak başlık", "kaynak gövde", None).await;
    let kaynak_id = kaynak["id"].as_str().expect("id").to_owned();

    let cp = cross_post(&router, &yazar_key, &kaynak_id, None).await;
    let cp_id = cp["id"].as_str().expect("id").to_owned();

    assert_eq!(cp["is_cross_post"], json!(true));
    assert_eq!(cp["title"], Value::Null, "çapraz-gönderinin başlığı yok");
    assert_eq!(cp["body"], json!(""), "çapraz-gönderinin gövdesi boş");
    assert_eq!(cp["cross_post"]["id"], json!(kaynak_id));
    assert_eq!(cp["cross_post"]["title"], json!("kaynak başlık"));
    assert_eq!(
        cp["cross_post"]["author"]["username"],
        json!("cp_ana_yazar")
    );

    // Oy ve yorum çapraz-gönderiye gider, kaynağa değil.
    vote(&router, &okuyucu_key, &cp_id, 1).await;
    create_comment(&router, &okuyucu_key, &cp_id, "çapraz-gönderiye yorum").await;

    let (_, kaynak_body) = get_post(&router, None, &kaynak_id).await;
    assert_eq!(kaynak_body["score"], json!(0), "kaynak skoru etkilenmemeli");
    assert_eq!(
        kaynak_body["comment_count"],
        json!(0),
        "kaynak yorum sayısı etkilenmemeli"
    );

    let (_, cp_body) = get_post(&router, None, &cp_id).await;
    assert_eq!(cp_body["score"], json!(1));
    assert_eq!(cp_body["comment_count"], json!(1));
    assert_eq!(
        cp_body["cross_post"]["id"],
        json!(kaynak_id),
        "tekil okuma önizlemeyi çözmeli"
    );
}

/// Derinlik sınırı: bir çapraz-gönderi kaynak olamaz.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn capraz_gonderinin_capraz_gonderisi_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, key) = seed_actor(&raw_pool, "cp_derinlik").await;
    let kaynak = create_post(&router, &key, "kaynak", "gövde", None).await;
    let cp = cross_post(&router, &key, kaynak["id"].as_str().expect("id"), None).await;

    let (status, body) = try_cross_post(&router, &key, cp["id"].as_str().expect("id"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

/// Özel topluluktaki kaynak, o topluluğun üyesi olsa bile `403`.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ozel_topluluk_kaynagi_403(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (sahip_id, sahip_key) = seed_actor(&raw_pool, "cp_ozel_sahip").await;
    create_community(&router, &sahip_key, "cp_gizli", "private").await;
    add_member(
        &raw_pool,
        community_id(&raw_pool, "cp_gizli").await,
        sahip_id,
    )
    .await;

    let kaynak = create_post(&router, &sahip_key, "gizli", "gövde", Some("cp_gizli")).await;

    let (status, body) = try_cross_post(
        &router,
        &sahip_key,
        kaynak["id"].as_str().expect("id"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "FORBIDDEN");
}

/// Çapraz-gönderi hedef topluluğa yazmak normal üyelik kuralına tabi:
/// üye olmayan `403`.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn topluluga_capraz_gonderi_uyelik_ister(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, sahip_key) = seed_actor(&raw_pool, "cp_hedef_sahip").await;
    let (_, yabanci_key) = seed_actor(&raw_pool, "cp_hedef_yabanci").await;

    create_community(&router, &sahip_key, "cp_hedef_pub", "public").await;

    let kaynak = create_post(&router, &sahip_key, "kaynak", "gövde", None).await;
    let kaynak_id = kaynak["id"].as_str().expect("id");

    // Üye olmayan hedef topluluğa çapraz-gönderemez.
    let (status, body) =
        try_cross_post(&router, &yabanci_key, kaynak_id, Some("cp_hedef_pub")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Sahip (otomatik üye) çapraz-gönderebilir.
    let cp = cross_post(&router, &sahip_key, kaynak_id, Some("cp_hedef_pub")).await;
    assert_eq!(cp["is_cross_post"], json!(true));
    assert_eq!(
        cp["community"]["name"],
        json!("cp_hedef_pub"),
        "çapraz-gönderi hedef topluluğa ait olmalı"
    );
}

// --- Public → private geçişi ve mezar taşı ------------------------------------------------

/// Public bir topluluktaki kaynak çapraz-gönderildikten sonra topluluk özel
/// olursa: üye olmayan mezar taşı görür, üye önizlemeyi görür. Kaynak
/// silinirse **herkes** mezar taşı görür. Sebep hiçbir durumda açıklanmaz.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ozel_olan_ve_silinen_kaynak_mezartasi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_sahip_id, sahip_key) = seed_actor(&raw_pool, "cp_donusum_sahip").await;
    let (uye_id, uye_key) = seed_actor(&raw_pool, "cp_donusum_uye").await;
    let (_, yabanci_key) = seed_actor(&raw_pool, "cp_donusum_yabanci").await;

    create_community(&router, &sahip_key, "cp_donusum", "public").await;
    let cid = community_id(&raw_pool, "cp_donusum").await;
    add_member(&raw_pool, cid, uye_id).await;

    let kaynak = create_post(
        &router,
        &sahip_key,
        "dönüşen kaynak",
        "gövde",
        Some("cp_donusum"),
    )
    .await;
    let kaynak_id = kaynak["id"].as_str().expect("id").to_owned();

    // Yabancı da public kaynağı görebildiği için çapraz-gönderebilir.
    let cp = cross_post(&router, &yabanci_key, &kaynak_id, None).await;
    let cp_id = cp["id"].as_str().expect("id").to_owned();

    // Topluluk özel olur (public → private tek yönlü).
    patch_visibility(&router, &sahip_key, "cp_donusum", "private").await;

    // Yabancı (üye değil): mezar taşı.
    let (status, body) = get_post(&router, Some(&yabanci_key), &cp_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_cross_post"], json!(true));
    assert_eq!(
        body["cross_post"],
        Value::Null,
        "üye olmayan mezar taşı görür"
    );

    // Üye: önizleme görünür.
    let (status, body) = get_post(&router, Some(&uye_key), &cp_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cross_post"]["id"], json!(kaynak_id));
    assert_eq!(body["cross_post"]["title"], json!("dönüşen kaynak"));

    // Sahip de üyedir, önizlemeyi görür.
    let (_, body) = get_post(&router, Some(&sahip_key), &cp_id).await;
    assert_eq!(body["cross_post"]["id"], json!(kaynak_id));

    // Kaynak silinince herkes mezar taşı.
    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{kaynak_id}"), &sahip_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    for token in [
        Some(uye_key.as_str()),
        Some(sahip_key.as_str()),
        Some(yabanci_key.as_str()),
    ] {
        let (status, body) = get_post(&router, token, &cp_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["cross_post"],
            Value::Null,
            "silinmiş kaynak herkese mezar taşı"
        );
    }
}

/// Public yüzeyler (ana akış) çapraz-gönderiyi **okuyucunun üyeliğinden
/// bağımsız** olarak public-only çözer: özel topluluktaki kaynak orada mezar
/// taşıdır. Çapraz-gönderinin hedefi bağımsız olduğu için kendisi akışta
/// kalır; yalnızca kaynağı erişilemez olur.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn public_feedde_ozel_kaynak_mezartasi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (sahip_id, sahip_key) = seed_actor(&raw_pool, "cp_yuzey_sahip").await;

    create_community(&router, &sahip_key, "cp_yuzey", "public").await;
    let cid = community_id(&raw_pool, "cp_yuzey").await;
    add_member(&raw_pool, cid, sahip_id).await;

    let kaynak = create_post(
        &router,
        &sahip_key,
        "yüzey kaynağı",
        "gövde",
        Some("cp_yuzey"),
    )
    .await;
    let kaynak_id = kaynak["id"].as_str().expect("id").to_owned();

    // Hedef bağımsız: topluluk private olunca çapraz-gönderi akışta kalır.
    let cp = cross_post(&router, &sahip_key, &kaynak_id, None).await;
    let cp_id = cp["id"].as_str().expect("id").to_owned();

    patch_visibility(&router, &sahip_key, "cp_yuzey", "private").await;

    let (status, feed, _) = send(&router, empty_req("GET", "/feed?sort=new&limit=100")).await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    let item = find_by_id(&feed, "posts", &cp_id).expect("bağımsız çapraz-gönderi feed'de olmalı");
    assert_eq!(item["is_cross_post"], json!(true));
    assert_eq!(
        item["cross_post"],
        Value::Null,
        "public feed'de özel kaynak mezar taşı"
    );
}

// --- Feed / community feed / search önizlemeleri ------------------------------------------

/// Ana akış ve topluluk akışı çapraz-gönderinin önizlemesini çözer.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn akisler_onizlemeyi_cozer(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (sahip_id, sahip_key) = seed_actor(&raw_pool, "cp_akis_sahip").await;

    create_community(&router, &sahip_key, "cp_akis", "public").await;
    let cid = community_id(&raw_pool, "cp_akis").await;
    let _ = (sahip_id, cid);

    let kaynak = create_post(&router, &sahip_key, "akis kaynağı", "gövde", None).await;
    let kaynak_id = kaynak["id"].as_str().expect("id").to_owned();

    let cp_feed = cross_post(&router, &sahip_key, &kaynak_id, None).await;
    let cp_feed_id = cp_feed["id"].as_str().expect("id").to_owned();

    let cp_community = cross_post(&router, &sahip_key, &kaynak_id, Some("cp_akis")).await;
    let cp_community_id = cp_community["id"].as_str().expect("id").to_owned();

    // Ana akış.
    let (status, feed, _) = send(&router, empty_req("GET", "/feed?sort=new&limit=100")).await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    let item = find_by_id(&feed, "posts", &cp_feed_id).expect("feed'de çapraz-gönderi olmalı");
    assert_eq!(item["cross_post"]["id"], json!(kaynak_id));

    // Topluluk akışı (anonim; public topluluk okunabilir).
    let (status, community_feed, _) = send(
        &router,
        empty_req("GET", "/communities/cp_akis/posts?sort=new&limit=100"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{community_feed}");
    let item = find_by_id(&community_feed, "posts", &cp_community_id)
        .expect("topluluk akışında çapraz-gönderi olmalı");
    assert_eq!(item["cross_post"]["id"], json!(kaynak_id));
}

/// Arama çapraz-gönderiyi bulduğunda önizlemeyi çözer.
///
/// **Not:** API hiçbir zaman çapraz-gönderiye kendi başlığını/gövdesini
/// vermez (`title = NULL`, `body = ''`), yani bir çapraz-gönderi normalde
/// hiçbir `q` ile eşleşmez. Bu test, arama yolundaki `resolve_cross_posts`
/// çağrısını tetikleyebilmek için şema-geçerli tek istisnayı kullanıyor:
/// `ck_contents_shape` bir post'a `title` VEYA `cross_post_source_id` izni
/// veriyor, ikisini birden yasaklamıyor. Başlığı SQL ile verip satırı
/// aranabilir kılıyoruz.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn arama_onizlemeyi_cozer(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, key) = seed_actor(&raw_pool, "cp_arama").await;
    let kaynak = create_post(&router, &key, "aranabilir kaynak", "gövde", None).await;
    let kaynak_id = kaynak["id"].as_str().expect("id").to_owned();

    let cp = cross_post(&router, &key, &kaynak_id, None).await;
    let cp_id = cp["id"].as_str().expect("id").to_owned();

    let source_internal: i64 = sqlx::query_scalar!(
        r#"SELECT id FROM contents WHERE title = 'aranabilir kaynak'
           AND content_type = 'post'::content_type"#,
    )
    .fetch_one(&raw_pool)
    .await
    .expect("kaynak bulunabilmeli");

    sqlx::query!(
        r#"UPDATE contents SET title = 'aranabilir capraz' WHERE cross_post_source_id = $1"#,
        source_internal,
    )
    .execute(&raw_pool)
    .await
    .expect("title güncellenebilmeli");

    let (status, results, _) = send(
        &router,
        empty_req("GET", "/search?q=aranabilir%20capraz&type=post"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{results}");
    let item = find_by_id(&results, "results", &cp_id).expect("çapraz-gönderi aramada olmalı");
    assert_eq!(item["is_cross_post"], json!(true));
    assert_eq!(item["cross_post"]["id"], json!(kaynak_id));
}
