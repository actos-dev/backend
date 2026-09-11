//! `crates/actos-api/routes/actors.rs` entegrasyon testleri.
//!
//! Kurulum (`test_config`/`build_router`/`send`/`json_req`/`auth_req`/
//! `register`) `tests/auth_api.rs` ile birebir aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! **Takip/follow ve içerik (post/yorum) uçları henüz yok** (bkz. `PLAN.md`
//! Faz 8/9/11) — bu yüzden `follows` ve `contents` satırları burada
//! doğrudan `pool` üzerinden ham SQL/`actos_core::auth::register` ile
//! seed'leniyor, tıpkı `auth_api.rs`'teki ban testinin `bans` tablosunu
//! seed'lemesi gibi.
//!
//! **Neden çoğu fixture actor `actos_core::auth::register` ile (HTTP
//! `/auth/register` ile DEĞİL) oluşturuluyor:** `/auth/register`
//! kimliksiz istekler için IP başına saatte yalnızca birkaç kayda izin
//! veriyor (varsayılan 3/saat, bkz. `crates/actos-core/src/config.rs`
//! `RATE_LIMIT_REGISTER_IP_CAPACITY`) — sayfalama testleri tek bir router
//! içinde beşten fazla actor'e ihtiyaç duyuyor. Domain fonksiyonunu
//! doğrudan çağırmak bu limiti hiç görmez (HTTP katmanından geçmiyor) ve
//! zaten test edilen şey kayıt akışı değil, profil/sayfalama uçları.

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    auth::{self as core_auth, ActorType},
    config::{
        DatabaseConfig, LimitTable, RedisConfig, SecurityConfig, ServerConfig, StorageConfig,
        StorageQuotaConfig,
    },
    cursor::CursorCodec,
    id::{Attachment as AttachmentIdKind, IdCodec},
    idempotency::IdempotencyStore,
    ratelimit::RateLimiter,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt as _;

// --- Kurulum yardımcıları (bkz. `tests/auth_api.rs` — aynı desen) ---------

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
            orphan_cleanup_interval: std::time::Duration::ZERO,
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
    // Bkz. `crates/actos-api/tests/posts_api.rs`'teki aynı desen: rate
    // limiter ve idempotency deposu aynı benzersiz öneki paylaşıyor, ayrı
    // anahtar isim uzayları (`rl:` / `idem:`) çakışmayı zaten engelliyor.
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

#[allow(clippy::expect_used)]
fn auth_req(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("istek kurulabilmeli")
}

/// `auth_req` + JSON gövde — `PATCH /actors/me` ve `DELETE /actors/me`
/// hem kimlik hem gövde istiyor, `auth_api.rs`'teki `auth_req`/`json_req`
/// ikisi de tek başına yetmiyor.
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

async fn register(router: &Router, username: &str) -> Value {
    let (status, body, _) = send(
        router,
        json_req(
            "POST",
            "/auth/register",
            json!({ "username": username, "actor_type": "human", "display_name": null }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "kayıt başarısız oldu: {body}");
    body
}

// --- Fixture yardımcıları (HTTP dışı — bkz. dosya başı yorumu) -----------

/// `/auth/register`'ın hız sınırını görmeden doğrudan domain katmanından
/// bir actor oluşturur. Döner: iç `actor_id`.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str, actor_type: ActorType) -> i64 {
    core_auth::register(pool, username, actor_type, None)
        .await
        .expect("fixture actor oluşturulabilmeli")
        .actor
        .id
}

/// Sayfalama sırasını deterministik kılmak için `actors.created_at`'i elle
/// üzerine yazar (varsayılan `now()` art arda çağrılarda aynı mikrosaniyeye
/// denk gelebilir, testin sırası buna bağlı kalmamalı).
#[allow(clippy::expect_used)]
async fn set_actor_created_at(pool: &PgPool, actor_id: i64, at: DateTime<Utc>) {
    sqlx::query!(
        r#"UPDATE actors SET created_at = $2 WHERE id = $1"#,
        actor_id,
        at,
    )
    .execute(pool)
    .await
    .expect("created_at güncellenebilmeli");
}

#[allow(clippy::expect_used)]
async fn seed_follow(pool: &PgPool, follower_id: i64, followed_id: i64, at: DateTime<Utc>) {
    sqlx::query!(
        r#"
        INSERT INTO follows (follower_actor_id, followed_actor_id, created_at)
        VALUES ($1, $2, $3)
        "#,
        follower_id,
        followed_id,
        at,
    )
    .execute(pool)
    .await
    .expect("follow eklenebilmeli");
}

/// Bir post ekler, döner: iç `content_id`. `deleted` `true` ise ekledikten
/// hemen sonra soft-delete eder — çağıranın stat sorgusunun silinmiş
/// içeriği dışladığını sınamak için.
#[allow(clippy::expect_used)]
async fn seed_post(pool: &PgPool, actor_id: i64, score: i32, deleted: bool) -> i64 {
    let row = sqlx::query!(
        r#"
        INSERT INTO contents (actor_id, content_type, title, body, score)
        VALUES ($1, 'post'::content_type, 'başlık', 'gövde', $2)
        RETURNING id
        "#,
        actor_id,
        score,
    )
    .fetch_one(pool)
    .await
    .expect("post eklenebilmeli");

    if deleted {
        sqlx::query!(
            r#"UPDATE contents SET deleted_at = now() WHERE id = $1"#,
            row.id,
        )
        .execute(pool)
        .await
        .expect("post silinebilmeli");
    }

    row.id
}

#[allow(clippy::expect_used)]
async fn seed_comment(pool: &PgPool, actor_id: i64, parent_id: i64, score: i32) -> i64 {
    sqlx::query!(
        r#"
        INSERT INTO contents (actor_id, content_type, parent_content_id, body, score)
        VALUES ($1, 'comment'::content_type, $2, 'yorum', $3)
        RETURNING id
        "#,
        actor_id,
        parent_id,
        score,
    )
    .fetch_one(pool)
    .await
    .expect("yorum eklenebilmeli")
    .id
}

/// `attachments` tablosuna doğrudan bir satır ekler (gerçek depolamaya hiç
/// dokunmadan) — `test_config`'teki `Storage` bilerek erişilemez bir adrese
/// (`http://127.0.0.1:1`) işaret ediyor, `POST /uploads`'un gerçek akışını
/// (`actos_core::attachment::create_attachment`) buradan tetiklemenin bir
/// anlamı yok; PATCH /actors/me'nin avatar doğrulaması yalnızca satırın
/// varlığına/sahipliğine/`content_id`'sine bakıyor, gerçek bir S3 nesnesine
/// değil. `content_id` verilmişse (bir posta/yoruma "bağlı" senaryosu)
/// dolu, `None` ise (henüz bağlanmamış — avatar için geçerli durum) `NULL`.
/// Döner: iç `attachment_id`.
#[allow(clippy::expect_used)]
async fn seed_attachment(pool: &PgPool, actor_id: i64, content_id: Option<i64>) -> i64 {
    let object_key = format!("{actor_id}/{}.webp", uuid::Uuid::new_v4());
    // `ck_attachments_checksum_sha256_format`: 64 karakter küçük harf hex —
    // gerçek bir dosya hiç yok, sabit bir değer format kısıtını karşılıyor.
    let checksum = "0".repeat(64);

    sqlx::query!(
        r#"
        INSERT INTO attachments
            (actor_id, content_id, object_key, byte_size, mime_type, width, height, checksum_sha256)
        VALUES ($1, $2, $3, 1024, 'image/webp', 10, 10, $4)
        RETURNING id
        "#,
        actor_id,
        content_id,
        object_key,
        checksum,
    )
    .fetch_one(pool)
    .await
    .expect("attachment eklenebilmeli")
    .id
}

/// `actos_core::auth::register`'ı doğrudan çağırır (bkz. `seed_actor`
/// üzerindeki gerekçe — HTTP'nin hız sınırını görmeden) ama hem iç id'yi
/// hem ham `api_key`'i döner: PATCH /actors/me testleri kimliği doğrulamak
/// için gerçek bir key'e, avatar hedefini seed'lemek için de iç id'ye
/// ihtiyaç duyuyor — `register` (HTTP) + ayrı bir "actor_id_by_username"
/// sorgusu yerine tek fonksiyon.
#[allow(clippy::expect_used)]
async fn register_direct(pool: &PgPool, username: &str) -> (i64, String) {
    let reg = core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    (reg.actor.id, reg.api_key)
}

/// `test_config`'in `id_obfuscation_key`'iyle kurulmuş bir `IdCodec` —
/// `build_router`'ın içindekiyle **aynı anahtar**, yani burada üretilen
/// dış id'ler `send`'e verilen router tarafından doğru çözülebiliyor.
/// Avatar testleri, seed'lenen iç `attachment_id`'yi `PATCH /actors/me`
/// gövdesine yazabilmek için dış (`f_...`) biçime çevirmek zorunda.
#[allow(clippy::expect_used)]
fn test_id_codec() -> IdCodec {
    IdCodec::new(&test_config().security.id_obfuscation_key).expect("geçerli anahtar")
}

// --- Profil ----------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn profil_canli_actor_icin_200_ve_istatistikler_dogru(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    register(&router, "stats_user").await;
    let actor_id = sqlx::query_scalar!(r#"SELECT id FROM actors WHERE username = 'stats_user'"#)
        .fetch_one(&raw_pool)
        .await
        .expect("actor id bulunmalı");

    // 2 canlı post (skor 3 ve -1), 1 silinmiş post (skor 100 — hariç
    // tutulmalı), silinmiş post'un altına yorum yazılamayacağı için 1
    // yorum canlı bir post'un altına (skor 5).
    let live_post = seed_post(&raw_pool, actor_id, 3, false).await;
    seed_post(&raw_pool, actor_id, -1, false).await;
    seed_post(&raw_pool, actor_id, 100, true).await;
    seed_comment(&raw_pool, actor_id, live_post, 5).await;

    let (status, body, _) = send(&router, empty_req("GET", "/actors/stats_user")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["actor"]["username"], "stats_user");
    assert_eq!(body["stats"]["post_count"], 2, "{body}");
    assert_eq!(body["stats"]["comment_count"], 1, "{body}");
    assert_eq!(
        body["stats"]["total_score"], 7,
        "silinmiş post'un skoru (100) toplama dahil edilmemeli: {body}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn olmayan_kullanici_adi_404_not_found_doner(pool: PgPool) {
    let router = build_router(pool);

    let (status, body, _) = send(&router, empty_req("GET", "/actors/hic_boyle_biri_yok")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_actor_profili_404_degil_410_gone_doner(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "soon_deleted_user").await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();
    let recovery_code = reg["recovery_codes"][0]
        .as_str()
        .expect("kurtarma kodu olmalı")
        .to_owned();

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            "/actors/me",
            &api_key,
            json!({ "recovery_code": recovery_code }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Username serbest bırakılmadığı için "yok" (404) değil "silindi"
    // (410) dönmeli — bkz. `actos_core::actor::get_profile` dokümanı.
    let (status, body, _) = send(&router, empty_req("GET", "/actors/soon_deleted_user")).await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["code"], "GONE");
}

// --- PATCH /actors/me --------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_me_kismi_guncelleme_dokunma_temizle_guncelle_ayrimi(pool: PgPool) {
    let router = build_router(pool);
    let (_, reg, _) = send(
        &router,
        json_req(
            "POST",
            "/auth/register",
            json!({
                "username": "patch_user",
                "actor_type": "human",
                "display_name": "Başlangıç İsim",
            }),
        ),
    )
    .await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();
    assert_eq!(reg["actor"]["display_name"], "Başlangıç İsim");
    assert!(reg["actor"]["bio"].is_null());

    // 1) `display_name` gönderilmiyor → dokunulmamalı; `bio` gönderiliyor
    //    → dolmalı.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "bio": "Yeni bio" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["actor"]["display_name"], "Başlangıç İsim",
        "gönderilmeyen alana dokunulmamalı: {body}"
    );
    assert_eq!(body["actor"]["bio"], "Yeni bio");

    // 2) `display_name: null` → temizlenmeli; `bio` gönderilmiyor →
    //    dokunulmamalı (önceki adımdan kalan değeri korumalı).
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "display_name": null }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["actor"]["display_name"].is_null(),
        "null gönderilen alan temizlenmeli: {body}"
    );
    assert_eq!(
        body["actor"]["bio"], "Yeni bio",
        "gönderilmeyen alana dokunulmamalı: {body}"
    );

    // 3) İkisi de yeni bir değerle güncelleniyor.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "display_name": "Güncel İsim", "bio": null }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["actor"]["display_name"], "Güncel İsim");
    assert!(body["actor"]["bio"].is_null());
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_me_gecersiz_bio_400_validation_failed_doner(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "patch_invalid_user").await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "bio": "a".repeat(501) }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

// --- PATCH /actors/me — avatar --------------------------------------------
//
// Faz 18.A "Avatar": `req.avatar` üç doğrulamadan geçiyor
// (`actos_core::attachment::resolve_as_avatar`) — sırasıyla aşağıdaki dört
// test bunları ve başarılı set/kaldırma yolunu kapsıyor.

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_me_avatar_baskasinin_ekiyle_403_forbidden_doner(pool: PgPool) {
    let router = build_router(pool.clone());
    let (_, api_key) = register_direct(&pool, "avatar_sahibi_degil").await;
    let (baskasi_id, _) = register_direct(&pool, "avatar_baskasi").await;
    let baskasinin_eki = seed_attachment(&pool, baskasi_id, None).await;
    let ek_dis_id = test_id_codec()
        .encode::<AttachmentIdKind>(baskasinin_eki)
        .expect("kodlanabilmeli");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "avatar": ek_dis_id }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "FORBIDDEN");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_me_avatar_olmayan_id_404_not_found_doner(pool: PgPool) {
    let router = build_router(pool.clone());
    let (_, api_key) = register_direct(&pool, "avatar_olmayan_id").await;
    // Hiç eklenmemiş, uydurma bir iç id — `encode` kendisi asla başarısız
    // olmuyor (bkz. `IdCodec` dokümantasyonu), yalnızca çözüldüğünde
    // veritabanında karşılığı olmayan bir `f_...` üretiyor.
    let ek_dis_id = test_id_codec()
        .encode::<AttachmentIdKind>(999_999_999)
        .expect("kodlanabilmeli");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "avatar": ek_dis_id }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "NOT_FOUND");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_me_avatar_icerige_bagli_ek_409_conflict_doner(pool: PgPool) {
    let router = build_router(pool.clone());
    let (actor_id, api_key) = register_direct(&pool, "avatar_bagli_ek").await;
    let post_id = seed_post(&pool, actor_id, 0, false).await;
    // Ek kendi postuna bağlı — `content_id IS NOT NULL` — avatar olarak
    // reddedilmeli (bkz. `actos_core::attachment::resolve_as_avatar`
    // dokümanındaki `409` seçim gerekçesi).
    let bagli_ek = seed_attachment(&pool, actor_id, Some(post_id)).await;
    let ek_dis_id = test_id_codec()
        .encode::<AttachmentIdKind>(bagli_ek)
        .expect("kodlanabilmeli");

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "avatar": ek_dis_id }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "CONFLICT");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_me_avatar_set_edilir_gorunur_null_ile_kaldirilir(pool: PgPool) {
    let router = build_router(pool.clone());
    let (actor_id, api_key) = register_direct(&pool, "avatar_set_kaldir").await;
    let ek_id = seed_attachment(&pool, actor_id, None).await;
    let ek_dis_id = test_id_codec()
        .encode::<AttachmentIdKind>(ek_id)
        .expect("kodlanabilmeli");

    // 1) Set: `200` + yanıttaki `avatar_url` beklenen `public_base_url` +
    //    `object_key` birleşimi olmalı (bkz. `Storage::public_url`).
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &api_key,
            json!({ "avatar": ek_dis_id }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let avatar_url = body["actor"]["avatar_url"]
        .as_str()
        .expect("avatar_url string olmalı: {body}")
        .to_owned();
    assert!(
        avatar_url.starts_with("http://127.0.0.1:1/test-bucket/"),
        "beklenmeyen avatar_url: {avatar_url}"
    );

    // 2) `GET /actors/{username}` da aynı avatar_url'i taşımalı — `PATCH`
    //    yanıtına özel bir kısayol değil, `actors.avatar_object_key` gerçekten
    //    yazıldı.
    let (status, profile_body, _) =
        send(&router, empty_req("GET", "/actors/avatar_set_kaldir")).await;
    assert_eq!(status, StatusCode::OK, "{profile_body}");
    assert_eq!(profile_body["actor"]["avatar_url"], avatar_url);

    // 3) `avatar: null` → kaldırılmalı.
    let (status, body, _) = send(
        &router,
        auth_json_req("PATCH", "/actors/me", &api_key, json!({ "avatar": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["actor"]["avatar_url"].is_null(),
        "null gönderilince avatar kaldırılmalı: {body}"
    );
}

// --- DELETE /actors/me ---------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn delete_me_yanlis_kod_reddedilir_dogru_kod_hesabi_ve_keyleri_olduruyor(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "delete_user").await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();
    let recovery_code = reg["recovery_codes"][0]
        .as_str()
        .expect("kurtarma kodu olmalı")
        .to_owned();

    // İkinci bir key üret — "tüm key'ler iptal edilir" iddiasını tek
    // key'le sınamak yetmez.
    let (status, second_key_body, _) = send(
        &router,
        auth_json_req("POST", "/auth/keys", &api_key, json!({ "label": null })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second_key_body}");
    let second_key = second_key_body["api_key"]
        .as_str()
        .expect("api_key olmalı")
        .to_owned();

    // Yanlış kod: reddedilmeli, hesap hâlâ çalışır durumda kalmalı.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            "/actors/me",
            &api_key,
            json!({ "recovery_code": "0000-0000-0000" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "INVALID_KEY");

    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &api_key)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "yanlış kodla silme denemesi hesabı etkilememeli: {body}"
    );

    // Doğru kod: hesap işaretlenmeli, iki key de ölmeli.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "DELETE",
            "/actors/me",
            &api_key,
            json!({ "recovery_code": recovery_code }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &api_key)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "silinen hesabın ilk key'i hâlâ çalışıyor: {body}"
    );
    assert_eq!(body["code"], "INVALID_KEY");

    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &second_key)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "silinen hesabın ikinci key'i hâlâ çalışıyor: {body}"
    );
    assert_eq!(body["code"], "INVALID_KEY");
}

// --- Keşif dizini --------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn dizin_type_filtresi_dogru_calisiyor(pool: PgPool) {
    let router = build_router(pool.clone());
    seed_actor(&pool, "dir_human_1", ActorType::Human).await;
    seed_actor(&pool, "dir_human_2", ActorType::Human).await;
    seed_actor(&pool, "dir_agent_1", ActorType::AiAgent).await;
    seed_actor(&pool, "dir_agent_2", ActorType::AiAgent).await;

    let (status, body, _) = send(&router, empty_req("GET", "/actors?type=ai_agent")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let actors = body["actors"].as_array().expect("actors dizi olmalı");
    assert_eq!(actors.len(), 2, "{body}");
    for actor in actors {
        assert_eq!(actor["actor_type"], "ai_agent", "{body}");
    }
    let usernames: Vec<&str> = actors
        .iter()
        .map(|a| a["username"].as_str().expect("username olmalı"))
        .collect();
    assert!(usernames.contains(&"dir_agent_1"));
    assert!(usernames.contains(&"dir_agent_2"));
    assert!(!usernames.contains(&"dir_human_1"));
    assert!(!usernames.contains(&"dir_human_2"));
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn dizin_gecersiz_sort_400_validation_failed_doner(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/actors?sort=top")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn dizin_cursor_ikinci_sayfayi_dogru_getiriyor(pool: PgPool) {
    let router = build_router(pool.clone());
    let base = Utc::now() - Duration::hours(1);

    // 5 actor, artan `created_at` (en yeni sonuncusu) — dizin en yeni
    // önce sıralanır, yani beklenen sıra: 4, 3, 2, 1, 0.
    let mut ids = Vec::new();
    for i in 0..5i64 {
        let id = seed_actor(&pool, &format!("dir_page_user_{i}"), ActorType::Human).await;
        set_actor_created_at(&pool, id, base + Duration::minutes(i)).await;
        ids.push(id);
    }

    let mut seen_usernames: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(c) => format!("/actors?limit=2&cursor={c}"),
            None => "/actors?limit=2".to_owned(),
        };
        let (status, body, _) = send(&router, empty_req("GET", &uri)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let actors = body["actors"].as_array().expect("actors dizi olmalı");
        assert!(actors.len() <= 2, "limit aşıldı: {body}");

        for actor in actors {
            seen_usernames.push(
                actor["username"]
                    .as_str()
                    .expect("username olmalı")
                    .to_owned(),
            );
        }

        cursor = body["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    let expected: Vec<String> = (0..5).rev().map(|i| format!("dir_page_user_{i}")).collect();
    assert_eq!(
        seen_usernames, expected,
        "sayfalar birleştirildiğinde en yeniden eskiye tam sıra beklenir"
    );
}

// --- Takipçi / takip listeleri --------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn takipciler_en_yeniden_eskiye_sayfalaniyor(pool: PgPool) {
    let router = build_router(pool.clone());
    let target_id = seed_actor(&pool, "popular_agent", ActorType::AiAgent).await;

    let base = Utc::now() - Duration::hours(1);
    let mut follower_ids = Vec::new();
    for i in 0..5i64 {
        let follower_id = seed_actor(&pool, &format!("follower_{i}"), ActorType::Human).await;
        seed_follow(&pool, follower_id, target_id, base + Duration::minutes(i)).await;
        follower_ids.push(follower_id);
    }

    let mut seen_usernames: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(c) => format!("/actors/popular_agent/followers?limit=2&cursor={c}"),
            None => "/actors/popular_agent/followers?limit=2".to_owned(),
        };
        let (status, body, _) = send(&router, empty_req("GET", &uri)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let actors = body["actors"].as_array().expect("actors dizi olmalı");
        assert!(actors.len() <= 2, "limit aşıldı: {body}");

        for actor in actors {
            seen_usernames.push(
                actor["username"]
                    .as_str()
                    .expect("username olmalı")
                    .to_owned(),
            );
        }

        cursor = body["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    // En yeni takip ilişkisi (follower_4) önce.
    let expected: Vec<String> = (0..5).rev().map(|i| format!("follower_{i}")).collect();
    assert_eq!(seen_usernames, expected);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn following_listesi_dogru_yonu_donuyor(pool: PgPool) {
    let router = build_router(pool.clone());
    let source_id = seed_actor(&pool, "explorer_agent", ActorType::AiAgent).await;
    let unrelated_id = seed_actor(&pool, "unrelated_agent", ActorType::AiAgent).await;

    let now = Utc::now();
    let followed_a = seed_actor(&pool, "followed_a", ActorType::Human).await;
    let followed_b = seed_actor(&pool, "followed_b", ActorType::Human).await;
    seed_follow(&pool, source_id, followed_a, now - Duration::minutes(2)).await;
    seed_follow(&pool, source_id, followed_b, now - Duration::minutes(1)).await;
    // `unrelated_agent`'in takip ettikleri `explorer_agent`'in listesine
    // sızmamalı — ilişkinin yönü doğru filtrelenmeli.
    seed_follow(&pool, unrelated_id, followed_a, now).await;

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/explorer_agent/following?limit=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let actors = body["actors"].as_array().expect("actors dizi olmalı");
    let usernames: Vec<&str> = actors
        .iter()
        .map(|a| a["username"].as_str().expect("username olmalı"))
        .collect();

    // En yeni takip önce: followed_b, followed_a.
    assert_eq!(usernames, vec!["followed_b", "followed_a"], "{body}");
    assert!(body["next_cursor"].is_null());
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn takipciler_olmayan_kullanici_icin_404_doner(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/hic_boyle_biri_yok/followers"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "NOT_FOUND");
}
