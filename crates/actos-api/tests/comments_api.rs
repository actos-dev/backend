//! `crates/actos-api/routes/comments.rs` entegrasyon testleri.
//!
//! Kurulum yardımcıları `tests/posts_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//!
//! **Ağaç testlerinin okunması:** `GET /posts/{id}/comments` iç içe bir
//! yapı döner ve `actos_types::content::CommentNodeResponse` düğümün
//! alanlarını `flatten` ile açar — yani `node["body"]` doğrudan çalışır,
//! `node["content"]["body"]` değil. `node["replies"]` alt ağaçtır.

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

/// Bir yorum oluşturup dış id'sini döner. `parent` verilmezse post'un
/// doğrudan çocuğu olur.
#[allow(clippy::expect_used)]
async fn seed_comment(
    router: &Router,
    api_key: &str,
    post_id: &str,
    parent: Option<&str>,
    body_text: &str,
) -> String {
    let payload = match parent {
        Some(p) => json!({ "body": body_text, "parent_id": p }),
        None => json!({ "body": body_text }),
    };
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            api_key,
            payload,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "yorum oluşturulamadı: {body}");
    body["id"].as_str().expect("yorum id").to_owned()
}

/// `GET /posts/{id}/comments` ağacını çeker.
async fn fetch_tree(router: &Router, post_id: &str, query: &str) -> (StatusCode, Value) {
    let uri = if query.is_empty() {
        format!("/posts/{post_id}/comments")
    } else {
        format!("/posts/{post_id}/comments?{query}")
    };
    let (status, body, _) = send(router, empty_req("GET", &uri)).await;
    (status, body)
}

/// Bir içeriğin `comment_count`'unu doğrudan veritabanından okur — sayaç
/// güncellemesini API yanıtına değil, kaynağına bakarak doğrulamak için.
#[allow(clippy::expect_used)]
async fn comment_count_of(pool: &PgPool, id_codec: &IdCodec, external_id: &str) -> i32 {
    let id = id_codec
        .decode::<actos_core::id::Content>(external_id)
        .expect("dış id çözülebilmeli");
    sqlx::query_scalar!(
        r#"SELECT comment_count AS "comment_count!" FROM contents WHERE id = $1"#,
        id
    )
    .fetch_one(pool)
    .await
    .expect("comment_count okunabilmeli")
}

/// Testlerin dış id çözmek için kullandığı kodlayıcı; `test_config` ile
/// aynı anahtardan türetiliyor, dolayısıyla router'ınkiyle birebir aynı.
#[allow(clippy::expect_used)]
fn test_id_codec() -> IdCodec {
    IdCodec::new(&test_config().security.id_obfuscation_key).expect("geçerli anahtar")
}

// --- POST /posts/{id}/comments ---------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yorum_olusturma_201_location_ve_govde_dogru(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yorumcu").await;
    let post_id = seed_post(&router, &api_key, "Ana post").await;

    let (status, body, headers) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            &api_key,
            json!({ "body": "İlk yorum" }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["body"], "İlk yorum");
    assert_eq!(body["content_type"], "comment");
    assert!(body["title"].is_null(), "yorumun başlığı olmamalı: {body}");
    assert_eq!(body["deleted"], false);
    assert_eq!(body["comment_count"], 0);
    assert!(
        body["id"].as_str().is_some_and(|s| s.starts_with("c_")),
        "yorum id'si c_ önekiyle başlamalı: {body}"
    );

    let location = headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("Location header'ı olmalı");
    assert_eq!(
        location,
        format!("/comments/{}", body["id"].as_str().unwrap())
    );
}

/// `comment_count` yalnızca doğrudan ebeveynde değil, **tüm atalarda**
/// artmalı — `ltree`'nin `@>` operatörüyle yapılan güncellemenin asıl
/// sınandığı yer burası.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yanit_zinciri_tum_atalarda_comment_count_artiriyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let codec = test_id_codec();
    let (_, api_key) = seed_actor(&raw_pool, "zincirci").await;

    let post_id = seed_post(&router, &api_key, "Zincir").await;
    let c1 = seed_comment(&router, &api_key, &post_id, None, "birinci").await;
    let c2 = seed_comment(&router, &api_key, &post_id, Some(&c1), "ikinci").await;
    let c3 = seed_comment(&router, &api_key, &post_id, Some(&c2), "üçüncü").await;

    assert_eq!(
        comment_count_of(&raw_pool, &codec, &post_id).await,
        3,
        "post üç yorumun da atası"
    );
    assert_eq!(comment_count_of(&raw_pool, &codec, &c1).await, 2);
    assert_eq!(comment_count_of(&raw_pool, &codec, &c2).await, 1);
    assert_eq!(
        comment_count_of(&raw_pool, &codec, &c3).await,
        0,
        "yaprak düğümün altında yorum yok"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn baska_postun_yorumuna_yanit_404_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yabanci_thread").await;

    let post_a = seed_post(&router, &api_key, "A postu").await;
    let post_b = seed_post(&router, &api_key, "B postu").await;
    let yorum_a = seed_comment(&router, &api_key, &post_a, None, "A'nın yorumu").await;

    // B post'unun altına, A'nın yorumuna yanıt vermeye çalış.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_b}/comments"),
            &api_key,
            json!({ "body": "olmaz", "parent_id": yorum_a }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_posta_yorum_410_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "silen").await;
    let post_id = seed_post(&router, &api_key, "Silinecek").await;

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/posts/{post_id}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            &api_key,
            json!({ "body": "geç kaldım" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
}

/// Şema (`ck_contents_depth`) 32 seviyeye izin veriyor; 33'üncü reddedilmeli
/// ve bu `500` değil, anlaşılır bir `400` olmalı — trigger'a düşmeden önce
/// `create_comment`'in önden yaptığı kontrol (bkz.
/// `actos_core::comment` modül dokümantasyonu) tam olarak bunun için var.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn derinlik_limiti_asimi_400_ile_reddediliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_actor_id, api_key) = seed_actor(&raw_pool, "derinlesen").await;
    let post_id = seed_post(&router, &api_key, "Derin").await;

    // This test checks the depth limit, not rate limiting — firing 32
    // comments from a single actor in one hour is well under the capacity
    // of the single `comment` bucket after trust level was removed
    // (200/hour, see `actos_core::config::LimitTable::from_env`), so
    // there's no need for a separate tier adjustment anymore.

    // depth 1..=32 → 32 yorum. Post depth 0.
    let mut parent: Option<String> = None;
    for i in 1..=32 {
        let id = seed_comment(
            &router,
            &api_key,
            &post_id,
            parent.as_deref(),
            &format!("seviye {i}"),
        )
        .await;
        parent = Some(id);
    }

    // 33'üncü seviye sınırı aşıyor.
    let (status, body, _) = send(
        &router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            &api_key,
            json!({ "body": "seviye 33", "parent_id": parent.unwrap() }),
        ),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "derinlik aşımı 500 değil 400 olmalı: {body}"
    );
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

// --- GET /posts/{id}/comments (ağaç) ---------------------------------------

/// Planın açıkça istediği test: 5 seviye derin bir ağaç kurup doğru sırada
/// ve doğru iç içelikte döndüğünü doğrula.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bes_seviye_derin_agac_ic_ice_ve_dogru_sirada_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "agacci").await;
    let post_id = seed_post(&router, &api_key, "Ağaç").await;

    let mut parent: Option<String> = None;
    let mut beklenen = Vec::new();
    for i in 1..=5 {
        let govde = format!("seviye {i}");
        let id = seed_comment(&router, &api_key, &post_id, parent.as_deref(), &govde).await;
        beklenen.push(govde);
        parent = Some(id);
    }

    let (status, tree) = fetch_tree(&router, &post_id, "").await;
    assert_eq!(status, StatusCode::OK, "{tree}");

    // Kökten yaprağa yürü: her seviyede tam bir yanıt olmalı.
    let mut node = &tree["comments"][0];
    for (i, govde) in beklenen.iter().enumerate() {
        assert_eq!(
            node["body"],
            govde.as_str(),
            "seviye {} gövdesi: {tree}",
            i + 1
        );
        assert_eq!(node["content_type"], "comment");
        let replies = node["replies"].as_array().expect("replies dizi olmalı");
        if i + 1 == beklenen.len() {
            assert!(
                replies.is_empty(),
                "en derin düğümün yanıtı olmamalı: {tree}"
            );
        } else {
            assert_eq!(replies.len(), 1, "seviye {} tek yanıt: {tree}", i + 1);
            node = &node["replies"][0];
        }
    }

    assert_eq!(
        tree["comments"].as_array().expect("comments dizi").len(),
        1,
        "yalnızca bir üst seviye yorum var: {tree}"
    );
}

/// `?depth=` sınırının altında kalan seviyeler dönmemeli — istemci daha
/// derini `?parent=` ile ayrıca çeker.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn depth_siniri_derin_seviyeleri_kesiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "kesici").await;
    let post_id = seed_post(&router, &api_key, "Kesik").await;

    let c1 = seed_comment(&router, &api_key, &post_id, None, "bir").await;
    let c2 = seed_comment(&router, &api_key, &post_id, Some(&c1), "iki").await;
    let _c3 = seed_comment(&router, &api_key, &post_id, Some(&c2), "üç").await;

    let (status, tree) = fetch_tree(&router, &post_id, "depth=1").await;
    assert_eq!(status, StatusCode::OK, "{tree}");

    let seviye1 = &tree["comments"][0];
    assert_eq!(seviye1["body"], "bir");
    let seviye2 = &seviye1["replies"][0];
    assert_eq!(seviye2["body"], "iki");
    assert!(
        seviye2["replies"].as_array().expect("replies").is_empty(),
        "depth=1 üçüncü seviyeyi getirmemeli: {tree}"
    );
}

/// "Daha fazla yanıt yükle": `?parent=` ile bir alt ağaç ayrıca çekilebilir.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn parent_ile_alt_agac_ayrica_cekilebilir(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "altagac").await;
    let post_id = seed_post(&router, &api_key, "Alt ağaç").await;

    let c1 = seed_comment(&router, &api_key, &post_id, None, "bir").await;
    let c2 = seed_comment(&router, &api_key, &post_id, Some(&c1), "iki").await;
    let _c3 = seed_comment(&router, &api_key, &post_id, Some(&c2), "üç").await;

    let (status, tree) = fetch_tree(&router, &post_id, &format!("parent={c2}")).await;
    assert_eq!(status, StatusCode::OK, "{tree}");

    let ust = tree["comments"].as_array().expect("comments dizi");
    assert_eq!(ust.len(), 1, "c2'nin tek çocuğu var: {tree}");
    assert_eq!(ust[0]["body"], "üç");
}

/// Silinen yorum ağaçtan düşmüyor: `[deleted]` gövdesiyle yerinde kalıyor ve
/// çocuğu erişilebilir olmaya devam ediyor. Planın "silinen yorumun
/// çocukları yaşamaya devam eder" maddesi.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinen_yorumun_cocuklari_yasamaya_devam_eder(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "silinen_dal").await;
    let post_id = seed_post(&router, &api_key, "Dal").await;

    let ebeveyn = seed_comment(&router, &api_key, &post_id, None, "ebeveyn gövdesi").await;
    let _cocuk = seed_comment(&router, &api_key, &post_id, Some(&ebeveyn), "çocuk gövdesi").await;

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/comments/{ebeveyn}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, tree) = fetch_tree(&router, &post_id, "").await;
    assert_eq!(status, StatusCode::OK, "{tree}");

    let dugum = &tree["comments"][0];
    assert_eq!(dugum["deleted"], true, "{tree}");
    assert_eq!(dugum["body"], "[deleted]", "{tree}");
    assert!(
        !tree.to_string().contains("ebeveyn gövdesi"),
        "silinmiş yorumun gerçek gövdesi sızmamalı: {tree}"
    );
    assert_eq!(
        dugum["replies"][0]["body"], "çocuk gövdesi",
        "çocuk yaşamaya devam etmeli: {tree}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sort_top_skora_gore_siraliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let codec = test_id_codec();
    let (_, api_key) = seed_actor(&raw_pool, "skorcu").await;
    let post_id = seed_post(&router, &api_key, "Skor").await;

    // Skorlar oluşturma sırasının **tersi**: böylece `top` ve `new` farklı
    // sıralar üretiyor ve iki iddia da gerçekten ayırt edici oluyor. Aynı
    // sırayı üretselerdi `new` doğrulaması hiçbir şey kanıtlamazdı.
    let eski = seed_comment(&router, &api_key, &post_id, None, "eski ama yüksek").await;
    let yeni = seed_comment(&router, &api_key, &post_id, None, "yeni ama düşük").await;

    // Oylama Faz 11'de; skoru doğrudan yazıyoruz.
    for (ext_id, score) in [(&eski, 50), (&yeni, 1)] {
        let id = codec
            .decode::<actos_core::id::Content>(ext_id)
            .expect("id çözülebilmeli");
        sqlx::query!(r#"UPDATE contents SET score = $2 WHERE id = $1"#, id, score)
            .execute(&raw_pool)
            .await
            .expect("skor yazılabilmeli");
    }

    let (status, tree) = fetch_tree(&router, &post_id, "sort=top").await;
    assert_eq!(status, StatusCode::OK, "{tree}");
    assert_eq!(tree["comments"][0]["body"], "eski ama yüksek", "{tree}");
    assert_eq!(tree["comments"][1]["body"], "yeni ama düşük", "{tree}");

    // Varsayılan sıralama `new`: en yeni önce, skordan bağımsız.
    let (status, tree) = fetch_tree(&router, &post_id, "").await;
    assert_eq!(status, StatusCode::OK, "{tree}");
    assert_eq!(tree["comments"][0]["body"], "yeni ama düşük", "{tree}");
    assert_eq!(tree["comments"][1]["body"], "eski ama yüksek", "{tree}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ust_seviye_yorumlar_cursorla_sayfalaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "sayfaci").await;
    let post_id = seed_post(&router, &api_key, "Sayfa").await;

    for i in 1..=3 {
        seed_comment(&router, &api_key, &post_id, None, &format!("yorum {i}")).await;
    }

    let (status, sayfa1) = fetch_tree(&router, &post_id, "limit=2").await;
    assert_eq!(status, StatusCode::OK, "{sayfa1}");
    assert_eq!(sayfa1["comments"].as_array().expect("dizi").len(), 2);
    let cursor = sayfa1["next_cursor"]
        .as_str()
        .expect("ikinci sayfa olmalı")
        .to_owned();

    let (status, sayfa2) = fetch_tree(&router, &post_id, &format!("limit=2&cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK, "{sayfa2}");
    let ikinci = sayfa2["comments"].as_array().expect("dizi");
    assert_eq!(ikinci.len(), 1, "geriye tek yorum kalmalı: {sayfa2}");
    assert!(
        sayfa2["next_cursor"].is_null(),
        "son sayfada cursor olmamalı: {sayfa2}"
    );

    // En yeni önce: ilk sayfa 3 ve 2, ikinci sayfa 1.
    assert_eq!(sayfa1["comments"][0]["body"], "yorum 3");
    assert_eq!(sayfa1["comments"][1]["body"], "yorum 2");
    assert_eq!(ikinci[0]["body"], "yorum 1");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gecersiz_sort_400_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "gecersiz_sort").await;
    let post_id = seed_post(&router, &api_key, "Sort").await;

    let (status, body) = fetch_tree(&router, &post_id, "sort=populer").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_FAILED", "{body}");
}

/// Bir cursor hangi sıralamaya ait olduğunu imzasında taşıyor; `new`
/// cursor'ı `sort=top` ile kullanılamaz — aksi hâlde istemci sayfaların
/// ortasında sessizce sıçrayan bir liste görürdü.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sort_ile_uyusmayan_cursor_reddediliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "cursor_uyusmaz").await;
    let post_id = seed_post(&router, &api_key, "Cursor").await;

    for i in 1..=3 {
        seed_comment(&router, &api_key, &post_id, None, &format!("y{i}")).await;
    }

    let (_, sayfa1) = fetch_tree(&router, &post_id, "limit=2").await;
    let new_cursor = sayfa1["next_cursor"].as_str().expect("cursor").to_owned();

    let (status, body) = fetch_tree(
        &router,
        &post_id,
        &format!("sort=top&limit=2&cursor={new_cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yorumsuz_post_bos_liste_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yorumsuz").await;
    let post_id = seed_post(&router, &api_key, "Sessiz").await;

    let (status, tree) = fetch_tree(&router, &post_id, "").await;
    assert_eq!(status, StatusCode::OK, "{tree}");
    assert!(tree["comments"].as_array().expect("dizi").is_empty());
    assert!(tree["next_cursor"].is_null());
}

// --- GET /comments/{id} ----------------------------------------------------

/// Breadcrumb kökten başlar (ilk öğe post) ve yorumun kendisini içermez.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_comment_breadcrumb_kokten_baslar(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "ekmekkirintisi").await;
    let post_id = seed_post(&router, &api_key, "Kök post").await;

    let c1 = seed_comment(&router, &api_key, &post_id, None, "bir").await;
    let c2 = seed_comment(&router, &api_key, &post_id, Some(&c1), "iki").await;
    let c3 = seed_comment(&router, &api_key, &post_id, Some(&c2), "üç").await;

    let (status, body, _) = send(&router, empty_req("GET", &format!("/comments/{c3}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(body["comment"]["body"], "üç");
    let atalar = body["ancestors"].as_array().expect("ancestors dizi");
    assert_eq!(atalar.len(), 3, "post + iki ata yorum: {body}");
    assert_eq!(atalar[0]["content_type"], "post", "{body}");
    assert_eq!(atalar[0]["title"], "Kök post");
    assert_eq!(atalar[1]["body"], "bir");
    assert_eq!(atalar[2]["body"], "iki");
}

/// `GET /posts/{id}` silinmiş post için `410` dönerken, `GET /comments/{id}`
/// silinmiş yorum için `200` + `[deleted]` döner — çocukları erişilebilir
/// kalmalı diye (bkz. `actos_core::comment::get_comment`).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_yorum_200_ile_maskeli_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "maskeli").await;
    let post_id = seed_post(&router, &api_key, "Maske").await;
    let c1 = seed_comment(&router, &api_key, &post_id, None, "gizli gövde").await;

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/comments/{c1}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(&router, empty_req("GET", &format!("/comments/{c1}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["comment"]["deleted"], true, "{body}");
    assert_eq!(body["comment"]["body"], "[deleted]", "{body}");
    assert!(
        !body.to_string().contains("gizli gövde"),
        "silinmiş yorumun gövdesi sızmamalı: {body}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn olmayan_yorum_404_doner(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/comments/c_zzzzzz")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

// --- PATCH / DELETE /comments/{id} -----------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_comment_sahibi_200_ve_edit_history_yaziliyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let codec = test_id_codec();
    let (_, api_key) = seed_actor(&raw_pool, "duzenleyen").await;
    let post_id = seed_post(&router, &api_key, "Düzenle").await;
    let c1 = seed_comment(&router, &api_key, &post_id, None, "eski gövde").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/comments/{c1}"),
            &api_key,
            json!({ "body": "yeni gövde" }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["body"], "yeni gövde");
    assert!(!body["edited_at"].is_null(), "edited_at dolmalı: {body}");

    let id = codec
        .decode::<actos_core::id::Content>(&c1)
        .expect("id çözülebilmeli");
    let onceki: String = sqlx::query_scalar!(
        r#"SELECT previous_body FROM edit_history WHERE content_id = $1"#,
        id
    )
    .fetch_one(&raw_pool)
    .await
    .expect("edit_history satırı olmalı");
    assert_eq!(onceki, "eski gövde");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn patch_comment_yabanci_403_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, sahip_key) = seed_actor(&raw_pool, "yorum_sahibi").await;
    let (_, yabanci_key) = seed_actor(&raw_pool, "yorum_yabanci").await;

    let post_id = seed_post(&router, &sahip_key, "Sahiplik").await;
    let c1 = seed_comment(&router, &sahip_key, &post_id, None, "benim").await;

    let (status, body, _) = send(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/comments/{c1}"),
            &yabanci_key,
            json!({ "body": "senin değil" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn delete_comment_moderator_204_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, yazar_key) = seed_actor(&raw_pool, "yorum_yazari").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "yorum_moderatoru").await;
    core_auth::grant_role(&raw_pool, mod_id, AdminRole::Moderator, None)
        .await
        .expect("moderatör rolü verilebilmeli");

    let post_id = seed_post(&router, &yazar_key, "Moderasyon").await;
    let c1 = seed_comment(&router, &yazar_key, &post_id, None, "başkasının yorumu").await;

    let (status, body, _) = send(
        &router,
        auth_req("DELETE", &format!("/comments/{c1}"), &mod_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

// --- GET /actors/{username}/comments (Faz 7'den devir) ---------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_yorumlari_silinmisleri_gostermez(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yorum_listesi").await;
    let post_id = seed_post(&router, &api_key, "Liste").await;

    let kalan = seed_comment(&router, &api_key, &post_id, None, "kalan yorum").await;
    let silinen = seed_comment(&router, &api_key, &post_id, None, "silinen yorum").await;

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/comments/{silinen}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(&router, empty_req("GET", "/actors/yorum_listesi/comments")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let yorumlar = body["comments"].as_array().expect("comments dizi");
    assert_eq!(yorumlar.len(), 1, "silinmiş yorum listede olmamalı: {body}");
    assert_eq!(yorumlar[0]["id"], kalan);
    assert_eq!(yorumlar[0]["body"], "kalan yorum");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_yorumlari_fields_ile_filtreleniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yorum_fields").await;
    let post_id = seed_post(&router, &api_key, "Alanlar").await;
    seed_comment(&router, &api_key, &post_id, None, "gövde").await;

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/yorum_fields/comments?fields=id,body"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ilk = &body["comments"][0];
    assert!(ilk.get("id").is_some(), "{body}");
    assert!(ilk.get("body").is_some(), "{body}");
    assert!(
        ilk.get("score").is_none(),
        "istenmeyen alan gelmemeli: {body}"
    );
    assert!(
        ilk.get("author").is_none(),
        "istenmeyen alan gelmemeli: {body}"
    );
}

// --- `body_html` (Faz 18.A, bkz. NOTES.md §8.3) -----------------------------

/// Tekil uç: `GET /comments/{id}` `body_html`'i her zaman doldurur (bkz.
/// `actos_types::content::ContentSummary::body_html` "Nerede dolu döner").
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn get_comment_body_html_her_zaman_dolu(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yorum_html").await;
    let post_id = seed_post(&router, &api_key, "HTML").await;
    let c1 = seed_comment(&router, &api_key, &post_id, None, "**kalın** gövde").await;

    let (status, body, _) = send(&router, empty_req("GET", &format!("/comments/{c1}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let html = body["comment"]["body_html"]
        .as_str()
        .expect("body_html string olmalı");
    assert!(html.contains("<strong>kalın</strong>"), "{html}");
}

/// Bugün silinmiş yorum `body = "[deleted]"` ile `200` dönüyor (bkz.
/// `silinmis_yorum_200_ile_maskeli_doner`) — `body_html` de aynı maskeleme
/// kuralına uymalı: ham gövde hiçbir şekilde sızmamalı (görev tanımı
/// madde 6).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_yorum_body_html_de_maskeli(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yorum_html_maskeli").await;
    let post_id = seed_post(&router, &api_key, "Maske HTML").await;
    let c1 = seed_comment(&router, &api_key, &post_id, None, "<script>gizli</script>").await;

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/comments/{c1}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body, _) = send(&router, empty_req("GET", &format!("/comments/{c1}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let html = body["comment"]["body_html"]
        .as_str()
        .expect("body_html string olmalı");
    assert!(
        html.contains("[deleted]"),
        "body_html body ile aynı maskeyi taşımalı: {html}"
    );
    assert!(
        !html.contains("script") && !html.contains("gizli"),
        "silinmiş yorumun ham gövdesi body_html'e de sızmamalı: {html}"
    );
}

/// Liste uçlarında `body_html` varsayılan olarak hesaplanmaz (görev tanımı
/// madde 4).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_yorumlari_body_html_varsayilan_hesaplanmiyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yorum_liste_html").await;
    let post_id = seed_post(&router, &api_key, "Liste HTML").await;
    seed_comment(&router, &api_key, &post_id, None, "**kalın**").await;

    let (status, body, _) = send(
        &router,
        empty_req("GET", "/actors/yorum_liste_html/comments"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["comments"][0]["body_html"].is_null(),
        "?fields= olmadan liste öğesinde body_html hesaplanmamalı: {body}"
    );
}

/// `?fields=body_html` liste ucunda hesaplamayı açıkça tetikler.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_yorumlari_fields_body_html_ile_hesaplaniyor(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "yorum_liste_html_fields").await;
    let post_id = seed_post(&router, &api_key, "Liste HTML Fields").await;
    seed_comment(&router, &api_key, &post_id, None, "**kalın**").await;

    let (status, body, _) = send(
        &router,
        empty_req(
            "GET",
            "/actors/yorum_liste_html_fields/comments?fields=id,body_html",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let html = body["comments"][0]["body_html"]
        .as_str()
        .expect("?fields=body_html ile string dönmeli");
    assert!(html.contains("<strong>kalın</strong>"), "{html}");
}

// --- GET /posts/{id}/comments `?body_html=` (ağaç ucuna opt-in) ------------
//
// Ağaç ucu `?fields=` desteklemiyor (bkz. `comments.rs::list_comments`
// dokümanı), o yüzden `body_html` burada ayrı bir `?body_html=true`
// bayrağıyla açılıyor. Aşağıdaki üç test görev tanımının istediği üç
// iddiayı karşılıyor: (1) parametresiz varsayılan `null`, (2) `true` ile
// ağacın her seviyesinde (en az iki seviye derinlikte) dolu, (3) silinmiş
// bir düğümde maskeleme kuralına uyuyor.

/// Parametresiz istekte ağaçtaki hiçbir düğümde `body_html` hesaplanmaz —
/// bu ucun bugünkü (parametre eklenmeden önceki) davranışı birebir korunuyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn agac_body_html_parametresiz_hicbir_dugumde_hesaplanmaz(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "agac_html_kapali").await;
    let post_id = seed_post(&router, &api_key, "Ağaç HTML kapalı").await;

    let ebeveyn = seed_comment(&router, &api_key, &post_id, None, "**ebeveyn**").await;
    seed_comment(&router, &api_key, &post_id, Some(&ebeveyn), "**çocuk**").await;

    let (status, tree) = fetch_tree(&router, &post_id, "").await;
    assert_eq!(status, StatusCode::OK, "{tree}");

    let dugum = &tree["comments"][0];
    assert!(
        dugum["body_html"].is_null(),
        "?body_html= olmadan kök düğümde hesaplanmamalı: {tree}"
    );
    assert!(
        dugum["replies"][0]["body_html"].is_null(),
        "?body_html= olmadan iç içe düğümde de hesaplanmamalı: {tree}"
    );
}

/// `?body_html=true` ağaçtaki **her** düğümde `body_html`'i doldurur —
/// yalnızca kökte değil, en az iki seviye derinlikte de (görev tanımı
/// madde 7'nin "en az 2 seviye derinlikte doğrula" isteği).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn agac_body_html_true_ile_ic_ice_dugumlerde_de_dolar(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "agac_html_acik").await;
    let post_id = seed_post(&router, &api_key, "Ağaç HTML açık").await;

    let seviye1 = seed_comment(&router, &api_key, &post_id, None, "**bir**").await;
    let seviye2 = seed_comment(&router, &api_key, &post_id, Some(&seviye1), "**iki**").await;
    seed_comment(&router, &api_key, &post_id, Some(&seviye2), "**üç**").await;

    let (status, tree) = fetch_tree(&router, &post_id, "body_html=true").await;
    assert_eq!(status, StatusCode::OK, "{tree}");

    let dugum1 = &tree["comments"][0];
    let html1 = dugum1["body_html"]
        .as_str()
        .expect("seviye 1'de body_html string olmalı");
    assert!(html1.contains("<strong>bir</strong>"), "seviye 1: {html1}");

    let dugum2 = &dugum1["replies"][0];
    let html2 = dugum2["body_html"]
        .as_str()
        .expect("seviye 2'de (iç içe) body_html string olmalı");
    assert!(html2.contains("<strong>iki</strong>"), "seviye 2: {html2}");

    let dugum3 = &dugum2["replies"][0];
    let html3 = dugum3["body_html"]
        .as_str()
        .expect("seviye 3'te (iki seviye iç içe) body_html string olmalı");
    assert!(html3.contains("<strong>üç</strong>"), "seviye 3: {html3}");
}

/// Ağaçta silinmiş bir düğüm için `?body_html=true` de aynı maskeleme
/// kuralına uymalı: `body_html` `"[deleted]"` gövdesinden türer, ham gövde
/// hiçbir şekilde sızmaz (bkz. `silinmis_yorum_body_html_de_maskeli` —
/// tekil uçtaki aynı iddianın ağaç ucundaki karşılığı).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn agac_body_html_true_silinen_dugumde_maskeli(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "agac_html_silinen").await;
    let post_id = seed_post(&router, &api_key, "Ağaç HTML silinen").await;

    let ebeveyn = seed_comment(&router, &api_key, &post_id, None, "<script>gizli</script>").await;
    seed_comment(&router, &api_key, &post_id, Some(&ebeveyn), "çocuk gövdesi").await;

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/comments/{ebeveyn}"), &api_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, tree) = fetch_tree(&router, &post_id, "body_html=true").await;
    assert_eq!(status, StatusCode::OK, "{tree}");

    let dugum = &tree["comments"][0];
    assert_eq!(dugum["deleted"], true, "{tree}");
    let html = dugum["body_html"]
        .as_str()
        .expect("silinmiş düğümde de body_html string olmalı (maskelenmiş içerikle)");
    assert!(
        html.contains("[deleted]"),
        "body_html body ile aynı maskeyi taşımalı: {html}"
    );
    assert!(
        !html.contains("script") && !html.contains("gizli"),
        "silinmiş düğümün ham gövdesi body_html'e sızmamalı: {html}"
    );

    // Çocuk yaşamaya devam ediyor ve o da (silinmemiş olduğu için) kendi
    // gerçek gövdesinden türeyen body_html'i taşıyor.
    let cocuk_html = dugum["replies"][0]["body_html"]
        .as_str()
        .expect("silinmemiş çocukta body_html string olmalı");
    assert!(cocuk_html.contains("çocuk gövdesi"), "{cocuk_html}");
}

/// `?body_html=` bool olmayan bir değerle gelirse `400` döner —
/// `gecersiz_sort_400_doner` ile aynı desen (`parse_body_html`'in kendi
/// hata yolu, bkz. `comments.rs`).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gecersiz_body_html_400_doner(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let (_, api_key) = seed_actor(&raw_pool, "gecersiz_body_html").await;
    let post_id = seed_post(&router, &api_key, "Geçersiz body_html").await;

    let (status, body) = fetch_tree(&router, &post_id, "body_html=evet").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}
