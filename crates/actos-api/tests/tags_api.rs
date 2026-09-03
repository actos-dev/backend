//! `crates/actos-api/routes/tags.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/posts_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! Etiket temizliği (`actos_core::tag::cleanup_unused`) burada değil,
//! `crates/actos-core/tests/tag.rs`'te test ediliyor: HTTP uçları yok,
//! domain fonksiyonu doğrudan çağrılıyor.

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
            orphan_cleanup_interval: std::time::Duration::ZERO,
            trust_level_interval: std::time::Duration::ZERO,
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

/// Etiketli bir post oluşturup dış id'sini döner.
#[allow(clippy::expect_used)]
async fn seed_post(router: &Router, api_key: &str, title: &str, tags: &[&str]) -> String {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            "/posts",
            api_key,
            json!({ "title": title, "body": "gövde", "tags": tags }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post oluşturulamadı: {body}");
    body["id"].as_str().expect("post id").to_owned()
}

async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
    let (status, body, _) = send(router, empty_req("GET", uri)).await;
    (status, body)
}

/// Yanıttaki etiket adlarını sırasıyla döner.
#[allow(clippy::expect_used)]
fn tag_names(body: &Value) -> Vec<&str> {
    body["tags"]
        .as_array()
        .expect("tags dizi olmalı")
        .iter()
        .map(|t| t["name"].as_str().expect("name string olmalı"))
        .collect()
}

// --- GET /tags -------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tags_populerlige_gore_siralaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "etiketci").await;

    // rust: 3 post, nvidia: 2, linux: 1
    seed_post(&router, &api_key, "1", &["rust", "nvidia", "linux"]).await;
    seed_post(&router, &api_key, "2", &["rust", "nvidia"]).await;
    seed_post(&router, &api_key, "3", &["rust"]).await;

    let (status, body) = get_json(&router, "/tags").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(tag_names(&body), vec!["rust", "nvidia", "linux"], "{body}");

    let sayilar: Vec<i64> = body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["post_count"].as_i64().expect("post_count sayı olmalı"))
        .collect();
    assert_eq!(sayilar, vec![3, 2, 1], "{body}");
}

/// Bütün post'ları silinmiş bir etiket popülerlik listesinden düşmeli —
/// `content_tags` satırları hâlâ durduğu için temizlik işi de onu silmez,
/// listeden çıkarmak `HAVING`'in işi.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tum_postlari_silinmis_etiket_listede_gorunmuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "silen_etiketci").await;

    let kalan = seed_post(&router, &api_key, "kalan", &["kalici"]).await;
    let silinen = seed_post(&router, &api_key, "silinen", &["gecici"]).await;

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{silinen}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = get_json(&router, "/tags").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(tag_names(&body), vec!["kalici"], "{body}");

    // `kalan` hâlâ duruyor; testin kendisi bunu kullanmıyor ama silinen
    // post'un etiketinin düştüğünü, kalanınkinin düşmediğini gösteriyor.
    assert!(!kalan.is_empty());
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tags_cursorla_sayfalaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "sayfali_etiket").await;

    seed_post(&router, &api_key, "1", &["bir", "iki", "uc"]).await;
    seed_post(&router, &api_key, "2", &["bir", "iki"]).await;
    seed_post(&router, &api_key, "3", &["bir"]).await;

    let (status, sayfa1) = get_json(&router, "/tags?limit=2").await;
    assert_eq!(status, StatusCode::OK, "{sayfa1}");
    assert_eq!(tag_names(&sayfa1), vec!["bir", "iki"], "{sayfa1}");

    let cursor = sayfa1["next_cursor"]
        .as_str()
        .expect("ikinci sayfa olmalı")
        .to_owned();

    let (status, sayfa2) = get_json(&router, &format!("/tags?limit=2&cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK, "{sayfa2}");
    assert_eq!(tag_names(&sayfa2), vec!["uc"], "{sayfa2}");
    assert!(sayfa2["next_cursor"].is_null(), "{sayfa2}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn hic_etiket_yokken_bos_liste(pool: PgPool) {
    let router = build_router(pool);
    let (status, body) = get_json(&router, "/tags").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["tags"].as_array().expect("dizi").is_empty(), "{body}");
    assert!(body["next_cursor"].is_null(), "{body}");
}

// --- GET /tags/search ------------------------------------------------------

/// Planın kendi örneği: `?q=nv` → `nvidia`. Saf trigram bunu yakalayamıyor
/// (`similarity('nvidia','nv')` = 0.25 < 0.3 eşiği), önek eşleşmesi şart.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn arama_kisa_onek_ile_eslesiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "arayici").await;
    seed_post(&router, &api_key, "1", &["nvidia", "rust"]).await;

    let (status, body) = get_json(&router, "/tags/search?q=nv").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(tag_names(&body), vec!["nvidia"], "{body}");
}

/// Trigram'ın asıl işi: yazım hatasını toparlamak.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn arama_yazim_hatasini_trigram_ile_yakaliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yazim_hatasi").await;
    seed_post(&router, &api_key, "1", &["nvidia"]).await;

    let (status, body) = get_json(&router, "/tags/search?q=nvdia").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(tag_names(&body), vec!["nvidia"], "{body}");
}

/// Etiketler her zaman küçük harf saklanıyor ve `validate_tag_name` büyük
/// harfi *reddediyor*; arama tarafı sorguyu küçültmeseydi `?q=NV` boş liste
/// dönerdi.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn arama_buyuk_harfli_sorguyu_kucultuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "buyuk_harf").await;
    seed_post(&router, &api_key, "1", &["nvidia"]).await;

    let (status, body) = get_json(&router, "/tags/search?q=NV").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(tag_names(&body), vec!["nvidia"], "{body}");
}

/// Önek eşleşmesi, yalnızca trigram'la eşleşenden önce gelmeli.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn arama_onek_eslesmesini_one_aliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "onek_sirasi").await;
    seed_post(&router, &api_key, "1", &["rust", "trust"]).await;

    let (status, body) = get_json(&router, "/tags/search?q=rust").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let isimler = tag_names(&body);
    assert_eq!(isimler[0], "rust", "önekle eşleşen önce gelmeli: {body}");
    assert!(isimler.contains(&"trust"), "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn arama_gecersiz_sorguda_bos_liste_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "gecersiz_arama").await;
    seed_post(&router, &api_key, "1", &["nvidia"]).await;

    // Etiket kurallarına hiç uymayan bir sorgu: hata değil, boş liste.
    let (status, body) = get_json(&router, "/tags/search?q=%40%40%40").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["tags"].as_array().expect("dizi").is_empty(), "{body}");

    // `q` hiç verilmemiş.
    let (status, body) = get_json(&router, "/tags/search").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["tags"].as_array().expect("dizi").is_empty(), "{body}");
}

// --- GET /tags/{name}/posts ------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn etiketteki_postlar_donuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "etiket_postlari").await;

    seed_post(&router, &api_key, "nvidia yazısı", &["nvidia"]).await;
    seed_post(&router, &api_key, "alakasız", &["linux"]).await;

    let (status, body) = get_json(&router, "/tags/nvidia/posts").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let postlar = body["posts"].as_array().expect("posts dizi");
    assert_eq!(postlar.len(), 1, "{body}");
    assert_eq!(postlar[0]["title"], "nvidia yazısı", "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn olmayan_etiket_404_bos_etiket_bos_liste(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "bos_etiket").await;

    // Hiç var olmayan etiket → 404.
    let (status, body) = get_json(&router, "/tags/hicyok/posts").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Var olan ama tek post'u silinmiş etiket → boş liste, 404 değil.
    let post_id = seed_post(&router, &api_key, "silinecek", &["bosalan"]).await;
    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{post_id}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = get_json(&router, "/tags/bosalan/posts").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["posts"].as_array().expect("dizi").is_empty(), "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn etiket_postlari_sort_top_skora_gore(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let codec = IdCodec::new(&test_config().security.id_obfuscation_key).expect("anahtar");
    let (_, api_key) = seed_actor(&raw_pool, "skorlu_etiket").await;

    // Skorlar oluşturma sırasının tersi, `top` ve `new` ayırt edilebilsin.
    let eski = seed_post(&router, &api_key, "eski ama yüksek", &["skor"]).await;
    let yeni = seed_post(&router, &api_key, "yeni ama düşük", &["skor"]).await;

    for (ext, score) in [(&eski, 40), (&yeni, 2)] {
        let id = codec
            .decode::<actos_core::id::Content>(ext)
            .expect("id çözülebilmeli");
        sqlx::query!(r#"UPDATE contents SET score = $2 WHERE id = $1"#, id, score)
            .execute(&raw_pool)
            .await
            .expect("skor yazılabilmeli");
    }

    let (status, body) = get_json(&router, "/tags/skor/posts?sort=top").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["posts"][0]["title"], "eski ama yüksek", "{body}");
    assert_eq!(body["posts"][1]["title"], "yeni ama düşük", "{body}");

    let (status, body) = get_json(&router, "/tags/skor/posts?sort=new").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["posts"][0]["title"], "yeni ama düşük", "{body}");
    assert_eq!(body["posts"][1]["title"], "eski ama yüksek", "{body}");
}

/// `sort=hot` bugünden çalışıyor; `hot_score` Faz 12'ye kadar her satırda
/// `0` olduğu için sıralama pratikte `id DESC`'e düşüyor. Test ucun
/// çalıştığını ve cursor'ının tutarlı olduğunu doğruluyor, sıralamanın
/// anlamlılığını değil.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn etiket_postlari_sort_hot_calisiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "sicak_etiket").await;

    seed_post(&router, &api_key, "bir", &["sicak"]).await;
    seed_post(&router, &api_key, "iki", &["sicak"]).await;

    let (status, body) = get_json(&router, "/tags/sicak/posts?sort=hot&limit=1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["posts"].as_array().expect("dizi").len(), 1, "{body}");

    let cursor = body["next_cursor"].as_str().expect("cursor").to_owned();
    let (status, body2) = get_json(
        &router,
        &format!("/tags/sicak/posts?sort=hot&limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body2}");
    assert_eq!(body2["posts"].as_array().expect("dizi").len(), 1, "{body2}");
    assert_ne!(
        body["posts"][0]["id"], body2["posts"][0]["id"],
        "ikinci sayfa farklı bir post getirmeli: {body2}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn etiket_postlari_gecersiz_sort_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "gecersiz_sortlu").await;
    seed_post(&router, &api_key, "1", &["sortlu"]).await;

    let (status, body) = get_json(&router, "/tags/sortlu/posts?sort=populer").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

/// Cursor imzası sıralamayı taşıyor: `new` cursor'ı `sort=top` ile
/// kullanılamaz.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn etiket_postlari_uyusmayan_cursor_reddediliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "cursor_uyum").await;

    seed_post(&router, &api_key, "1", &["cur"]).await;
    seed_post(&router, &api_key, "2", &["cur"]).await;

    let (_, sayfa1) = get_json(&router, "/tags/cur/posts?sort=new&limit=1").await;
    let cursor = sayfa1["next_cursor"].as_str().expect("cursor").to_owned();

    let (status, body) = get_json(
        &router,
        &format!("/tags/cur/posts?sort=top&limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn etiket_postlari_fields_ile_filtreleniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "etiket_fields").await;
    seed_post(&router, &api_key, "başlık", &["alanli"]).await;

    let (status, body) = get_json(&router, "/tags/alanli/posts?fields=id,title").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ilk = &body["posts"][0];
    assert!(ilk.get("id").is_some(), "{body}");
    assert!(ilk.get("title").is_some(), "{body}");
    assert!(
        ilk.get("body").is_none(),
        "istenmeyen alan gelmemeli: {body}"
    );
    assert!(
        ilk.get("score").is_none(),
        "istenmeyen alan gelmemeli: {body}"
    );
}
