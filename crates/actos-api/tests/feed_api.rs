//! `crates/actos-api/routes/feed.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/posts_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! Oy sayaçlarının tutarlılığı, eşzamanlılık ve `hot_score` hesabı burada
//! değil, `crates/actos-core/tests/interaction.rs`'te: onlar domain
//! davranışı, HTTP'ye ihtiyaç duymuyorlar. Buradaki testler HTTP katmanına
//! özgü olanlar — durum kodları, kimlik zorunluluğu, dış id çözümü ve
//! query parametrelerinin ayrıştırılması.

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
    seed_actor_typed(pool, username, ActorType::Human).await
}

/// [`seed_actor`] ile aynı, ama `actor_type`'ı seçebiliyor —
/// `?actor_type=` filtre testleri için (bkz. aşağıdaki
/// `feed_actor_type_ile_filtreleniyor` grubu).
#[allow(clippy::expect_used)]
async fn seed_actor_typed(pool: &PgPool, username: &str, actor_type: ActorType) -> (i64, String) {
    let reg = core_auth::register(pool, username, actor_type, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    (reg.actor.id, reg.api_key)
}

/// Bir post oluşturup dış id'sini döner.
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

async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
    let (status, body, _) = send(router, empty_req("GET", uri)).await;
    (status, body)
}

/// Yanıttaki post başlıklarını sırasıyla döner.
#[allow(clippy::expect_used)]
fn basliklar(body: &Value) -> Vec<&str> {
    body["posts"]
        .as_array()
        .expect("posts dizi olmalı")
        .iter()
        .map(|p| p["title"].as_str().expect("title string olmalı"))
        .collect()
}

// --- GET /feed -------------------------------------------------------------

/// Ana akış **kimliksiz** erişilebilir olmalı — platformun ana sayfası
/// ajanlara ve anonim istemcilere açık.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_kimliksiz_erisilebiliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "feed_yazar").await;
    seed_post(&router, &api_key, "birinci").await;

    let (status, body) = get_json(&router, "/feed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body), vec!["birinci"], "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_yorumlari_icermiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "feed_yorum_yazar").await;
    let post = seed_post(&router, &api_key, "post").await;

    let (status, _, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post}/comments"),
            &api_key,
            json!({ "body": "yorum" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = get_json(&router, "/feed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let postlar = body["posts"].as_array().expect("dizi");
    assert_eq!(postlar.len(), 1, "feed'de yalnızca post olmalı: {body}");
    assert_eq!(postlar[0]["content_type"], "post", "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_silinmis_postu_gostermiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "feed_silme").await;
    seed_post(&router, &api_key, "kalan").await;
    let silinen = seed_post(&router, &api_key, "silinen").await;

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{silinen}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = get_json(&router, "/feed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body), vec!["kalan"], "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_sort_top_skora_gore(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "feed_skor_yazar").await;
    let (_, oylayan_key) = seed_actor(&raw_pool, "feed_skor_oylayan").await;

    let eski = seed_post(&router, &yazar_key, "eski ama oylu").await;
    seed_post(&router, &yazar_key, "yeni ama oysuz").await;

    let (status, _, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{eski}/vote"),
            &oylayan_key,
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = get_json(&router, "/feed?sort=top").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body)[0], "eski ama oylu", "{body}");

    let (status, body) = get_json(&router, "/feed?sort=new").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body)[0], "yeni ama oysuz", "{body}");
}

/// Düzeltilmiş formülün asıl sınandığı yer: oy almamış **yeni** bir post,
/// oy almış ama eski bir posttan yukarıda olmalı. Planın ilk formülünde
/// oysuz post `hot_score = 0` alıp dibe düşerdi.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_sort_hot_yeni_oysuz_postu_gomulmuyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "hot_feed_yazar").await;
    let (_, oylayan_key) = seed_actor(&raw_pool, "hot_feed_oylayan").await;

    let eski = seed_post(&router, &yazar_key, "eski oylu").await;

    let (status, _, _) = send(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{eski}/vote"),
            &oylayan_key,
            json!({ "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Eski post'u üç gün geriye al ve hot_score'unu o tarihe göre yeniden
    // hesapla (oy anında hesaplanmıştı, tarih sonradan değişti).
    sqlx::query!(
        r#"
        UPDATE contents
        SET created_at = now() - interval '3 days',
            hot_score = (
                sign(score) * log(greatest(abs(score), 1)::numeric)
                + extract(epoch FROM (now() - interval '3 days')) / 45000.0
            )::double precision
        WHERE title = 'eski oylu'
        "#
    )
    .execute(&raw_pool)
    .await
    .expect("eski post geriye alınabilmeli");

    let yeni = seed_post(&router, &yazar_key, "yeni oysuz").await;

    // Yeni post şema varsayılanıyla (0) başlıyor; tazeleme onu hesaplıyor.
    let guncellenen = actos_core::feed::recompute_hot_scores(&raw_pool)
        .await
        .expect("tazeleme çalışabilmeli");
    assert!(guncellenen >= 1, "tazeleme en az bir satır güncellemeli");

    let (status, body) = get_json(&router, "/feed?sort=hot").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        basliklar(&body)[0],
        "yeni oysuz",
        "oy almamış yeni post, üç günlük tek oylu postun üstünde olmalı: {body}"
    );
    assert!(!yeni.is_empty());
}

/// Trust level was removed (see REFACTOR.md §3): `hot` used to require the
/// author's `trust_level >= 1`, and a fresh account's post never showed
/// up. This test now verifies the exact opposite — a brand-new account's
/// post shows up in `hot` (and of course in `new`) with no waiting at all.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_taze_hesabin_postu_hotta_da_aninda_goruyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, taze_key) = seed_actor(&raw_pool, "hot_taze_yazar").await;

    seed_post(&router, &taze_key, "taze hesabin postu").await;

    // Refresh the time term so the post gets a hot_score (no votes, at the
    // schema default of 0).
    actos_core::feed::recompute_hot_scores(&raw_pool)
        .await
        .expect("tazeleme çalışabilmeli");

    let (status, body) = get_json(&router, "/feed?sort=hot").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        basliklar(&body),
        vec!["taze hesabin postu"],
        "yeni bir hesabın postu artık hot'ta anında görünmeli: {body}"
    );

    let (status, body) = get_json(&router, "/feed?sort=new").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body), vec!["taze hesabin postu"]);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_window_eski_postlari_eliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "feed_pencere").await;

    seed_post(&router, &api_key, "yeni").await;
    seed_post(&router, &api_key, "eski").await;

    sqlx::query!(
        r#"UPDATE contents SET created_at = now() - interval '40 days' WHERE title = 'eski'"#
    )
    .execute(&raw_pool)
    .await
    .expect("tarih geriye alınabilmeli");

    let (status, body) = get_json(&router, "/feed?window=month").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body), vec!["yeni"], "{body}");

    let (status, body) = get_json(&router, "/feed?window=all").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body).len(), 2, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_gecersiz_sort_ve_window_400(pool: PgPool) {
    let router = build_router(pool);

    let (status, body) = get_json(&router, "/feed?sort=populer").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");

    let (status, body) = get_json(&router, "/feed?window=yil").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

/// Faz 18.A: `?actor_type=` yazarın actor_type'ına göre süzüyor.
/// `NOTES.md` §8.1 — bu filtre bir garanti değil kolaylık, `actor_type`
/// kendi beyanı (bkz. `docs/API.md` §3.8), ama uçtan doğru şekilde
/// uygulandığını doğrulamak yine de gerekiyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_actor_type_ile_filtreleniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, insan_key) = seed_actor(&raw_pool, "feed_tur_insan").await;
    let (_, ajan_key) = seed_actor_typed(&raw_pool, "feed_tur_ajan", ActorType::AiAgent).await;

    seed_post(&router, &insan_key, "insan postu").await;
    seed_post(&router, &ajan_key, "ajan postu").await;

    let (status, body) = get_json(&router, "/feed?actor_type=ai_agent").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body), vec!["ajan postu"], "{body}");

    let (status, body) = get_json(&router, "/feed?actor_type=human").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body), vec!["insan postu"], "{body}");

    // Filtresiz: ikisi de görünür — filtrenin gerçekten filtrelediğini,
    // varsayılan davranışın kısıtlanmadığını doğruluyor.
    let (status, body) = get_json(&router, "/feed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body).len(), 2, "{body}");
}

/// Geçersiz bir `actor_type` sessizce yok sayılmamalı — `sort`/`window` ile
/// aynı sözleşme (bkz. `feed_gecersiz_sort_ve_window_400`).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_gecersiz_actor_type_400(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "feed_tur_gecersiz").await;

    let (status, body) = get_json(&router, "/feed?actor_type=robot").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");

    // `/feed/following` de aynı `FeedQuery`'yi kullanıyor — kimlik doğru
    // olsa bile (`CurrentActor` extractor'ı geçse bile) geçersiz
    // `actor_type` yine `400` vermeli.
    let (status, body, _) = send(
        &router,
        auth_req("GET", "/feed/following?actor_type=robot", &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

/// `follower` filtresindeki `takip_akisi_cursorla_sayfalaniyor`'un
/// `actor_type` için tekrarı: filtreyle birlikte sayfalama öğe
/// atlamamalı/tekrarlamamalı. `page` CTE'sinin içine giren yeni koşulun
/// (bkz. `actos_core::feed::list_feed` doküman yorumu) sayfalama
/// doğruluğunu bozmadığını doğruluyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_actor_type_ile_cursorla_sayfalaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, ajan_key) =
        seed_actor_typed(&raw_pool, "feed_tur_sayfa_ajan", ActorType::AiAgent).await;
    let (_, insan_key) = seed_actor(&raw_pool, "feed_tur_sayfa_insan").await;

    // Aralara insan postları serpiştiriliyor — filtre gerçekten
    // `actor_type`'a göre süzmüyor olsaydı sayfalama farklı sonuç verirdi.
    seed_post(&router, &ajan_key, "ajan post 1").await;
    seed_post(&router, &insan_key, "insan araya girdi 1").await;
    seed_post(&router, &ajan_key, "ajan post 2").await;
    seed_post(&router, &insan_key, "insan araya girdi 2").await;
    seed_post(&router, &ajan_key, "ajan post 3").await;

    let (status, sayfa1) = get_json(&router, "/feed?actor_type=ai_agent&limit=2").await;
    assert_eq!(status, StatusCode::OK, "{sayfa1}");
    assert_eq!(
        basliklar(&sayfa1),
        vec!["ajan post 3", "ajan post 2"],
        "{sayfa1}"
    );

    let cursor = sayfa1["next_cursor"].as_str().expect("cursor").to_owned();
    let (status, sayfa2) = get_json(
        &router,
        &format!("/feed?actor_type=ai_agent&limit=2&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sayfa2}");
    assert_eq!(basliklar(&sayfa2), vec!["ajan post 1"], "{sayfa2}");
    assert!(sayfa2["next_cursor"].is_null(), "{sayfa2}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_cursorla_sayfalaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "feed_sayfa").await;

    for i in 1..=3 {
        seed_post(&router, &api_key, &format!("post {i}")).await;
    }

    let (status, sayfa1) = get_json(&router, "/feed?limit=2").await;
    assert_eq!(status, StatusCode::OK, "{sayfa1}");
    assert_eq!(basliklar(&sayfa1), vec!["post 3", "post 2"], "{sayfa1}");

    let cursor = sayfa1["next_cursor"].as_str().expect("cursor").to_owned();
    let (status, sayfa2) = get_json(&router, &format!("/feed?limit=2&cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK, "{sayfa2}");
    assert_eq!(basliklar(&sayfa2), vec!["post 1"], "{sayfa2}");
    assert!(sayfa2["next_cursor"].is_null(), "{sayfa2}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_fields_ile_filtreleniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "feed_fields").await;
    seed_post(&router, &api_key, "başlık").await;

    let (status, body) = get_json(&router, "/feed?fields=id,title").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ilk = &body["posts"][0];
    assert!(ilk.get("id").is_some(), "{body}");
    assert!(ilk.get("title").is_some(), "{body}");
    assert!(ilk.get("body").is_none(), "{body}");
}

// --- GET /feed/following ---------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn takip_akisi_yalnizca_takip_edilenleri_gosteriyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, okuyan_key) = seed_actor(&raw_pool, "akis_okuyan").await;
    let (_, takip_key) = seed_actor(&raw_pool, "akis_takip_edilen").await;
    let (_, yabanci_key) = seed_actor(&raw_pool, "akis_yabanci").await;

    seed_post(&router, &takip_key, "takip ettiğim").await;
    seed_post(&router, &yabanci_key, "takip etmediğim").await;

    let (status, _, _) = send(
        &router,
        auth_req("PUT", "/actors/akis_takip_edilen/follow", &okuyan_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(&router, auth_req("GET", "/feed/following", &okuyan_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body), vec!["takip ettiğim"], "{body}");

    // Genel feed ikisini de gösteriyor — filtre gerçekten takip listesinden.
    let (status, body) = get_json(&router, "/feed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(basliklar(&body).len(), 2, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn takip_akisi_kimliksiz_401(pool: PgPool) {
    let router = build_router(pool);
    let (status, body) = get_json(&router, "/feed/following").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

/// Kimseyi takip etmeyen bir actor boş liste alır, hata değil.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn takip_akisi_bos_liste_donebiliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "akis_yalniz").await;
    let (_, baska_key) = seed_actor(&raw_pool, "akis_baskasi").await;
    seed_post(&router, &baska_key, "kimsenin takip etmediği").await;

    let (status, body, _) = send(&router, auth_req("GET", "/feed/following", &api_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["posts"].as_array().expect("dizi").is_empty(), "{body}");
}

/// `list_feed`'in Faz 17'de iki aşamalı sorguya çevrilmesi `follower`
/// filtreli sayfalamanın davranışını değiştirmemeli — bu test tam olarak
/// onu doğruluyor: `feed_cursorla_sayfalaniyor`'un genel feed için yaptığını
/// `follower` dolu uç için tekrarlıyor (tekrar yok, atlama yok, son sayfada
/// `next_cursor` yok).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn takip_akisi_cursorla_sayfalaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, okuyan_key) = seed_actor(&raw_pool, "akis_sayfa_okuyan").await;
    let (_, takip_key) = seed_actor(&raw_pool, "akis_sayfa_takip").await;

    let (status, _, _) = send(
        &router,
        auth_req("PUT", "/actors/akis_sayfa_takip/follow", &okuyan_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    for i in 1..=3 {
        seed_post(&router, &takip_key, &format!("takip post {i}")).await;
    }

    let (status, sayfa1, _) = send(
        &router,
        auth_req("GET", "/feed/following?limit=2", &okuyan_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sayfa1}");
    assert_eq!(
        basliklar(&sayfa1),
        vec!["takip post 3", "takip post 2"],
        "{sayfa1}"
    );

    let cursor = sayfa1["next_cursor"].as_str().expect("cursor").to_owned();
    let (status, sayfa2, _) = send(
        &router,
        auth_req(
            "GET",
            &format!("/feed/following?limit=2&cursor={cursor}"),
            &okuyan_key,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sayfa2}");
    assert_eq!(basliklar(&sayfa2), vec!["takip post 1"], "{sayfa2}");
    assert!(sayfa2["next_cursor"].is_null(), "{sayfa2}");
}
