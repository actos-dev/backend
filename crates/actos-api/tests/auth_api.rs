//! `crates/actos-api/routes/auth.rs` entegrasyon testleri.
//!
//! Her test `#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]` ile kendi
//! izole Postgres veritabanını alır. Router `tower::ServiceExt::oneshot` ile
//! doğrudan çağrılır — hiçbir yerde gerçekten ağ dinlenmiyor.
//!
//! **`AppState` nasıl kuruldu:** Faz 6 ile birlikte `crate::middleware::
//! ratelimit::enforce` router'a bağlandı ve **her** istekte (sağlık/versiyon
//! uçları hariç) `actos_core::ratelimit::RateLimiter::check` üzerinden
//! gerçekten Redis'e gidiyor — auth uçları artık Redis'e "hiç dokunmuyor"
//! değil. Bu yüzden burada `crates/actos-core/tests/ratelimit.rs`'teki ile
//! aynı kalıp izleniyor: geliştirme ortamında zaten ayakta olan gerçek
//! Redis'e (`127.0.0.1:3102`, `docker-compose.yml`'deki `actos_redis`)
//! bağlanılıyor. Redis ayakta değilse bu dosyadaki testler de (tıpkı
//! `actos-core/tests/ratelimit.rs` gibi) başarısız olur — ayrı bir "atla"
//! mekanizması yok, çünkü rate limiting davranışının kendisi burada test
//! ediliyor (bkz. aşağıdaki "Hız sınırlama" bölümü) ve gerçek Redis olmadan
//! bunu sınamanın bir yolu yok.
//!
//! **Test izolasyonu:** testler paralel koşar ve hepsi aynı Redis'i
//! paylaşır. İki çakışma kaynağı var: (1) `#[sqlx::test]`'in verdiği her
//! izole Postgres veritabanı kendi otoincrement `id` sırasını `1`'den
//! başlatır, yani farklı testlerdeki actor'ler aynı `id`'yi alabilir; (2)
//! test harness'i gerçek bir soket açmadığı için `ConnectInfo` yok, tüm
//! kimliksiz istekler aynı `0.0.0.0` IP'sine düşer. İkisi de aynı Redis
//! kovasını (`rl:<scope>:<a|i>:<kimlik>`) paylaşan iki testin birbirinin
//! token'ını tüketip rastgele 429 almasına yol açar. Çözüm:
//! `build_router` her çağrıda `RateLimiter::with_prefix` ile **benzersiz**
//! bir anahtar önekiyle kurulur — her test kendi izole anahtar uzayında
//! çalışır, üretim anahtar şemasına dokunulmaz.
//!
//! Storage/S3'e hâlâ gerçekten **bağlanılmıyor** (o kısım hâlâ bilerek
//! erişilemez bir adrese ayarlanıyor — `actos_core::Storage::new`'in tembel
//! oluşu bunu güvenli kılıyor, bkz. `test_config`), ama Faz 18.A'dan beri
//! `whoami` `state.storage()` **çağırıyor**: avatar set edilmişse
//! `Storage::public_url` ile URL üretiyor. Bu saf bir string birleştirmesi
//! (ağa çıkmıyor), yani erişilemez adrese rağmen güvenli — bkz. `whoami_
//! avatar_url_set_edilmisse_dogru_url_doner`.

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    config::{
        DatabaseConfig, LimitTable, RedisConfig, SecurityConfig, ServerConfig, StorageConfig,
        StorageQuotaConfig,
    },
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

// --- Kurulum yardımcıları -------------------------------------------------

// `expect_used` lint'i test gövdelerinde muaf ama onların çağırdığı serbest
// fonksiyonlarda değil; bu dosyadaki diğer yardımcılarla aynı kalıp.
#[allow(clippy::expect_used)]
fn test_config() -> Config {
    Config {
        server: ServerConfig {
            addr: std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
            request_timeout: std::time::Duration::from_secs(30),
            max_concurrent_requests: 512,
            max_body_bytes: 1024 * 1024,
            max_upload_bytes: 8 * 1024 * 1024,
            // Bu testler `X-Forwarded-For` göndermiyor, değeri önemsiz.
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
            // Gerçek bağlantı `#[sqlx::test]`'in verdiği `PgPool` ile zaten
            // kurulu; bu alan yalnızca `Config`'in bir parçası olduğu için
            // dolduruluyor, hiçbir yerde kullanılmıyor.
            url: String::new(),
            max_connections: 5,
            acquire_timeout: std::time::Duration::from_secs(5),
        },
        redis: RedisConfig {
            // Gerçek, geliştirme ortamında ayakta olan Redis (bkz. dosya
            // başındaki yorum): rate-limit middleware'i artık her istekte
            // Redis'e gidiyor, bu yüzden erişilemez bir adres burada
            // testlerin çoğunu (yazma scope'ları fail-closed olduğu için)
            // 429'a düşürür. `build_router`, çakışmayı önlemek için bu
            // havuzu her zaman benzersiz bir anahtar önekiyle sarıyor
            // (`RateLimiter::with_prefix`) — bu yüzden burada havuz
            // paylaşılsa bile testler birbirini etkilemiyor.
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
        // Ortam değişkeni yokken `from_env` plandaki varsayılan tabloyu
        // üretiyor. Hız sınırlama davranışını doğrudan sınayan testler bile
        // (bkz. "Hız sınırlama" bölümü) bu varsayılan tabloyu kullanıyor —
        // ör. `anonymous.register` varsayılanı 3/saat, bu da tek bir testin
        // 429'u tetiklemesi için zaten yeterince düşük. Diğer testler için
        // önemli olan tek şey `build_router`'ın her testi kendi izole
        // anahtar önekiyle kurması (bkz. dosya başı yorumu) — aksi halde bu
        // varsayılanlar bile paralel testler arasında çakışırdı.
        rate_limits: LimitTable::from_env().expect("varsayılan limit tablosu geçerli olmalı"),
        storage_quota: StorageQuotaConfig::from_env()
            .expect("varsayılan depolama kotası geçerli olmalı"),
    }
}

// Bu dosyadaki yardımcı fonksiyonlar `#[sqlx::test]` ile işaretli asenkron
// test gövdelerinin *içinde değil*, onların çağırdığı sıradan fonksiyonlar —
// `clippy::expect_used`in test-gövdesi muafiyeti bunları kapsamıyor. Girdiler
// burada sabit/testin kendi ürettiği değerler olduğu için `expect` yapısal
// olarak başarısız olmaz (bkz. `crates/actos-core/tests/auth.rs`'teki
// `split_key` üzerindeki aynı gerekçe).

#[allow(clippy::expect_used)]
fn build_router(pool: PgPool) -> Router {
    build_router_with_config(test_config(), pool)
}

/// `build_router`'ın `Config`'i çağırana bıraktığı hâli — hız sınırlama
/// testleri (bkz. "Hız sınırlama" bölümü) varsayılan `rate_limits`
/// tablosundaki bir kapasiteyi (ör. `anonymous.register`) sabit sayı olarak
/// tekrar yazmak yerine `test_config()`'ten okuyup burada geçirebilsin diye
/// ayrıldı — kapasite ileride değişirse test de otomatik uyum sağlar.
#[allow(clippy::expect_used)]
fn build_router_with_config(config: Config, pool: PgPool) -> Router {
    let id_codec = IdCodec::new(&config.security.id_obfuscation_key).expect("geçerli anahtar");
    let cursor_codec = actos_core::cursor::CursorCodec::new(&config.security.cursor_signing_key);
    let redis = deadpool_redis::Config::from_url(config.redis.url.clone())
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("redis pool yapılandırması kurulabilmeli (ağ bağlantısı açmaz)");
    let storage = Storage::new(&config.storage);

    // `LimitTable` `Copy`, bu yüzden `config` taşınmadan önce okunabiliyor
    // (main.rs'teki kurulumla aynı sıra). `with_prefix` ile **her çağrıda**
    // benzersiz bir anahtar öneki verilir (bkz. dosya başındaki "Test
    // izolasyonu" yorumu) — bu olmadan paralel testler aynı Redis'te aynı
    // kovaları paylaşıp birbirinin token'ını tüketebilir.
    let test_prefix = format!("test:{}:", uuid::Uuid::new_v4());
    let rate_limiter =
        RateLimiter::with_prefix(redis.clone(), config.rate_limits, test_prefix.clone());
    // `idempotency` de aynı önekle izole edilir — bkz.
    // `crates/actos-api/tests/posts_api.rs`'teki aynı desen.
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

/// `crates/actos-core/src/secret.rs`'teki üretimle aynı biçim: bir key her
/// zaman `actos_` ile başlar.
const API_KEY_PREFIX: &str = "actos_";

/// `XXXX-XXXX-XXXX` biçimindeki (Crockford base32) bir kurtarma kodu
/// deseninin `haystack` içinde geçip geçmediğini denetler. `regex` crate'ine
/// bağımlılık eklemeden, sabit uzunluklu bir pencere kaydırarak bakıyoruz.
///
/// **Sınır denetimi neden var:** pencerenin hemen öncesi/sonrası da kod
/// karakteriyse (harf, rakam veya tire) eşleşme daha uzun bir jetonun
/// ortasına denk gelmiş demektir, gerçek bir kurtarma kodu değil. Bu olmadan
/// yanıtlardaki `request_id` UUID'si testi rastgele düşürüyordu: UUID'nin
/// tireleri 8/13/18/23. indekslerde, yani aradaki mesafe tam da bu desenin
/// beklediği 5 — `...-7340-8871-0161ea...` gibi iki grubun tamamı rakam
/// olduğunda (Crockford alfabesi rakamları da içerdiği için) pencere
/// eşleşiyordu. UUID her istekte değiştiğinden hata koşudan koşuya
/// görünüp kayboluyordu.
fn contains_recovery_code_pattern(haystack: &str) -> bool {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let is_code_char = |b: u8| b.is_ascii_alphanumeric() || b == b'-';

    let bytes = haystack.as_bytes();
    if bytes.len() < 14 {
        return false;
    }
    bytes.windows(14).enumerate().any(|(start, w)| {
        let onceki_bitisik = start > 0 && is_code_char(bytes[start - 1]);
        let sonraki_bitisik = bytes.get(start + 14).is_some_and(|&b| is_code_char(b));

        !onceki_bitisik
            && !sonraki_bitisik
            && w[4] == b'-'
            && w[9] == b'-'
            && [0, 1, 2, 3, 5, 6, 7, 8, 10, 11, 12, 13]
                .iter()
                .all(|&i| ALPHABET.contains(&w[i]))
    })
}

/// [`contains_recovery_code_pattern`]'in kendisi test ediliyor: sızıntı
/// denetçisini yanlış pozitife karşı gevşetirken gerçek bir kodu kaçırır
/// hâle getirmediğimizden emin olmak için. Denetçi körleşirse bu dosyadaki
/// bütün sızıntı iddiaları sessizce anlamsızlaşır.
#[test]
fn sizinti_denetcisi_gercek_kodu_yakalar_uuid_ile_yanilmaz() {
    // Gerçek bir kurtarma kodu, JSON'da göründüğü gibi tırnak içinde.
    assert!(contains_recovery_code_pattern(
        r#"{"code":"H8K2-9WQ4-MN3P"}"#
    ));
    // Tek başına, sınırsız.
    assert!(contains_recovery_code_pattern("H8K2-9WQ4-MN3P"));
    // Bir dizinin ortasında.
    assert!(contains_recovery_code_pattern(
        r#"["ABCD-1234-EFGH","JKMN-5678-PQRS"]"#
    ));

    // Yanlış pozitifin asıl kaynağı: request_id UUID'si. Üçüncü ve dördüncü
    // grubu tamamen rakam olan bir UUID, sınır denetimi olmadan eşleşiyordu.
    assert!(!contains_recovery_code_pattern(
        r#"{"request_id":"01a05de1-880e-7340-8871-0161ea68fb51"}"#
    ));
    // Aynı UUID çıplak hâliyle de eşleşmemeli.
    assert!(!contains_recovery_code_pattern(
        "01a05de1-880e-7340-8871-0161ea68fb51"
    ));
    // Küçük harf Crockford alfabesinde değil, zaten eşleşmemeli.
    assert!(!contains_recovery_code_pattern("h8k2-9wq4-mn3p"));
}

fn assert_no_secret_leak(context: &str, body: &Value, headers: &axum::http::HeaderMap) {
    let dump = format!("{body} {headers:?}");
    assert!(
        !dump.contains(API_KEY_PREFIX),
        "{context}: yanıt bir API key önekini sızdırıyor: {dump}"
    );
    assert!(
        !contains_recovery_code_pattern(&dump),
        "{context}: yanıt bir kurtarma kodu desenini sızdırıyor: {dump}"
    );
}

// --- Kayıt -----------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kayit_basarili_api_key_ve_on_kurtarma_kodu_doner(pool: PgPool) {
    let router = build_router(pool);
    let body = register(&router, "test_user_1").await;

    let actor_id = body["actor"]["id"]
        .as_str()
        .expect("actor.id string olmalı");
    assert!(
        actor_id.starts_with("a_"),
        "actor id base62 kodlanmış olmalı: {actor_id}"
    );
    assert_eq!(body["actor"]["username"], "test_user_1");
    assert_eq!(body["actor"]["actor_type"], "human");

    let api_key = body["api_key"].as_str().expect("api_key string olmalı");
    assert!(api_key.starts_with(API_KEY_PREFIX));

    let codes = body["recovery_codes"]
        .as_array()
        .expect("recovery_codes dizi olmalı");
    assert_eq!(codes.len(), 10);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ayni_kullanici_adi_ikinci_kez_409_conflict_doner(pool: PgPool) {
    let router = build_router(pool);
    register(&router, "duplicate_user").await;

    let (status, body, headers) = send(
        &router,
        json_req(
            "POST",
            "/auth/register",
            json!({ "username": "duplicate_user", "actor_type": "human", "display_name": null }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "CONFLICT");
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gecersiz_kullanici_adi_400_validation_failed_doner(pool: PgPool) {
    let router = build_router(pool);

    let (status, body, _) = send(
        &router,
        json_req(
            "POST",
            "/auth/register",
            json!({ "username": "AB", "actor_type": "human", "display_name": null }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_FAILED");
}

// --- whoami / auth extractor -----------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn authsuz_whoami_401_missing_credentials_doner(pool: PgPool) {
    let router = build_router(pool);

    let (status, body, _) = send(&router, empty_req("GET", "/auth/whoami")).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "MISSING_CREDENTIALS");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn uydurma_key_ile_whoami_401_invalid_key_doner(pool: PgPool) {
    let router = build_router(pool);

    let (status, body, _) = send(
        &router,
        auth_req("GET", "/auth/whoami", "actos_uydurma_bir_key_1234"),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "INVALID_KEY");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kucuk_harf_bearer_yazimi_da_kabul_ediliyor(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "lowercase_bearer").await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();

    let req = Request::builder()
        .method("GET")
        .uri("/auth/whoami")
        .header(header::AUTHORIZATION, format!("bearer {api_key}"))
        .body(Body::empty())
        .expect("istek kurulabilmeli");

    let (status, body, _) = send(&router, req).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "'bearer' (küçük harf) kabul edilmeli: {body}"
    );
    assert_eq!(body["actor"]["username"], "lowercase_bearer");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kayittan_donen_key_ile_whoami_dogru_kullaniciyi_doner(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "whoami_user").await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();

    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &api_key)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["actor"]["username"], "whoami_user");
    assert_eq!(body["actor"]["id"], reg["actor"]["id"]);
    assert!(
        body["roles"]
            .as_array()
            .expect("roles dizi olmalı")
            .is_empty()
    );
    assert!(body["key"]["id"].as_str().is_some());
    // Doğrulamada kullanılan key'in özeti dönüyor olmalı, secret değil.
    assert!(body.get("secret_hash").is_none());
    // Avatar hiç set edilmemiş — `avatar_url` `null` olmalı (bkz.
    // `whoami_avatar_url_set_edilmisse_dogru_url_doner` set edilmiş hâli
    // için).
    assert!(body["actor"]["avatar_url"].is_null());
}

/// Faz 18.A: `whoami`'nin `AuthenticatedActor::avatar_object_key`'i doğru
/// okuyup `Storage::public_url` ile URL'e çevirdiğini sınar.
///
/// `actors.avatar_object_key` burada **doğrudan `UPDATE` ile** yazılıyor,
/// `PATCH /actors/me` üzerinden değil: bu testin konusu `whoami`'nin okuma
/// tarafı (`actos_core::auth::authenticate`'in yeni `avatar_object_key`
/// alanı) — yazma tarafının kendi doğrulama testleri (`crates/actos-api/
/// tests/actors_api.rs`'teki `patch_me_avatar_*`) zaten ayrı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn whoami_avatar_url_set_edilmisse_dogru_url_doner(pool: PgPool) {
    let router = build_router(pool.clone());
    let reg = register(&router, "whoami_avatarli").await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();
    let actor_id =
        sqlx::query_scalar!(r#"SELECT id FROM actors WHERE username = 'whoami_avatarli'"#)
            .fetch_one(&pool)
            .await
            .expect("actor bulunabilmeli");

    sqlx::query!(
        r#"UPDATE actors SET avatar_object_key = 'abc/some-key.webp' WHERE id = $1"#,
        actor_id,
    )
    .execute(&pool)
    .await
    .expect("avatar_object_key yazılabilmeli");

    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &api_key)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["actor"]["avatar_url"],
        "http://127.0.0.1:1/test-bucket/abc/some-key.webp"
    );
}

// --- Key yönetimi ------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn key_uret_listede_iki_key_var_secret_yok(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "key_manager").await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();

    let (status, body, _) = send(&router, {
        let mut req = json_req("POST", "/auth/keys", json!({ "label": "ikinci key" }));
        req.headers_mut().insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {api_key}")).expect("geçerli header"),
        );
        req
    })
    .await;
    assert_eq!(status, StatusCode::CREATED, "key üretimi başarısız: {body}");
    let second_api_key = body["api_key"].as_str().expect("api_key olmalı").to_owned();
    assert_ne!(second_api_key, api_key);

    let (status, list_body, _) = send(&router, auth_req("GET", "/auth/keys", &api_key)).await;
    assert_eq!(status, StatusCode::OK);
    let keys = list_body["keys"].as_array().expect("keys dizi olmalı");
    assert_eq!(keys.len(), 2, "iki key olmalı: {list_body}");
    for key in keys {
        assert!(key.get("secret").is_none());
        assert!(key.get("api_key").is_none());
        let dump = key.to_string();
        assert!(
            !dump.contains(API_KEY_PREFIX),
            "key listesi secret sızdırıyor: {dump}"
        );
    }
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn key_iptal_edilince_o_key_calismiyor_digeri_calisiyor(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "revoke_user").await;
    let first_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();

    let (status, second_reg, _) = send(&router, {
        let mut req = json_req("POST", "/auth/keys", json!({ "label": null }));
        req.headers_mut().insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {first_key}")).expect("geçerli header"),
        );
        req
    })
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let second_key = second_reg["api_key"]
        .as_str()
        .expect("api_key olmalı")
        .to_owned();
    let second_key_id = second_reg["key"]["id"]
        .as_str()
        .expect("key.id olmalı")
        .to_owned();

    let (status, _, _) = send(
        &router,
        auth_req("DELETE", &format!("/auth/keys/{second_key_id}"), &first_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // İptal edilen key artık çalışmıyor.
    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &second_key)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "iptal edilen key hâlâ çalışıyor: {body}"
    );
    assert_eq!(body["code"], "INVALID_KEY");

    // Diğer key hâlâ çalışıyor.
    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &first_key)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "iptal edilmeyen key çalışmalı: {body}"
    );
    assert_eq!(body["actor"]["username"], "revoke_user");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn baskasinin_keyini_iptal_404_doner_key_calismaya_devam_eder(pool: PgPool) {
    let router = build_router(pool);
    let victim = register(&router, "victim_user").await;
    let victim_key = victim["api_key"]
        .as_str()
        .expect("api_key olmalı")
        .to_owned();

    // Kayıt yanıtı key'in uuid'sini içermiyor (yalnızca actor id ve ham
    // key), whoami ile alıyoruz.
    let (_, whoami_body, _) = send(&router, auth_req("GET", "/auth/whoami", &victim_key)).await;
    let victim_key_id = whoami_body["key"]["id"]
        .as_str()
        .expect("key.id olmalı")
        .to_owned();

    let attacker = register(&router, "attacker_user").await;
    let attacker_key = attacker["api_key"]
        .as_str()
        .expect("api_key olmalı")
        .to_owned();

    let (status, body, _) = send(
        &router,
        auth_req(
            "DELETE",
            &format!("/auth/keys/{victim_key_id}"),
            &attacker_key,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "başkasının key'i iptal edilebildi: {body}"
    );

    // Kurban'ın key'i hâlâ çalışıyor.
    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &victim_key)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "kurban'ın key'i etkilenmemeli: {body}"
    );
}

// --- Kurtarma ----------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gecerli_kurtarma_kodu_yeni_key_uretir_tekrar_kullanilamaz(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "recover_user").await;
    let code = reg["recovery_codes"][0]
        .as_str()
        .expect("kurtarma kodu olmalı")
        .to_owned();

    let (status, body, _) = send(
        &router,
        json_req(
            "POST",
            "/auth/recover",
            json!({ "username": "recover_user", "recovery_code": code }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kurtarma başarısız: {body}");
    let new_key = body["api_key"].as_str().expect("api_key olmalı").to_owned();
    assert_eq!(body["remaining_recovery_codes"], 9);

    // Yeni key çalışıyor.
    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &new_key)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "kurtarma ile üretilen key çalışmalı: {body}"
    );
    assert_eq!(body["actor"]["username"], "recover_user");

    // Aynı kod tekrar kullanılamaz.
    let (status, body, _) = send(
        &router,
        json_req(
            "POST",
            "/auth/recover",
            json!({ "username": "recover_user", "recovery_code": code }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "kullanılmış kod tekrar kabul edildi: {body}"
    );
    assert_eq!(body["code"], "INVALID_KEY");
}

// --- Ban -----------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn banli_actor_yazamaz_ama_okuyabilir(pool: PgPool) {
    // Ham SQL için ayrı bir tutamaç: `pool` altta `build_router`'a taşınacak
    // (`PgPool` klonlanabilir, iç havuz paylaşımlı — aynı veritabanına gider).
    let raw_pool = pool.clone();
    let router = build_router(pool);
    let admin = register(&router, "ban_admin").await;
    let victim = register(&router, "ban_victim").await;
    let victim_key = victim["api_key"]
        .as_str()
        .expect("api_key olmalı")
        .to_owned();

    // Actor id'leri (bigint) doğrudan DB'den okunuyor: yanıttaki id'ler
    // base62 kodlanmış, ban satırı ise ham `bigint` FK bekliyor.
    let admin_id: i64 = sqlx::query_scalar!(
        r#"SELECT id FROM actors WHERE username = $1"#,
        admin["actor"]["username"].as_str().expect("username")
    )
    .fetch_one(&raw_pool)
    .await
    .expect("admin actor id bulunmalı");
    let victim_id: i64 = sqlx::query_scalar!(
        r#"SELECT id FROM actors WHERE username = $1"#,
        victim["actor"]["username"].as_str().expect("username")
    )
    .fetch_one(&raw_pool)
    .await
    .expect("victim actor id bulunmalı");

    sqlx::query!(
        r#"
        INSERT INTO bans (actor_id, banned_by, reason, expires_at)
        VALUES ($1, $2, 'test banı', NULL)
        "#,
        victim_id,
        admin_id,
    )
    .execute(&raw_pool)
    .await
    .expect("ban eklenebilmeli");

    // **Okuma serbest** (Faz 14'te değişen davranış): ban yazmaya
    // yöneliktir, banlı bir actor kendi profilini bile göremeseydi ceza
    // amacını aşardı. Kimlik doğrulama artık ban'de düşmüyor, yalnızca
    // `AuthenticatedActor::banned` bayrağını işaretliyor.
    let (status, body, _) = send(&router, auth_req("GET", "/auth/whoami", &victim_key)).await;
    assert_eq!(status, StatusCode::OK, "banlı actor okuyabilmeli: {body}");
    assert_eq!(body["actor"]["username"], "ban_victim", "{body}");

    // **Yazma kapalı.** Kural `CurrentActor` extractor'ında, güvenli
    // olmayan HTTP metotlarında uygulanıyor (bkz. `actos-api/src/auth.rs`).
    let (status, body, _) = send(
        &router,
        Request::builder()
            .method("POST")
            .uri("/auth/keys")
            .header(header::AUTHORIZATION, format!("Bearer {victim_key}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "label": "yeni key" }).to_string()))
            .expect("istek kurulabilmeli"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "banlı actor yazamamalı: {body}"
    );
    assert_eq!(body["code"], "BANNED", "{body}");
}

// --- Sır sızıntısı ----------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn hicbir_hata_yaniti_sir_sizdirmiyor(pool: PgPool) {
    let router = build_router(pool);
    let reg = register(&router, "leak_check_user").await;
    let api_key = reg["api_key"].as_str().expect("api_key olmalı").to_owned();

    let cases: Vec<(&str, StatusCode, Request<Body>)> = vec![
        (
            "409 conflict",
            StatusCode::CONFLICT,
            json_req(
                "POST",
                "/auth/register",
                json!({ "username": "leak_check_user", "actor_type": "human", "display_name": null }),
            ),
        ),
        (
            "400 validation",
            StatusCode::BAD_REQUEST,
            json_req(
                "POST",
                "/auth/register",
                json!({ "username": "x", "actor_type": "human", "display_name": null }),
            ),
        ),
        (
            "401 missing credentials",
            StatusCode::UNAUTHORIZED,
            empty_req("GET", "/auth/whoami"),
        ),
        (
            "401 invalid key",
            StatusCode::UNAUTHORIZED,
            auth_req("GET", "/auth/whoami", "actos_bogus_key_value"),
        ),
        (
            "401 invalid recovery",
            StatusCode::UNAUTHORIZED,
            json_req(
                "POST",
                "/auth/recover",
                json!({ "username": "leak_check_user", "recovery_code": "0000-0000-0000" }),
            ),
        ),
        (
            "404 not found key",
            StatusCode::NOT_FOUND,
            // Geçerli bir key ile ama ayrıştırılamayan bir `key_id` — handler'a
            // ulaşabilmesi için auth başarılı olmalı, aksi halde 401'e takılır.
            auth_req("DELETE", "/auth/keys/not-a-uuid", &api_key),
        ),
    ];

    for (label, expected_status, req) in cases {
        let (status, body, headers) = send(&router, req).await;
        assert_eq!(
            status, expected_status,
            "{label}: beklenmeyen durum kodu: {body}"
        );
        assert_no_secret_leak(label, &body, &headers);
    }
}

// --- Hız sınırlama -----------------------------------------------------
//
// Faz 6 ile `crate::middleware::ratelimit::enforce` router'a bağlandı; bu
// iki test o bağlantının **gerçekten** işlediğini kanıtlıyor (token
// bucket'ın kendisi zaten `actos-core/tests/ratelimit.rs`'te sınanıyor —
// burada sınanan, HTTP katmanının doğru scope'u seçip doğru header'ları/
// durum kodunu üretmesi). `POST /auth/register` kullanılıyor çünkü
// kimliksiz (IP başına) ve düşük varsayılan kapasiteli (3/saat) tek uç —
// başka bir uçta bunu tetiklemek için ya kimlik kurmak ya da onlarca istek
// atmak gerekirdi.

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kayit_limiti_asilinca_429_rate_limited_ve_retry_after_donuyor(pool: PgPool) {
    let router = build_router(pool);

    // Varsayılan `anonymous.register` kapasitesi kadar istek at — hepsi
    // izinli olmalı. Kullanıcı adları bilerek farklı: amaç kayıt
    // mantığını değil, `/auth/register`'a giden isteklerin **kendisinin**
    // sayılmasını sınamak — middleware token'ı `next.run` çağrılmadan
    // *önce* tüketir, isteğin sonucundan (409/400/201) bağımsız.
    let capacity = test_config().rate_limits.anonymous.register.capacity;
    for i in 0..capacity {
        let (status, body, _) = send(
            &router,
            json_req(
                "POST",
                "/auth/register",
                json!({ "username": format!("rl_reg_user_{i}"), "actor_type": "human", "display_name": null }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "istek {i}/{capacity}: {body}");
    }

    // Kapasitenin bir fazlası: kovada token kalmadı, reddedilmeli.
    let (status, body, headers) = send(
        &router,
        json_req(
            "POST",
            "/auth/register",
            json!({ "username": "rl_reg_user_over_limit", "actor_type": "human", "display_name": null }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "RATE_LIMITED");

    let retry_after: u64 = headers
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .expect("Retry-After header'ı sayısal olmalı");
    assert!(
        retry_after > 0,
        "retry_after 0'dan büyük olmalı: {retry_after}"
    );

    // `apply_headers` her yanıta eklenir — sadece izinli olanlara değil,
    // reddedilen istekte de `X-RateLimit-Remaining: 0` görülmeli.
    assert_eq!(
        headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok()),
        Some("0")
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn basarili_yanitta_x_ratelimit_headerlari_dogru(pool: PgPool) {
    let config = test_config();
    let capacity = config.rate_limits.anonymous.register.capacity;
    let window_secs = config.rate_limits.anonymous.register.window.as_secs();
    let router = build_router_with_config(config, pool);

    let (status, body, headers) = send(
        &router,
        json_req(
            "POST",
            "/auth/register",
            json!({ "username": "rl_header_user", "actor_type": "human", "display_name": null }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    assert_eq!(
        headers
            .get("x-ratelimit-limit")
            .and_then(|v| v.to_str().ok()),
        Some(capacity.to_string().as_str()),
        "X-RateLimit-Limit, kademenin kapasitesini yansıtmalı"
    );
    assert_eq!(
        headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok()),
        Some((capacity - 1).to_string().as_str()),
        "bu istekle kapasiteden tam 1 token tüketilmiş olmalı"
    );

    let reset: u64 = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .expect("X-RateLimit-Reset sayısal olmalı");
    assert!(
        reset > 0 && reset <= window_secs,
        "kovanın tamamen dolmasına kalan süre pencere içinde olmalı: {reset}s (pencere {window_secs}s)"
    );
}
