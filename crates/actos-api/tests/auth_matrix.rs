//! Yetkilendirme matrisi: her operasyon × 6 rol (anon / normal / sahip /
//! moderatör / admin / banlı).
//!
//! Faz 18.B'nin görevi: "her operasyon × her rol" tablosu **elle** yazılır
//! (aşağıdaki `MATRIX_OPERATIONS` + her test fonksiyonundaki rol bazlı
//! iddialar), ama bu tablonun **kapsamı** elle değil canlı `OpenApi`
//! değerinden türetiliyor —
//! [`matris_spec_ile_ayni_kapsami_kapsiyor`] `GET /openapi.json`'dan bütün
//! `(method, path)` operasyonlarını okuyup [`MATRIX_OPERATIONS`] ∪
//! [`EXEMPT_OPERATIONS`] ile birebir eşleştiğini doğruluyor. Biri yeni bir
//! uç ekleyip bu dosyaya satır eklemeyi unutursa (ya da `EXEMPT_OPERATIONS`'a
//! gerekçesiz eklerse) bu test kırılır ve eksik/fazla operasyonları listeler.
//!
//! **İddialar HTTP durum kodu + gövdedeki makine-okunur `code` alanı
//! üzerinden** (bkz. `crate::error::ApiError` / RFC 9457
//! `application/problem+json`) — `title`/`detail` gibi insan-okunur
//! metinlere hiçbir yerde assert edilmiyor (bu görevle paralel çalışan
//! başka bir ajan tam o metinleri İngilizceye çeviriyor).
//!
//! Kurulum yardımcıları `tests/admin_api.rs` ile aynı desen — ayrı bir
//! entegrasyon test binary'si olduğu için (Rust her `tests/*.rs` dosyasını
//! bağımsız derler) paylaşılan bir modül olmadan tekrar tanımlanıyor.
//! Avatar yükleme (`POST /actors/me/avatar`) testleri gerçek MinIO'ya
//! bağlanıyor (`127.0.0.1:3103`, bkz. `test_config` — `.env`'teki `S3_*`
//! değerleriyle birebir aynı), diğer bütün testler `admin_api.rs`'teki gibi
//! erişilemez (`127.0.0.1:1`) bir depolama uç noktası kullanabilirdi ama tek
//! bir `test_config` tutmak (ikisi arasında geçiş yapmamak) daha basit —
//! depolamaya hiç dokunmayan testler için bu ayarın bir maliyeti yok
//! (`Storage::new` ağa hiç dokunmuyor, yalnızca bir istemci kurar).
//!
//! ## Roller
//!
//! - **anon** — `Authorization` yok.
//! - **normal** — kimlikli, kaynağın sahibi değil, banlı değil, hiçbir rolü
//!   yok.
//! - **sahip (owner)** — kaynağı yaratan/kaynağın öznesi olan actor.
//! - **moderatör** — eski moderatör izin kümesi (content.delete, member.ban,
//!   report.view, report.resolve, audit.view).
//! - **admin** — eski admin izin kümesi (moderatörün beşi + community.edit,
//!   community.close, role.grant).
//! - **banlı** — kimlikli, banlı, izinsiz.
//!
//! ## Koddan doğrulanan, tahmin edilmeyen incelikler
//!
//! Aşağıdakiler ilk bakışta "hepsi aynı olur" sanılabilecek ama kodda
//! **farklı** davrandığı görülen hücreler — her biri ilgili test
//! fonksiyonunda ayrıca not düşülüyor:
//!
//! 1. **Ban kontrolü yalnızca güvenli olmayan metotlarda çalışır**
//!    (`crate::auth::CurrentActor::from_request_parts`): banlı bir actor'ün
//!    `GET /admin/reports` / `GET /admin/actions` gibi yalnızca
//!    moderatör/admin'e açık bir `GET` ucuna gitmesi `403 BANNED` değil
//!    `403 FORBIDDEN` döner — `GET` güvenli olduğu için ban kontrolü hiç
//!    devreye girmiyor, sonrasında rol kontrolü (moderatör değil) devreye
//!    giriyor.
//! 2. **`PATCH /posts/{id}` ve `PATCH /comments/{id}`'de moderatör/admin
//!    override YOK** — `actos_core::content::update_post`/
//!    `actos_core::comment::update_comment` yalnızca sahiplik kontrol
//!    ediyor, `roles` parametresi bile almıyor (`delete_post`/
//!    `delete_comment`'in aksine). Yani bir moderatör başkasının postunu
//!    **silebilir** ama **düzenleyemez** — düzenleme her zaman `403`.
//! 3. **`DELETE /auth/keys/{key_id}`'de başkasının key'i `403` değil `404`**
//!    (`actos_core::auth::revoke_key`) — "biçim geçerli ama bu key sana ait
//!    değil" ile "böyle bir key hiç yok" ayrımı saldırgana bilgi verirdi.
//! 4. **`PUT /contents/{id}/vote`'ta sahip kendi içeriğine oy veremez**
//!    (`403 FORBIDDEN`, `actos_core::interaction::set_vote`) — matristeki
//!    tek satır burada "sahip" için `200` değil `403` bekliyor.
//! 5. **`PUT /actors/{username}/follow`'ta sahip = kendini takip**, bu da
//!    `204` değil `400 VALIDATION_FAILED` (`actos_core::interaction::follow`).
//! 6. **`POST /auth/recover` kimlik doğrulama gerektirmiyor ama yine de
//!    banı kontrol ediyor** (`actos_core::auth::recover` içinde,
//!    extractor'dan bağımsız, elle bir `if found.is_banned` kontrolü) —
//!    banlı bir hesap kendi kurtarma koduyla bile yeni key alamıyor.

use std::collections::BTreeSet;

use actos_api::{app, state::AppState};
use actos_core::{
    Config, Storage,
    auth::{self as core_auth, ActorType, Permission, PermissionScope},
    config::{
        DatabaseConfig, LimitTable, RedisConfig, SecurityConfig, ServerConfig, StorageConfig,
        StorageQuotaConfig,
    },
    cursor::CursorCodec,
    id::IdCodec,
    idempotency::IdempotencyStore,
    moderation as core_mod,
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
        // Gerçek MinIO — bkz. dosya başındaki modül dokümanı. `.env`'teki
        // `S3_*` değerleriyle birebir aynı.
        storage: StorageConfig {
            endpoint: "http://127.0.0.1:3103".to_owned(),
            region: "us-east-1".to_owned(),
            bucket: "actos-media".to_owned(),
            access_key: "actos_minio".to_owned(),
            secret_key: "actos_minio_dev_password".to_owned(),
            public_base_url: "http://127.0.0.1:3103/actos-media".to_owned(),
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
    // Bkz. `tests/admin_api.rs`'teki aynı önek gerekçesi: gerçek Redis'i
    // paylaşan paralel testler arasında izolasyon.
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
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
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

/// `token: None` → header hiç yok (anon); `Some(t)` → auth'lu JSON isteği.
#[allow(clippy::expect_used)]
fn maybe_auth_json_req(method: &str, uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    match token {
        Some(t) => auth_json_req(method, uri, t, body),
        None => Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("istek kurulabilmeli"),
    }
}

/// Beklenen durum kodunu doğrular, gövdeyi döner (başarı gövdelerine daha
/// fazla bakmak isteyen çağıranlar için).
#[allow(clippy::expect_used)]
async fn assert_status(
    router: &Router,
    req: Request<Body>,
    expected: StatusCode,
    label: &str,
) -> Value {
    let (status, body, _) = send(router, req).await;
    assert_eq!(
        status, expected,
        "{label}: beklenmeyen durum — gövde: {body}"
    );
    body
}

/// Beklenen durum kodu **ve** makine-okunur `code` alanını doğrular.
/// `title`/`detail`'e hiç bakılmıyor (bkz. dosya başındaki modül dokümanı).
async fn assert_code(
    router: &Router,
    req: Request<Body>,
    expected_status: StatusCode,
    expected_code: &str,
    label: &str,
) {
    let (status, body, _) = send(router, req).await;
    assert_eq!(
        status, expected_status,
        "{label}: beklenmeyen durum — gövde: {body}"
    );
    assert_eq!(
        body["code"], expected_code,
        "{label}: beklenmeyen kod — gövde: {body}"
    );
}

/// `actos_core::auth::register`'ı doğrudan çağırır (hız sınırını görmeden).
/// Döner: `(actor_id, api_key)`.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> (i64, String) {
    let reg = core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    (reg.actor.id, reg.api_key)
}

/// [`seed_actor`] + kurtarma kodları — `POST /auth/recover` /
/// `DELETE /actors/me` testleri için.
#[allow(clippy::expect_used)]
async fn seed_actor_with_recovery(pool: &PgPool, username: &str) -> (i64, String, Vec<String>) {
    let reg = core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli");
    (reg.actor.id, reg.api_key, reg.recovery_codes)
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
        core_auth::grant_permission(
            pool,
            actor_id,
            *permission,
            PermissionScope::Global,
            None,
            None,
        )
        .await
        .expect("izin verilebilmeli");
    }
}

/// `actor_id`'yi kalıcı olarak banlar. `banlayan_id` yalnızca denetim izi
/// için — herhangi bir actor id'si olabilir (bu testte gerçekten
/// moderatör olması şart değil, `ban_actor`'ın kendisi çağıranın rolünü
/// kontrol etmiyor; HTTP katmanındaki `Require<CanBan>` izin kontrolü zaten
/// ayrı testlerle kapsanıyor — bkz. `moderasyon_uclarinin_yetki_matrisi`).
#[allow(clippy::expect_used)]
async fn banla(pool: &PgPool, banlayan_id: i64, username: &str) {
    core_mod::ban_actor(pool, banlayan_id, username, "yetki matrisi testi", None)
        .await
        .expect("actor banlanabilmeli");
}

#[allow(clippy::expect_used)]
async fn seed_post(router: &Router, api_key: &str, title: &str) -> String {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            "/posts",
            api_key,
            json!({ "title": title, "body": "matris test gövdesi", "tags": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post oluşturulamadı: {body}");
    body["id"].as_str().expect("post id").to_owned()
}

#[allow(clippy::expect_used)]
async fn seed_post_with_tag(router: &Router, api_key: &str, title: &str, tag: &str) -> String {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            "/posts",
            api_key,
            json!({ "title": title, "body": "matris test gövdesi", "tags": [tag] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post oluşturulamadı: {body}");
    body["id"].as_str().expect("post id").to_owned()
}

#[allow(clippy::expect_used)]
async fn seed_comment(router: &Router, api_key: &str, post_id: &str, body_text: &str) -> String {
    let (status, body, _) = send(
        router,
        auth_json_req(
            "POST",
            &format!("/posts/{post_id}/comments"),
            api_key,
            json!({ "body": body_text }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "yorum oluşturulamadı: {body}");
    body["id"].as_str().expect("comment id").to_owned()
}

// --- Bağımlığı olmayan PNG üretimi (bkz. `crates/actos-core/tests/media.rs`
// `gercek_png`/`crc32`'nin aynı deseni — burada `image` crate'i
// `actos-api`'nin bağımlılığı olmadığı için sıfırdan, minimal bir PNG elle
// kuruluyor: "stored" (sıkıştırmasız) bir deflate bloğu + zlib sarmalayıcı,
// tamamen RFC 1950/1951'e uygun, harici hiçbir crate gerektirmiyor). ------

#[allow(clippy::cast_possible_truncation)]
fn crc32(veri: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in veri {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    const MODULO: u32 = 65521;
    for &byte in data {
        a = (a + u32::from(byte)) % MODULO;
        b = (b + a) % MODULO;
    }
    (b << 16) | a
}

fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    #[allow(clippy::cast_possible_truncation)]
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Sıkıştırmasız ("stored") tek bir deflate bloğu + zlib sarmalayıcı.
/// `raw.len()` burada hep birkaç bayt (1×1 piksel) olduğu için tek blok
/// yeterli (deflate stored blokları azami 65535 bayt taşıyabilir).
#[allow(clippy::cast_possible_truncation)]
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let len = raw.len() as u16;
    out.push(0x01); // BFINAL=1, BTYPE=00 (stored)
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(raw);
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// 1×1 pikselli, geçerli bir RGB8 PNG üretir — `actos_core::media::
/// process_image`'ın kabul ettiği doğrulandı (bu dosyanın yazımı sırasında
/// `crates/actos-core`'da geçici bir testle elle doğrulandı, sonra
/// silindi).
fn tiny_png() -> Vec<u8> {
    let raw = vec![0u8, 255, 0, 0]; // filtre baytı (None) + 1 piksel RGB (kırmızı)
    let zlib = zlib_stored(&raw);
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // derinlik 8, renk tipi 2 (RGB)
    push_png_chunk(&mut png, b"IHDR", &ihdr);
    push_png_chunk(&mut png, b"IDAT", &zlib);
    push_png_chunk(&mut png, b"IEND", &[]);
    png
}

#[allow(clippy::expect_used)]
fn multipart_upload_req(
    method: &str,
    uri: &str,
    token: Option<&str>,
    bytes: &[u8],
) -> Request<Body> {
    const BOUNDARY: &str = "----actosAuthMatrixBoundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"tiny.png\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    let mut builder = Request::builder().method(method).uri(uri).header(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={BOUNDARY}"),
    );
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    builder.body(Body::from(body)).expect("istek kurulabilmeli")
}

// ============================================================================
// Kapsam: spec'ten türetilen operasyon listesiyle bu dosyanın tablosu
// birebir eşleşmeli.
// ============================================================================

/// Bu dosyanın rol matrisiyle kapsadığı her `(METHOD, path)` operasyonu.
/// Gruplar aşağıdaki test fonksiyonlarına karşılık geliyor — bkz. her
/// grubun üstündeki yorum.
const MATRIX_OPERATIONS: &[(&str, &str)] = &[
    // --- auth_uclarinin_yetki_matrisi ---
    ("POST", "/auth/register"),
    ("GET", "/auth/whoami"),
    ("POST", "/auth/keys"),
    ("GET", "/auth/keys"),
    ("DELETE", "/auth/keys/{key_id}"),
    ("POST", "/auth/recover"),
    ("POST", "/auth/recovery-codes/regenerate"),
    // --- actor_profil_uclarinin_yetki_matrisi ---
    ("GET", "/actors"),
    ("GET", "/actors/{username}"),
    ("PATCH", "/actors/me"),
    ("DELETE", "/actors/me"),
    ("POST", "/actors/me/avatar"),
    ("DELETE", "/actors/me/avatar"),
    ("GET", "/actors/{username}/followers"),
    ("GET", "/actors/{username}/following"),
    // --- post_uclarinin_yetki_matrisi ---
    ("POST", "/posts"),
    ("GET", "/posts/{id}"),
    ("PATCH", "/posts/{id}"),
    ("DELETE", "/posts/{id}"),
    ("GET", "/actors/{username}/posts"),
    // --- comment_uclarinin_yetki_matrisi ---
    ("POST", "/posts/{id}/comments"),
    ("GET", "/posts/{id}/comments"),
    ("GET", "/comments/{id}"),
    ("PATCH", "/comments/{id}"),
    ("DELETE", "/comments/{id}"),
    ("GET", "/actors/{username}/comments"),
    // --- interaction_uclarinin_yetki_matrisi ---
    ("PUT", "/contents/{id}/vote"),
    ("GET", "/me/votes"),
    ("PUT", "/contents/{id}/save"),
    ("DELETE", "/contents/{id}/save"),
    ("PUT", "/actors/{username}/follow"),
    ("DELETE", "/actors/{username}/follow"),
    ("GET", "/me/saves"),
    // --- notification_uclarinin_yetki_matrisi ---
    ("GET", "/me/inbox"),
    ("PATCH", "/me/inbox/{id}/read"),
    ("POST", "/me/inbox/read"),
    // --- feed_arama_etiket_uclarinin_yetki_matrisi ---
    ("GET", "/feed"),
    ("GET", "/feed/following"),
    ("GET", "/search"),
    ("GET", "/tags"),
    ("GET", "/tags/search"),
    ("GET", "/tags/{name}/posts"),
    // --- moderasyon_uclarinin_yetki_matrisi ---
    ("POST", "/reports"),
    ("GET", "/admin/reports"),
    ("PATCH", "/admin/reports/{id}"),
    ("DELETE", "/admin/contents/{id}"),
    ("POST", "/admin/bans"),
    ("DELETE", "/admin/bans/{username}"),
    ("PUT", "/admin/permissions"),
    ("DELETE", "/admin/permissions"),
    ("GET", "/admin/actions"),
];

/// Bilinçli olarak matris dışı bırakılan operasyonlar — her biri **kendi
/// gerekçesiyle**. Rol/yetki boyutu bu uçlarda anlamsız: hiçbiri bir
/// kaynağa, sahipliğe ya da role bakmıyor, hepsi her zaman herkese açık.
const EXEMPT_OPERATIONS: &[(&str, &str, &str)] = &[
    (
        "GET",
        "/health",
        "Canlılık probu: kaynak/sahiplik kavramı yok, her zaman herkese 200 \
         döner (bkz. crate::routes::health) — bir yetki matrisinin konusu değil.",
    ),
    (
        "GET",
        "/health/ready",
        "Hazır olma probu: /health ile aynı gerekçe, yalnızca DB/Redis \
         bağlantısını yoklar, kimlikten bağımsız.",
    ),
    (
        "GET",
        "/version",
        "Statik sürüm/derleme bilgisi, tasarım gereği herkese açık — test \
         edilecek bir yetki sınırı yok.",
    ),
    (
        "GET",
        "/docs/agent",
        "Bilinçli olarak kimlikten VE hız sınırından muaf (bkz. \
         crate::routes::mod modül dokümanı, `/docs/agent` bölümü): bir \
         ajanın henüz hiçbir API key'i yokken okuyabilmesi bu ucun tüm \
         amacı — rol bazlı erişim testi bu amacın tam tersini sınardı.",
    ),
];

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn matris_spec_ile_ayni_kapsami_kapsiyor(pool: PgPool) {
    let router = build_router(pool);
    let (status, body, _) = send(&router, empty_req("GET", "/openapi.json")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let paths = body["paths"].as_object().expect("paths bir nesne olmalı");

    const HTTP_METHODS: &[&str] = &[
        "get", "post", "put", "patch", "delete", "head", "options", "trace",
    ];

    let mut spec_operations: BTreeSet<(String, String)> = BTreeSet::new();
    for (path, item) in paths {
        let item = item.as_object().expect("path öğesi bir nesne olmalı");
        for method in HTTP_METHODS {
            if item.contains_key(*method) {
                spec_operations.insert((method.to_ascii_uppercase(), path.clone()));
            }
        }
    }

    let mut declared: BTreeSet<(String, String)> = MATRIX_OPERATIONS
        .iter()
        .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
        .collect();
    // Aynı operasyon hem matriste hem EXEMPT'te olamaz — biri diğerini
    // örtmesin diye burada da toplanıyor ve aşağıda çakışma kontrolü var.
    let exempt: BTreeSet<(String, String)> = EXEMPT_OPERATIONS
        .iter()
        .map(|(m, p, _)| ((*m).to_owned(), (*p).to_owned()))
        .collect();

    for op in &exempt {
        assert!(
            !declared.contains(op),
            "{op:?} hem MATRIX_OPERATIONS hem EXEMPT_OPERATIONS'ta — birini seç"
        );
    }

    declared.extend(exempt.iter().cloned());

    let eksik: Vec<_> = spec_operations.difference(&declared).collect();
    assert!(
        eksik.is_empty(),
        "spec'te olup bu dosyanın matrisinde/EXEMPT listesinde karşılığı olmayan \
         operasyonlar var — yeni bir uç eklenip buraya satır eklenmesi unutulmuş \
         olabilir: {eksik:?}"
    );

    let fazla: Vec<_> = declared.difference(&spec_operations).collect();
    assert!(
        fazla.is_empty(),
        "bu dosyanın matrisinde/EXEMPT listesinde olup spec'te karşılığı \
         olmayan operasyonlar var (silinmiş bir uç mü, yazım hatası mı?): {fazla:?}"
    );

    // EXEMPT'in her satırı gerçekten gerekçeli olmalı — sessizce atlanmıyor.
    for (method, path, reason) in EXEMPT_OPERATIONS {
        assert!(
            !reason.trim().is_empty(),
            "{method} {path} EXEMPT listesinde ama gerekçesi boş"
        );
    }
}

// ============================================================================
// auth_uclarinin_yetki_matrisi — 7 operasyon
// ============================================================================

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
#[allow(clippy::too_many_lines)]
async fn auth_uclarinin_yetki_matrisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (_, normal_key) = seed_actor(&raw_pool, "am_auth_normal").await;
    let (owner_id, owner_key) = seed_actor(&raw_pool, "am_auth_owner").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "am_auth_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "am_auth_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;
    let (_banned_id, banned_key, banned_codes) =
        seed_actor_with_recovery(&raw_pool, "am_auth_banned").await;
    banla(&raw_pool, owner_id, "am_auth_banned").await;

    // --- POST /auth/register — çağıranın kimliği/rolü tamamen alakasız,
    // her zaman herkese açık (bkz. handler: `CurrentActor` hiç kullanmıyor).
    for (rol, token) in [
        ("anon", None),
        ("normal", Some(normal_key.as_str())),
        ("sahip", Some(owner_key.as_str())),
        ("moderator", Some(mod_key.as_str())),
        ("admin", Some(admin_key.as_str())),
        ("banli", Some(banned_key.as_str())),
    ] {
        let username = format!("am_reg_{rol}");
        assert_status(
            &router,
            maybe_auth_json_req(
                "POST",
                "/auth/register",
                token,
                json!({ "username": username, "actor_type": "human", "display_name": null }),
            ),
            StatusCode::CREATED,
            &format!("POST /auth/register [{rol}]"),
        )
        .await;
    }

    // --- GET /auth/whoami ---
    assert_code(
        &router,
        empty_req("GET", "/auth/whoami"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "GET /auth/whoami [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        // Okuma serbest: GET güvenli metot, ban kontrolüne hiç takılmıyor.
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", "/auth/whoami", token),
            StatusCode::OK,
            &format!("GET /auth/whoami [{rol}]"),
        )
        .await;
    }

    // --- POST /auth/keys — yazma, banlı için 403 BANNED ---
    assert_code(
        &router,
        maybe_auth_json_req("POST", "/auth/keys", None, json!({ "label": null })),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "POST /auth/keys [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_json_req("POST", "/auth/keys", token, json!({ "label": null })),
            StatusCode::CREATED,
            &format!("POST /auth/keys [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_json_req("POST", "/auth/keys", &banned_key, json!({ "label": null })),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /auth/keys [banli]",
    )
    .await;

    // --- GET /auth/keys — okuma, banlı dahil herkes (kimlikli) 200 ---
    assert_code(
        &router,
        empty_req("GET", "/auth/keys"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "GET /auth/keys [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", "/auth/keys", token),
            StatusCode::OK,
            &format!("GET /auth/keys [{rol}]"),
        )
        .await;
    }

    // --- DELETE /auth/keys/{key_id} ---
    // "sahip" (`owner_key`) için kendi key'ini oluşturup siliyoruz.
    let owner_new_key = assert_status(
        &router,
        auth_json_req(
            "POST",
            "/auth/keys",
            &owner_key,
            json!({ "label": "silinecek" }),
        ),
        StatusCode::CREATED,
        "DELETE /auth/keys/{key_id} kurulum: sahip key'i",
    )
    .await;
    let owner_key_id = owner_new_key["key"]["id"]
        .as_str()
        .expect("key id")
        .to_owned();

    assert_code(
        &router,
        empty_req("DELETE", &format!("/auth/keys/{owner_key_id}")),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /auth/keys/{key_id} [anon]",
    )
    .await;
    // Başkasının key'i: 403 değil 404 (bkz. dosya başındaki incelik #4).
    for (rol, token) in [
        ("normal", &normal_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_code(
            &router,
            auth_req("DELETE", &format!("/auth/keys/{owner_key_id}"), token),
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("DELETE /auth/keys/{{key_id}} (başkasının key'i) [{rol}]"),
        )
        .await;
    }
    assert_status(
        &router,
        auth_req("DELETE", &format!("/auth/keys/{owner_key_id}"), &owner_key),
        StatusCode::NO_CONTENT,
        "DELETE /auth/keys/{key_id} [sahip]",
    )
    .await;
    // Banlı: DELETE güvenli değil, ban kontrolü key'in var olup olmadığına
    // bakmadan devreye giriyor — rastgele bir UUID bile 403 BANNED verir.
    assert_code(
        &router,
        auth_req(
            "DELETE",
            "/auth/keys/00000000-0000-0000-0000-000000000000",
            &banned_key,
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "DELETE /auth/keys/{key_id} [banli]",
    )
    .await;

    // --- POST /auth/recover — kimlik doğrulama gerektirmiyor, ama banlı
    // hesap kurtarma koduyla bile engelleniyor (bkz. incelik #7). ---
    for (rol, username, token) in [
        ("anon", "am_auth_normal", None),
        ("normal", "am_auth_normal", Some(normal_key.as_str())),
        ("moderator", "am_auth_normal", Some(mod_key.as_str())),
        ("admin", "am_auth_normal", Some(admin_key.as_str())),
    ] {
        // `am_auth_normal`'ın kendi kurtarma kodlarını burada yeniden
        // üretmek yerine (her seferinde tüketilecekleri için) taze bir
        // hesap seed'liyoruz — `rol` yalnızca *çağıranın* kimliğini
        // değiştiriyor, kurtarılan hesabı değil (recover'ın davranışı
        // çağırana bakmıyor).
        let recover_username = format!("am_recover_{rol}");
        let (_, _, codes) = seed_actor_with_recovery(&raw_pool, &recover_username).await;
        assert_status(
            &router,
            maybe_auth_json_req(
                "POST",
                "/auth/recover",
                token,
                json!({ "username": recover_username, "recovery_code": codes[0] }),
            ),
            StatusCode::OK,
            &format!("POST /auth/recover [{rol} çağırıyor, hedef banlı değil]"),
        )
        .await;
        let _ = username; // yalnızca döngü etiketleme netliği için
    }
    // "sahip" hücresi: hedef = çağıranın kendisi, banlı değil.
    let (_, _, owner_recovery_codes) =
        seed_actor_with_recovery(&raw_pool, "am_recover_sahip").await;
    assert_status(
        &router,
        maybe_auth_json_req(
            "POST",
            "/auth/recover",
            Some(owner_key.as_str()),
            json!({ "username": "am_recover_sahip", "recovery_code": owner_recovery_codes[0] }),
        ),
        StatusCode::OK,
        "POST /auth/recover [sahip, kendi hesabını kurtarıyor]",
    )
    .await;
    // "banli" hücresi: hedef hesabın KENDİSİ banlı — 403 BANNED.
    assert_code(
        &router,
        maybe_auth_json_req(
            "POST",
            "/auth/recover",
            None,
            json!({ "username": "am_auth_banned", "recovery_code": banned_codes[0] }),
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /auth/recover [banlı hesap kendi kodunu kullanıyor]",
    )
    .await;

    // --- POST /auth/recovery-codes/regenerate — yazma, banlı 403 BANNED ---
    assert_code(
        &router,
        empty_req("POST", "/auth/recovery-codes/regenerate"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "POST /auth/recovery-codes/regenerate [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_req("POST", "/auth/recovery-codes/regenerate", token),
            StatusCode::OK,
            &format!("POST /auth/recovery-codes/regenerate [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req("POST", "/auth/recovery-codes/regenerate", &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /auth/recovery-codes/regenerate [banli]",
    )
    .await;
}

// ============================================================================
// actor_profil_uclarinin_yetki_matrisi — 8 operasyon
// ============================================================================

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn actor_profil_uclarinin_yetki_matrisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (target_id, _target_key) = seed_actor(&raw_pool, "am_prof_target").await;
    let _ = target_id;
    let (_, normal_key) = seed_actor(&raw_pool, "am_prof_normal").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "am_prof_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "am_prof_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;
    let (banned_owner_id, banned_key, _) =
        seed_actor_with_recovery(&raw_pool, "am_prof_banned").await;
    banla(&raw_pool, target_id, "am_prof_banned").await;
    let _ = banned_owner_id;

    // Salt okunur uçlar: tamamen herkese açık, roller arasında hiçbir
    // ayrım yok (anon dahil).
    let public_reads: &[(&str, &str)] = &[
        ("GET", "/actors"),
        ("GET", "/actors/am_prof_target"),
        ("GET", "/actors/am_prof_target/followers"),
        ("GET", "/actors/am_prof_target/following"),
    ];
    for (method, uri) in public_reads {
        assert_status(
            &router,
            empty_req(method, uri),
            StatusCode::OK,
            &format!("{method} {uri} [anon]"),
        )
        .await;
        for (rol, token) in [
            ("normal", &normal_key),
            ("moderator", &mod_key),
            ("admin", &admin_key),
            ("banli", &banned_key),
        ] {
            assert_status(
                &router,
                auth_req(method, uri, token),
                StatusCode::OK,
                &format!("{method} {uri} [{rol}]"),
            )
            .await;
        }
    }

    // --- PATCH /actors/me — her zaman "kendi" profilin, sahip/normal
    // ayrımı bu uçta yok (path'te username yok) — yine de her rol kendi
    // profilini güncelleyebiliyor mu diye ayrı ayrı doğrulanıyor. ---
    assert_code(
        &router,
        maybe_auth_json_req("PATCH", "/actors/me", None, json!({ "display_name": "x" })),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "PATCH /actors/me [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &normal_key), // bu uçta "sahip" == "kendi profilin", normal ile aynı davranış
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_json_req(
                "PATCH",
                "/actors/me",
                token,
                json!({ "display_name": format!("güncellendi-{rol}") }),
            ),
            StatusCode::OK,
            &format!("PATCH /actors/me [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_json_req(
            "PATCH",
            "/actors/me",
            &banned_key,
            json!({ "display_name": "x" }),
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "PATCH /actors/me [banli]",
    )
    .await;

    // --- DELETE /actors/me — geri döndürülemez, her rol için ayrı, tek
    // kullanımlık bir hesap seed'liyoruz. ---
    assert_code(
        &router,
        empty_req("DELETE", "/actors/me"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /actors/me [anon]",
    )
    .await;
    for rol in ["normal", "sahip", "moderator", "admin"] {
        let username = format!("am_delme_{rol}");
        let (actor_id, key, codes) = seed_actor_with_recovery(&raw_pool, &username).await;
        if rol == "moderator" {
            rol_ver(&raw_pool, actor_id, "moderator").await;
        } else if rol == "admin" {
            rol_ver(&raw_pool, actor_id, "admin").await;
        }
        assert_status(
            &router,
            auth_json_req(
                "DELETE",
                "/actors/me",
                &key,
                json!({ "recovery_code": codes[0] }),
            ),
            StatusCode::NO_CONTENT,
            &format!("DELETE /actors/me [{rol}]"),
        )
        .await;
    }
    {
        let (actor_id, key, codes) = seed_actor_with_recovery(&raw_pool, "am_delme_banli").await;
        banla(&raw_pool, actor_id, "am_delme_banli").await;
        assert_code(
            &router,
            auth_json_req(
                "DELETE",
                "/actors/me",
                &key,
                json!({ "recovery_code": codes[0] }),
            ),
            StatusCode::FORBIDDEN,
            "BANNED",
            "DELETE /actors/me [banli]",
        )
        .await;
    }

    // --- POST /actors/me/avatar — "PATCH /actors/me" ile aynı gerekçe:
    // path'te username yok, her zaman kendi avatarın, sahip/normal ayrımı
    // anlamsız. ---
    assert_code(
        &router,
        multipart_upload_req("POST", "/actors/me/avatar", None, &tiny_png()),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "POST /actors/me/avatar [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &normal_key), // bu uçta "sahip" == "kendi profilin", normal ile aynı davranış
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            multipart_upload_req("POST", "/actors/me/avatar", Some(token), &tiny_png()),
            StatusCode::CREATED,
            &format!("POST /actors/me/avatar [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        multipart_upload_req("POST", "/actors/me/avatar", Some(&banned_key), &tiny_png()),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /actors/me/avatar [banli]",
    )
    .await;

    // --- DELETE /actors/me/avatar — idempotent (bkz.
    // `actos_core::avatar::clear_avatar`): the avatar doesn't need to exist
    // beforehand for this to return `204`, so no seeding step is needed
    // per role. ---
    assert_code(
        &router,
        empty_req("DELETE", "/actors/me/avatar"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /actors/me/avatar [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &normal_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_req("DELETE", "/actors/me/avatar", token),
            StatusCode::NO_CONTENT,
            &format!("DELETE /actors/me/avatar [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req("DELETE", "/actors/me/avatar", &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "DELETE /actors/me/avatar [banli]",
    )
    .await;
}

// ============================================================================
// post_uclarinin_yetki_matrisi — 5 operasyon
// ============================================================================

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn post_uclarinin_yetki_matrisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (owner_id, owner_key) = seed_actor(&raw_pool, "am_post_owner").await;
    let (_, normal_key) = seed_actor(&raw_pool, "am_post_normal").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "am_post_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "am_post_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;
    let (banned_id, banned_key, _) = seed_actor_with_recovery(&raw_pool, "am_post_banned").await;
    banla(&raw_pool, owner_id, "am_post_banned").await;
    let _ = banned_id;

    // --- POST /posts ---
    assert_code(
        &router,
        maybe_auth_json_req(
            "POST",
            "/posts",
            None,
            json!({ "title": "x", "body": "y", "tags": [] }),
        ),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "POST /posts [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_json_req(
                "POST",
                "/posts",
                token,
                json!({ "title": format!("post-{rol}"), "body": "gövde", "tags": [] }),
            ),
            StatusCode::CREATED,
            &format!("POST /posts [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_json_req(
            "POST",
            "/posts",
            &banned_key,
            json!({ "title": "x", "body": "y", "tags": [] }),
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /posts [banli]",
    )
    .await;

    // --- GET /posts/{id} — tamamen herkese açık okuma ---
    let okuma_postu = seed_post(&router, &owner_key, "okuma testi").await;
    let uri = format!("/posts/{okuma_postu}");
    assert_status(
        &router,
        empty_req("GET", &uri),
        StatusCode::OK,
        "GET /posts/{id} [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", &uri, token),
            StatusCode::OK,
            &format!("GET /posts/{{id}} [{rol}]"),
        )
        .await;
    }

    // --- PATCH /posts/{id} — moderatör/admin override YOK (incelik #2) ---
    let patch_hedefi = seed_post(&router, &owner_key, "patch hedefi").await;
    let patch_uri = format!("/posts/{patch_hedefi}");
    assert_code(
        &router,
        maybe_auth_json_req("PATCH", &patch_uri, None, json!({ "title": "x" })),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "PATCH /posts/{id} [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_code(
            &router,
            auth_json_req("PATCH", &patch_uri, token, json!({ "title": "başkasının" })),
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            &format!("PATCH /posts/{{id}} (başkasının postu) [{rol}]"),
        )
        .await;
    }
    assert_status(
        &router,
        auth_json_req(
            "PATCH",
            &patch_uri,
            &owner_key,
            json!({ "title": "güncellendi" }),
        ),
        StatusCode::OK,
        "PATCH /posts/{id} [sahip]",
    )
    .await;
    assert_code(
        &router,
        auth_json_req("PATCH", &patch_uri, &banned_key, json!({ "title": "x" })),
        StatusCode::FORBIDDEN,
        "BANNED",
        "PATCH /posts/{id} [banli]",
    )
    .await;

    // --- DELETE /posts/{id} — moderatör/admin override VAR (delete_post,
    // update_post'un aksine `roles` alıyor). ---
    assert_code(
        &router,
        empty_req(
            "DELETE",
            &format!(
                "/posts/{}",
                seed_post(&router, &owner_key, "anon delete").await
            ),
        ),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /posts/{id} [anon]",
    )
    .await;
    let normal_hedef = seed_post(&router, &owner_key, "normal delete hedefi").await;
    assert_code(
        &router,
        auth_req("DELETE", &format!("/posts/{normal_hedef}"), &normal_key),
        StatusCode::FORBIDDEN,
        "FORBIDDEN",
        "DELETE /posts/{id} (başkasının postu) [normal]",
    )
    .await;
    let sahip_hedef = seed_post(&router, &owner_key, "sahip delete hedefi").await;
    assert_status(
        &router,
        auth_req("DELETE", &format!("/posts/{sahip_hedef}"), &owner_key),
        StatusCode::NO_CONTENT,
        "DELETE /posts/{id} [sahip]",
    )
    .await;
    let mod_hedef = seed_post(&router, &owner_key, "mod delete hedefi").await;
    assert_status(
        &router,
        auth_req("DELETE", &format!("/posts/{mod_hedef}"), &mod_key),
        StatusCode::NO_CONTENT,
        "DELETE /posts/{id} (override) [moderator]",
    )
    .await;
    let admin_hedef = seed_post(&router, &owner_key, "admin delete hedefi").await;
    assert_status(
        &router,
        auth_req("DELETE", &format!("/posts/{admin_hedef}"), &admin_key),
        StatusCode::NO_CONTENT,
        "DELETE /posts/{id} (override) [admin]",
    )
    .await;
    let banli_hedef = seed_post(&router, &owner_key, "banli delete hedefi").await;
    assert_code(
        &router,
        auth_req("DELETE", &format!("/posts/{banli_hedef}"), &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "DELETE /posts/{id} [banli]",
    )
    .await;

    // --- GET /actors/{username}/posts — herkese açık ---
    let liste_uri = "/actors/am_post_owner/posts";
    assert_status(
        &router,
        empty_req("GET", liste_uri),
        StatusCode::OK,
        "GET /actors/{username}/posts [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", liste_uri, token),
            StatusCode::OK,
            &format!("GET /actors/{{username}}/posts [{rol}]"),
        )
        .await;
    }
}

// ============================================================================
// comment_uclarinin_yetki_matrisi — 6 operasyon
// ============================================================================

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn comment_uclarinin_yetki_matrisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (owner_id, owner_key) = seed_actor(&raw_pool, "am_com_owner").await;
    let (_, normal_key) = seed_actor(&raw_pool, "am_com_normal").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "am_com_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "am_com_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;
    let (banned_id, banned_key, _) = seed_actor_with_recovery(&raw_pool, "am_com_banned").await;
    banla(&raw_pool, owner_id, "am_com_banned").await;
    let _ = banned_id;

    let post_id = seed_post(&router, &owner_key, "yorum ana postu").await;
    let comments_uri = format!("/posts/{post_id}/comments");

    // --- POST /posts/{id}/comments — herkese açık yazma (sahiplik yok) ---
    assert_code(
        &router,
        maybe_auth_json_req("POST", &comments_uri, None, json!({ "body": "x" })),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "POST /posts/{id}/comments [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key), // post sahibi kendi postuna yorum yapıyor
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_json_req(
                "POST",
                &comments_uri,
                token,
                json!({ "body": format!("yorum-{rol}") }),
            ),
            StatusCode::CREATED,
            &format!("POST /posts/{{id}}/comments [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_json_req("POST", &comments_uri, &banned_key, json!({ "body": "x" })),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /posts/{id}/comments [banli]",
    )
    .await;

    // --- GET /posts/{id}/comments — herkese açık okuma ---
    assert_status(
        &router,
        empty_req("GET", &comments_uri),
        StatusCode::OK,
        "GET /posts/{id}/comments [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", &comments_uri, token),
            StatusCode::OK,
            &format!("GET /posts/{{id}}/comments [{rol}]"),
        )
        .await;
    }

    // --- GET /comments/{id} — herkese açık okuma ---
    let ornek_yorum = seed_comment(&router, &owner_key, &post_id, "örnek yorum").await;
    let yorum_uri = format!("/comments/{ornek_yorum}");
    assert_status(
        &router,
        empty_req("GET", &yorum_uri),
        StatusCode::OK,
        "GET /comments/{id} [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", &yorum_uri, token),
            StatusCode::OK,
            &format!("GET /comments/{{id}} [{rol}]"),
        )
        .await;
    }

    // --- PATCH /comments/{id} — moderatör/admin override YOK (post ile
    // aynı incelik #2) ---
    let patch_hedefi = seed_comment(&router, &owner_key, &post_id, "patch hedefi").await;
    let patch_uri = format!("/comments/{patch_hedefi}");
    assert_code(
        &router,
        maybe_auth_json_req("PATCH", &patch_uri, None, json!({ "body": "x" })),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "PATCH /comments/{id} [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_code(
            &router,
            auth_json_req("PATCH", &patch_uri, token, json!({ "body": "başkasının" })),
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            &format!("PATCH /comments/{{id}} (başkasının yorumu) [{rol}]"),
        )
        .await;
    }
    assert_status(
        &router,
        auth_json_req(
            "PATCH",
            &patch_uri,
            &owner_key,
            json!({ "body": "güncellendi" }),
        ),
        StatusCode::OK,
        "PATCH /comments/{id} [sahip]",
    )
    .await;
    assert_code(
        &router,
        auth_json_req("PATCH", &patch_uri, &banned_key, json!({ "body": "x" })),
        StatusCode::FORBIDDEN,
        "BANNED",
        "PATCH /comments/{id} [banli]",
    )
    .await;

    // --- DELETE /comments/{id} — moderatör/admin override VAR ---
    assert_code(
        &router,
        empty_req(
            "DELETE",
            &format!(
                "/comments/{}",
                seed_comment(&router, &owner_key, &post_id, "anon delete").await
            ),
        ),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /comments/{id} [anon]",
    )
    .await;
    let normal_hedef = seed_comment(&router, &owner_key, &post_id, "normal delete hedefi").await;
    assert_code(
        &router,
        auth_req("DELETE", &format!("/comments/{normal_hedef}"), &normal_key),
        StatusCode::FORBIDDEN,
        "FORBIDDEN",
        "DELETE /comments/{id} (başkasının yorumu) [normal]",
    )
    .await;
    let sahip_hedef = seed_comment(&router, &owner_key, &post_id, "sahip delete hedefi").await;
    assert_status(
        &router,
        auth_req("DELETE", &format!("/comments/{sahip_hedef}"), &owner_key),
        StatusCode::NO_CONTENT,
        "DELETE /comments/{id} [sahip]",
    )
    .await;
    let mod_hedef = seed_comment(&router, &owner_key, &post_id, "mod delete hedefi").await;
    assert_status(
        &router,
        auth_req("DELETE", &format!("/comments/{mod_hedef}"), &mod_key),
        StatusCode::NO_CONTENT,
        "DELETE /comments/{id} (override) [moderator]",
    )
    .await;
    let admin_hedef = seed_comment(&router, &owner_key, &post_id, "admin delete hedefi").await;
    assert_status(
        &router,
        auth_req("DELETE", &format!("/comments/{admin_hedef}"), &admin_key),
        StatusCode::NO_CONTENT,
        "DELETE /comments/{id} (override) [admin]",
    )
    .await;
    let banli_hedef = seed_comment(&router, &owner_key, &post_id, "banli delete hedefi").await;
    assert_code(
        &router,
        auth_req("DELETE", &format!("/comments/{banli_hedef}"), &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "DELETE /comments/{id} [banli]",
    )
    .await;

    // --- GET /actors/{username}/comments — herkese açık ---
    let liste_uri = "/actors/am_com_owner/comments";
    assert_status(
        &router,
        empty_req("GET", liste_uri),
        StatusCode::OK,
        "GET /actors/{username}/comments [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", liste_uri, token),
            StatusCode::OK,
            &format!("GET /actors/{{username}}/comments [{rol}]"),
        )
        .await;
    }
}

// ============================================================================
// interaction_uclarinin_yetki_matrisi — 7 operasyon
// ============================================================================

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn interaction_uclarinin_yetki_matrisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (owner_id, owner_key) = seed_actor(&raw_pool, "am_int_owner").await;
    let (_, normal_key) = seed_actor(&raw_pool, "am_int_normal").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "am_int_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "am_int_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;
    let (banned_id, banned_key, _) = seed_actor_with_recovery(&raw_pool, "am_int_banned").await;
    banla(&raw_pool, owner_id, "am_int_banned").await;
    let _ = banned_id;

    // --- PUT /contents/{id}/vote — sahip kendi içeriğine oy veremez
    // (incelik #5): tek satırda beklenen `403`, diğerlerinde `200`. ---
    let vote_hedefi_1 = seed_post(&router, &owner_key, "oy hedefi anon").await;
    assert_code(
        &router,
        maybe_auth_json_req(
            "PUT",
            &format!("/contents/{vote_hedefi_1}/vote"),
            None,
            json!({ "value": 1 }),
        ),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "PUT /contents/{id}/vote [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        let hedef = seed_post(&router, &owner_key, &format!("oy hedefi {rol}")).await;
        assert_status(
            &router,
            auth_json_req(
                "PUT",
                &format!("/contents/{hedef}/vote"),
                token,
                json!({ "value": 1 }),
            ),
            StatusCode::OK,
            &format!("PUT /contents/{{id}}/vote (başkasının içeriği) [{rol}]"),
        )
        .await;
    }
    let sahip_hedef = seed_post(&router, &owner_key, "oy hedefi sahip").await;
    assert_code(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{sahip_hedef}/vote"),
            &owner_key,
            json!({ "value": 1 }),
        ),
        StatusCode::FORBIDDEN,
        "FORBIDDEN",
        "PUT /contents/{id}/vote (kendi içeriğine) [sahip]",
    )
    .await;
    let banli_hedef = seed_post(&router, &owner_key, "oy hedefi banli").await;
    assert_code(
        &router,
        auth_json_req(
            "PUT",
            &format!("/contents/{banli_hedef}/vote"),
            &banned_key,
            json!({ "value": 1 }),
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "PUT /contents/{id}/vote [banli]",
    )
    .await;

    // --- GET /me/votes ---
    assert_code(
        &router,
        empty_req("GET", "/me/votes"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "GET /me/votes [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", "/me/votes", token),
            StatusCode::OK,
            &format!("GET /me/votes [{rol}]"),
        )
        .await;
    }

    // --- PUT /contents/{id}/save — sahiplik kısıtı yok, kendi içeriğini
    // de kaydedebilir. ---
    let kaydet_hedefi = seed_post(&router, &owner_key, "kaydetme hedefi").await;
    assert_code(
        &router,
        empty_req("PUT", &format!("/contents/{kaydet_hedefi}/save")),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "PUT /contents/{id}/save [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_req("PUT", &format!("/contents/{kaydet_hedefi}/save"), token),
            StatusCode::NO_CONTENT,
            &format!("PUT /contents/{{id}}/save [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req(
            "PUT",
            &format!("/contents/{kaydet_hedefi}/save"),
            &banned_key,
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "PUT /contents/{id}/save [banli]",
    )
    .await;

    // --- DELETE /contents/{id}/save — idempotent, kaydı olmasa da 204 ---
    assert_code(
        &router,
        empty_req("DELETE", &format!("/contents/{kaydet_hedefi}/save")),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /contents/{id}/save [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_req("DELETE", &format!("/contents/{kaydet_hedefi}/save"), token),
            StatusCode::NO_CONTENT,
            &format!("DELETE /contents/{{id}}/save [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req(
            "DELETE",
            &format!("/contents/{kaydet_hedefi}/save"),
            &banned_key,
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "DELETE /contents/{id}/save [banli]",
    )
    .await;

    // --- PUT /actors/{username}/follow — sahip = kendini takip = 400
    // (incelik #6). ---
    assert_code(
        &router,
        empty_req("PUT", "/actors/am_int_owner/follow"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "PUT /actors/{username}/follow [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_req("PUT", "/actors/am_int_owner/follow", token),
            StatusCode::NO_CONTENT,
            &format!("PUT /actors/{{username}}/follow [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req("PUT", "/actors/am_int_owner/follow", &owner_key),
        StatusCode::BAD_REQUEST,
        "VALIDATION_FAILED",
        "PUT /actors/{username}/follow (kendini takip) [sahip]",
    )
    .await;
    assert_code(
        &router,
        auth_req("PUT", "/actors/am_int_owner/follow", &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "PUT /actors/{username}/follow [banli]",
    )
    .await;

    // --- DELETE /actors/{username}/follow — idempotent, kendi kendini
    // "unfollow" da hata değil (yalnızca DELETE FROM, satır etkilenmese
    // de 204). ---
    assert_code(
        &router,
        empty_req("DELETE", "/actors/am_int_owner/follow"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /actors/{username}/follow [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_req("DELETE", "/actors/am_int_owner/follow", token),
            StatusCode::NO_CONTENT,
            &format!("DELETE /actors/{{username}}/follow [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req("DELETE", "/actors/am_int_owner/follow", &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "DELETE /actors/{username}/follow [banli]",
    )
    .await;

    // --- GET /me/saves ---
    assert_code(
        &router,
        empty_req("GET", "/me/saves"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "GET /me/saves [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", "/me/saves", token),
            StatusCode::OK,
            &format!("GET /me/saves [{rol}]"),
        )
        .await;
    }
}

// ============================================================================
// notification_uclarinin_yetki_matrisi — 3 operasyon
// ============================================================================

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn notification_uclarinin_yetki_matrisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (owner_id, owner_key) = seed_actor(&raw_pool, "am_notif_owner").await;
    let (_, normal_key) = seed_actor(&raw_pool, "am_notif_normal").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "am_notif_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "am_notif_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;
    let (banned_id, banned_key, _) = seed_actor_with_recovery(&raw_pool, "am_notif_banned").await;
    banla(&raw_pool, owner_id, "am_notif_banned").await;
    let _ = banned_id;

    // `owner`'ın gelen kutusuna bir bildirim düşürmenin en basit yolu:
    // biri onu takip etsin (`NewFollower`, bkz. `actos_core::interaction::
    // follow`).
    assert_status(
        &router,
        auth_req("PUT", "/actors/am_notif_owner/follow", &normal_key),
        StatusCode::NO_CONTENT,
        "kurulum: am_notif_normal → am_notif_owner takip",
    )
    .await;

    // --- GET /me/inbox ---
    assert_code(
        &router,
        empty_req("GET", "/me/inbox"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "GET /me/inbox [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", "/me/inbox", token),
            StatusCode::OK,
            &format!("GET /me/inbox [{rol}]"),
        )
        .await;
    }

    let inbox = assert_status(
        &router,
        auth_req("GET", "/me/inbox", &owner_key),
        StatusCode::OK,
        "GET /me/inbox (id almak için)",
    )
    .await;
    let notification_id = inbox["notifications"][0]["id"]
        .as_str()
        .expect("owner'ın gelen kutusunda en az bir bildirim olmalı")
        .to_owned();
    let mark_uri = format!("/me/inbox/{notification_id}/read");

    // --- PATCH /me/inbox/{id}/read ---
    assert_code(
        &router,
        empty_req("PATCH", &mark_uri),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "PATCH /me/inbox/{id}/read [anon]",
    )
    .await;
    // Başkasının bildirimi: var olmadığı gibi davranıyor (404, sızıntı yok).
    for (rol, token) in [
        ("normal", &normal_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_code(
            &router,
            auth_req("PATCH", &mark_uri, token),
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            &format!("PATCH /me/inbox/{{id}}/read (başkasının bildirimi) [{rol}]"),
        )
        .await;
    }
    assert_status(
        &router,
        auth_req("PATCH", &mark_uri, &owner_key),
        StatusCode::NO_CONTENT,
        "PATCH /me/inbox/{id}/read [sahip]",
    )
    .await;
    // Banlı: PATCH güvenli değil, ban kontrolü bildirim var mı diye hiç
    // bakmadan devreye giriyor.
    assert_code(
        &router,
        auth_req("PATCH", &mark_uri, &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "PATCH /me/inbox/{id}/read [banli]",
    )
    .await;

    // --- POST /me/inbox/read — toplu işaretleme, POST olduğu için banlı
    // yine engelleniyor (kendi bildirimlerini okundu işaretlemek bile
    // "yazma" sayılıyor — bkz. dosya başındaki genel ban gerekçesi). ---
    assert_code(
        &router,
        empty_req("POST", "/me/inbox/read"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "POST /me/inbox/read [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        assert_status(
            &router,
            auth_req("POST", "/me/inbox/read", token),
            StatusCode::OK,
            &format!("POST /me/inbox/read [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req("POST", "/me/inbox/read", &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /me/inbox/read [banli]",
    )
    .await;
}

// ============================================================================
// feed_arama_etiket_uclarinin_yetki_matrisi — 6 operasyon
// ============================================================================

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn feed_arama_etiket_uclarinin_yetki_matrisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (owner_id, owner_key) = seed_actor(&raw_pool, "am_feed_owner").await;
    let (_, normal_key) = seed_actor(&raw_pool, "am_feed_normal").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "am_feed_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "am_feed_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;
    let (banned_id, banned_key, _) = seed_actor_with_recovery(&raw_pool, "am_feed_banned").await;
    banla(&raw_pool, owner_id, "am_feed_banned").await;
    let _ = banned_id;

    seed_post_with_tag(&router, &owner_key, "etiketli post", "amyetkietiketi").await;

    // --- GET /feed — kimlik gerektirmiyor, tamamen herkese açık ---
    assert_status(
        &router,
        empty_req("GET", "/feed"),
        StatusCode::OK,
        "GET /feed [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", "/feed", token),
            StatusCode::OK,
            &format!("GET /feed [{rol}]"),
        )
        .await;
    }

    // --- GET /feed/following — kimlik gerekli ---
    assert_code(
        &router,
        empty_req("GET", "/feed/following"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "GET /feed/following [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("sahip", &owner_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
        ("banli", &banned_key),
    ] {
        assert_status(
            &router,
            auth_req("GET", "/feed/following", token),
            StatusCode::OK,
            &format!("GET /feed/following [{rol}]"),
        )
        .await;
    }

    // --- GET /search, GET /tags, GET /tags/search, GET /tags/{name}/posts
    // — dördü de tamamen herkese açık okuma. ---
    let public_reads = [
        "/search?type=post&q=etiketli",
        "/tags",
        "/tags/search?q=amyetkietiketi",
        "/tags/amyetkietiketi/posts",
    ];
    for uri in public_reads {
        assert_status(
            &router,
            empty_req("GET", uri),
            StatusCode::OK,
            &format!("GET {uri} [anon]"),
        )
        .await;
        for (rol, token) in [
            ("normal", &normal_key),
            ("sahip", &owner_key),
            ("moderator", &mod_key),
            ("admin", &admin_key),
            ("banli", &banned_key),
        ] {
            assert_status(
                &router,
                auth_req("GET", uri, token),
                StatusCode::OK,
                &format!("GET {uri} [{rol}]"),
            )
            .await;
        }
    }
}

// ============================================================================
// moderasyon_uclarinin_yetki_matrisi — 9 operasyon
// ============================================================================

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
#[allow(clippy::too_many_lines)]
async fn moderasyon_uclarinin_yetki_matrisi(pool: PgPool) {
    let raw_pool = pool.clone();
    let router = build_router(pool);

    let (owner_id, owner_key) = seed_actor(&raw_pool, "am_mod_owner").await;
    let (_, normal_key) = seed_actor(&raw_pool, "am_mod_normal").await;
    let (mod_id, mod_key) = seed_actor(&raw_pool, "am_mod_mod").await;
    rol_ver(&raw_pool, mod_id, "moderator").await;
    let (admin_id, admin_key) = seed_actor(&raw_pool, "am_mod_admin").await;
    rol_ver(&raw_pool, admin_id, "admin").await;
    let (banned_id, banned_key, _) = seed_actor_with_recovery(&raw_pool, "am_mod_banned").await;
    banla(&raw_pool, owner_id, "am_mod_banned").await;
    let _ = banned_id;

    // --- POST /reports — herkese açık (yalnızca kimlikli), sahiplik yok
    // (kendi içeriğini bile şikayet edebilir). ---
    let sikayet_hedefi = |suffix: &str| format!("şikayet hedefi {suffix}");
    assert_code(
        &router,
        maybe_auth_json_req(
            "POST",
            "/reports",
            None,
            json!({ "target_type": "post", "target_id": "c_x", "reason": "spam" }),
        ),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "POST /reports [anon]",
    )
    .await;
    for (rol, token) in [
        ("normal", &normal_key),
        ("moderator", &mod_key),
        ("admin", &admin_key),
    ] {
        let post = seed_post(&router, &owner_key, &sikayet_hedefi(rol)).await;
        assert_status(
            &router,
            auth_json_req(
                "POST",
                "/reports",
                token,
                json!({ "target_type": "post", "target_id": post, "reason": "spam" }),
            ),
            StatusCode::CREATED,
            &format!("POST /reports [{rol}]"),
        )
        .await;
    }
    let sahip_sikayet_hedefi = seed_post(&router, &owner_key, "kendi postunu şikayet").await;
    assert_status(
        &router,
        auth_json_req(
            "POST",
            "/reports",
            &owner_key,
            json!({ "target_type": "post", "target_id": sahip_sikayet_hedefi, "reason": "spam" }),
        ),
        StatusCode::CREATED,
        "POST /reports (kendi içeriğini şikayet) [sahip]",
    )
    .await;
    let banli_sikayet_hedefi = seed_post(&router, &owner_key, "banlı şikayet hedefi").await;
    assert_code(
        &router,
        auth_json_req(
            "POST",
            "/reports",
            &banned_key,
            json!({ "target_type": "post", "target_id": banli_sikayet_hedefi, "reason": "spam" }),
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /reports [banli]",
    )
    .await;

    // Kuyrukta en az bir bekleyen şikayet olsun (aşağıdaki PATCH testi için).
    let raporlanan_post = seed_post(&router, &owner_key, "kuyruk testi postu").await;
    let rapor = assert_status(
        &router,
        auth_json_req(
            "POST",
            "/reports",
            &normal_key,
            json!({ "target_type": "post", "target_id": raporlanan_post, "reason": "kuyruk testi" }),
        ),
        StatusCode::CREATED,
        "kurulum: kuyruk raporu",
    )
    .await;
    let rapor_id = rapor["id"].as_str().expect("rapor id").to_owned();

    // --- GET /admin/reports — GÜVENLİ metot: banlı için ban kontrolü hiç
    // devreye girmiyor, sonuç `403 FORBIDDEN` (BANNED değil — incelik #1). ---
    assert_code(
        &router,
        empty_req("GET", "/admin/reports"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "GET /admin/reports [anon]",
    )
    .await;
    for (rol, token) in [("normal", &normal_key), ("sahip", &owner_key)] {
        assert_code(
            &router,
            auth_req("GET", "/admin/reports", token),
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            &format!("GET /admin/reports [{rol}]"),
        )
        .await;
    }
    for (rol, token) in [("moderator", &mod_key), ("admin", &admin_key)] {
        assert_status(
            &router,
            auth_req("GET", "/admin/reports", token),
            StatusCode::OK,
            &format!("GET /admin/reports [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req("GET", "/admin/reports", &banned_key),
        StatusCode::FORBIDDEN,
        "FORBIDDEN", // BANNED DEĞİL — bkz. dosya başındaki incelik #1.
        "GET /admin/reports [banli] (GET güvenli, ban kontrolüne takılmıyor)",
    )
    .await;

    // --- PATCH /admin/reports/{id} — güvenli değil, banlı 403 BANNED ---
    let patch_uri = format!("/admin/reports/{rapor_id}");
    assert_code(
        &router,
        maybe_auth_json_req("PATCH", &patch_uri, None, json!({ "status": "resolved" })),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "PATCH /admin/reports/{id} [anon]",
    )
    .await;
    for (rol, token) in [("normal", &normal_key), ("sahip", &owner_key)] {
        assert_code(
            &router,
            auth_json_req("PATCH", &patch_uri, token, json!({ "status": "resolved" })),
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            &format!("PATCH /admin/reports/{{id}} [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_json_req(
            "PATCH",
            &patch_uri,
            &banned_key,
            json!({ "status": "resolved" }),
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "PATCH /admin/reports/{id} [banli]",
    )
    .await;
    assert_status(
        &router,
        auth_json_req(
            "PATCH",
            &patch_uri,
            &mod_key,
            json!({ "status": "resolved" }),
        ),
        StatusCode::OK,
        "PATCH /admin/reports/{id} [moderator]",
    )
    .await;
    // Admin için ayrı bir rapor (bir öncekini moderatör zaten çözdü).
    let post2 = seed_post(&router, &owner_key, "ikinci kuyruk postu").await;
    let rapor2 = assert_status(
        &router,
        auth_json_req(
            "POST",
            "/reports",
            &normal_key,
            json!({ "target_type": "post", "target_id": post2, "reason": "admin testi" }),
        ),
        StatusCode::CREATED,
        "kurulum: admin için ikinci rapor",
    )
    .await;
    let rapor2_id = rapor2["id"].as_str().expect("rapor id").to_owned();
    assert_status(
        &router,
        auth_json_req(
            "PATCH",
            &format!("/admin/reports/{rapor2_id}"),
            &admin_key,
            json!({ "status": "resolved" }),
        ),
        StatusCode::OK,
        "PATCH /admin/reports/{id} [admin]",
    )
    .await;

    // --- DELETE /admin/contents/{id} — güvenli değil, banlı 403 BANNED,
    // sahiplik moderatör/admin için önemsiz (override zaten var). ---
    assert_code(
        &router,
        empty_req(
            "DELETE",
            &format!(
                "/admin/contents/{}",
                seed_post(&router, &owner_key, "anon mod-delete").await
            ),
        ),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /admin/contents/{id} [anon]",
    )
    .await;
    let normal_hedef = seed_post(&router, &owner_key, "normal mod-delete hedefi").await;
    assert_code(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{normal_hedef}"),
            &normal_key,
            json!({ "reason": "test" }),
        ),
        StatusCode::FORBIDDEN,
        "FORBIDDEN",
        "DELETE /admin/contents/{id} [normal]",
    )
    .await;
    let sahip_hedef = seed_post(&router, &owner_key, "sahip mod-delete hedefi").await;
    assert_code(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{sahip_hedef}"),
            &owner_key,
            json!({ "reason": "test" }),
        ),
        StatusCode::FORBIDDEN,
        "FORBIDDEN",
        "DELETE /admin/contents/{id} (kendi içeriği ama moderatör değil) [sahip]",
    )
    .await;
    let mod_hedef = seed_post(&router, &owner_key, "mod mod-delete hedefi").await;
    assert_status(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{mod_hedef}"),
            &mod_key,
            json!({ "reason": "test" }),
        ),
        StatusCode::NO_CONTENT,
        "DELETE /admin/contents/{id} [moderator]",
    )
    .await;
    let admin_hedef = seed_post(&router, &owner_key, "admin mod-delete hedefi").await;
    assert_status(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{admin_hedef}"),
            &admin_key,
            json!({ "reason": "test" }),
        ),
        StatusCode::NO_CONTENT,
        "DELETE /admin/contents/{id} [admin]",
    )
    .await;
    let banli_hedef = seed_post(&router, &owner_key, "banli mod-delete hedefi").await;
    assert_code(
        &router,
        auth_json_req(
            "DELETE",
            &format!("/admin/contents/{banli_hedef}"),
            &banned_key,
            json!({ "reason": "test" }),
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "DELETE /admin/contents/{id} [banli]",
    )
    .await;

    // --- POST /admin/bans ---
    assert_code(
        &router,
        maybe_auth_json_req(
            "POST",
            "/admin/bans",
            None,
            json!({ "username": "kimse", "reason": "x" }),
        ),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "POST /admin/bans [anon]",
    )
    .await;
    for (rol, token) in [("normal", &normal_key), ("sahip", &owner_key)] {
        assert_code(
            &router,
            auth_json_req(
                "POST",
                "/admin/bans",
                token,
                json!({ "username": "am_mod_normal", "reason": "x" }),
            ),
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            &format!("POST /admin/bans [{rol}]"),
        )
        .await;
    }
    for (rol, token) in [("moderator", &mod_key), ("admin", &admin_key)] {
        let hedef_kullanici = format!("am_ban_hedefi_{rol}");
        seed_actor(&raw_pool, &hedef_kullanici).await;
        assert_status(
            &router,
            auth_json_req(
                "POST",
                "/admin/bans",
                token,
                json!({ "username": hedef_kullanici, "reason": "test" }),
            ),
            StatusCode::CREATED,
            &format!("POST /admin/bans [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_json_req(
            "POST",
            "/admin/bans",
            &banned_key,
            json!({ "username": "am_mod_normal", "reason": "x" }),
        ),
        StatusCode::FORBIDDEN,
        "BANNED",
        "POST /admin/bans [banli]",
    )
    .await;

    // --- DELETE /admin/bans/{username} — idempotent, moderatör/admin için
    // ban var olmasa da 204. ---
    assert_code(
        &router,
        empty_req("DELETE", "/admin/bans/am_mod_normal"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "DELETE /admin/bans/{username} [anon]",
    )
    .await;
    for (rol, token) in [("normal", &normal_key), ("sahip", &owner_key)] {
        assert_code(
            &router,
            auth_req("DELETE", "/admin/bans/am_mod_normal", token),
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            &format!("DELETE /admin/bans/{{username}} [{rol}]"),
        )
        .await;
    }
    for (rol, token) in [("moderator", &mod_key), ("admin", &admin_key)] {
        assert_status(
            &router,
            auth_req("DELETE", "/admin/bans/am_mod_normal", token),
            StatusCode::NO_CONTENT,
            &format!("DELETE /admin/bans/{{username}} [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req("DELETE", "/admin/bans/am_mod_normal", &banned_key),
        StatusCode::FORBIDDEN,
        "BANNED",
        "DELETE /admin/bans/{username} [banli]",
    )
    .await;

    // --- PUT/DELETE /admin/permissions — yalnızca `role.grant` (admin);
    // moderatör için de 403. ---
    let izin_govdesi = json!({ "username": "am_mod_role_hedefi", "permission": "content.delete" });
    for method in ["PUT", "DELETE"] {
        assert_code(
            &router,
            maybe_auth_json_req(method, "/admin/permissions", None, izin_govdesi.clone()),
            StatusCode::UNAUTHORIZED,
            "MISSING_CREDENTIALS",
            &format!("{method} /admin/permissions [anon]"),
        )
        .await;
        for (rol, token) in [
            ("normal", &normal_key),
            ("sahip", &owner_key),
            ("moderator", &mod_key),
        ] {
            assert_code(
                &router,
                auth_json_req(method, "/admin/permissions", token, izin_govdesi.clone()),
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                &format!("{method} /admin/permissions [{rol}]"),
            )
            .await;
        }
        assert_code(
            &router,
            auth_json_req(
                method,
                "/admin/permissions",
                &banned_key,
                izin_govdesi.clone(),
            ),
            StatusCode::FORBIDDEN,
            "BANNED",
            &format!("{method} /admin/permissions [banli]"),
        )
        .await;
    }
    // Ayrı, tek kullanımlık bir hedef: `am_mod_normal`'ın kendisine burada
    // gerçekten izin verilseydi, dosyanın geri kalanındaki "normal" rolü
    // hücreleri (ör. aşağıdaki `GET /admin/actions [normal]`) artık normal
    // bir actor'ü değil izinli bir aktörü sınardı.
    seed_actor(&raw_pool, "am_mod_role_hedefi").await;
    assert_status(
        &router,
        auth_json_req(
            "PUT",
            "/admin/permissions",
            &admin_key,
            json!({ "username": "am_mod_role_hedefi", "permission": "content.delete" }),
        ),
        StatusCode::NO_CONTENT,
        "PUT /admin/permissions [admin]",
    )
    .await;
    assert_status(
        &router,
        auth_json_req(
            "DELETE",
            "/admin/permissions",
            &admin_key,
            json!({ "username": "am_mod_role_hedefi", "permission": "content.delete" }),
        ),
        StatusCode::NO_CONTENT,
        "DELETE /admin/permissions [admin]",
    )
    .await;

    // --- GET /admin/actions — GÜVENLİ metot: banlı yine `403 FORBIDDEN`
    // (BANNED değil — incelik #1 ile aynı). ---
    assert_code(
        &router,
        empty_req("GET", "/admin/actions"),
        StatusCode::UNAUTHORIZED,
        "MISSING_CREDENTIALS",
        "GET /admin/actions [anon]",
    )
    .await;
    for (rol, token) in [("normal", &normal_key), ("sahip", &owner_key)] {
        assert_code(
            &router,
            auth_req("GET", "/admin/actions", token),
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            &format!("GET /admin/actions [{rol}]"),
        )
        .await;
    }
    for (rol, token) in [("moderator", &mod_key), ("admin", &admin_key)] {
        assert_status(
            &router,
            auth_req("GET", "/admin/actions", token),
            StatusCode::OK,
            &format!("GET /admin/actions [{rol}]"),
        )
        .await;
    }
    assert_code(
        &router,
        auth_req("GET", "/admin/actions", &banned_key),
        StatusCode::FORBIDDEN,
        "FORBIDDEN",
        "GET /admin/actions [banli] (GET güvenli, ban kontrolüne takılmıyor)",
    )
    .await;
}
