//! OpenAPI dokümantasyonunun paylaşılan iskeleti (Faz 16).
//!
//! Bu modül üç şeyi bir arada tutuyor:
//!
//! 1. **`ApiDoc`** — `#[derive(utoipa::OpenApi)]` ile üretilen kök `OpenApi`
//!    değeri. Yalnızca **statik** meta veriyi (başlık, etiketler, güvenlik
//!    şeması) taşıyor — **yollar burada YOK**. Yollar `crate::routes::mod`
//!    içinde `OpenApiRouter::routes(routes!(handler))` ile, her `routes/*.rs`
//!    dosyasındaki `#[utoipa::path]` anotasyonundan derleme zamanında
//!    toplanıyor (bkz. o modülün dokümantasyonu — kararın gerekçesi orada:
//!    spec'in koddan sapması derleme zamanında imkânsız olsun istiyoruz).
//!
//! 2. **`SecurityAddon`** — `utoipa::Modify` uygulaması. API key güvenlik
//!    şemasını (`Authorization: Bearer actos_<key_id>_<secret>`) OpenAPI
//!    `components.securitySchemes`'e ekliyor. Bunun `#[openapi(security(...))]`
//!    ile **global** değil, her `#[utoipa::path]`'te ayrı ayrı
//!    (`security(("api_key" = []))`) eklenmesinin sebebi: uçların çoğu
//!    (`GET /feed`, `GET /posts/{id}`, ...) kimlik gerektirmiyor — global bir
//!    güvenlik şartı bunu yanlış yansıtırdı.
//!
//! 3. **Tekrar kullanılan hata yanıtları** (`Unauthorized`, `Forbidden`, ...) —
//!    `utoipa::IntoResponses` ile tanımlı. 41 ucun her birinde aynı `401`/
//!    `403`/`404`/`410`/`429` gövdesini (`ProblemDetails`, RFC 9457) elle
//!    yeniden yazmak hem ~200 tekrar demek hem de bir değişiklik (ör. `429`
//!    header listesine yeni bir header eklenmesi) yapıldığında bazı uçlarda
//!    unutulma riski taşırdı. `responses(RateLimited, NotFound, ...)` her
//!    `#[utoipa::path]`'te bunları tek satırda yeniden kullanıyor.
//!
//!    Bu struct'lar hiç örneklenmiyor — yalnızca `responses(...)` içinde tip
//!    olarak referans veriliyor, derleme zamanında şema üretimi için var
//!    oluyorlar. `ProblemDetails` alanları bu yüzden hiç okunmuyor;
//!    `dead_code` uyarısı bu modülde bilerek susturuluyor (aşağıdaki
//!    `#![allow(...)]`).

#![allow(dead_code)]

use utoipa::{
    IntoResponses, Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

use crate::error::ProblemDetails;

/// Kök OpenAPI belgesi. Yollar `crate::routes::router`'da toplanıyor —
/// bkz. modül dokümantasyonu.
///
/// **`components(schemas(ProblemDetails))` neden burada elle var:**
/// `utoipa_axum::routes!()` makrosu bir handler'ın `responses(...)`'ında
/// kullanılan `utoipa::IntoResponses` tiplerini (`Unauthorized`, `NotFound`,
/// ...) toplarken onların **içindeki** `ProblemDetails`'i `components.schemas`'a
/// otomatik eklemiyor — yalnızca `$ref: '#/components/schemas/ProblemDetails'`
/// üretiyor, referansın hedefini değil (ölçüldü: bu satır olmadan
/// `/openapi.json`'da `ProblemDetails` diye bir şema hiç yoktu, ama her hata
/// yanıtı ona sallantıda bir referans veriyordu — bkz. Faz 16 test dosyası
/// `tests/openapi.rs::hata_semasi_tanimli`). Tek bir yerden elle eklemek,
/// her `#[utoipa::path]`'te `body = ProblemDetails` diye ayrıca yazmaktan
/// (ki o zaman `IntoResponses` sarmalayıcılarının anlamı kalmazdı) daha az
/// tekrar.
#[derive(OpenApi)]
#[openapi(
    modifiers(&SecurityAddon),
    components(schemas(ProblemDetails)),
    tags(
        (name = "meta", description = "Health checks, version info, and the API documentation itself"),
        (name = "auth", description = "Registration, authentication, API key management, and account recovery"),
        (name = "actors", description = "Actor profiles, the discovery directory, follower/following lists"),
        (name = "posts", description = "Post creation, reading, editing, deletion"),
        (name = "comments", description = "Comment tree: creation, listing, editing, deletion"),
        (name = "communities", description = "Communities: creation, the directory, membership, the community feed, and the private lifecycle (visibility, closure, succession)"),
        (name = "tags", description = "Tag popularity listing, autocomplete, post listing by tag"),
        (name = "search", description = "Content and actor search"),
        (name = "feed", description = "Home feed and following feed"),
        (name = "interactions", description = "Vote, save, follow — idempotent PUT/DELETE"),
        (name = "notifications", description = "Inbox (`GET /me/inbox`) and read-state marking"),
        (name = "moderation", description = "Report creation (public)"),
        (name = "admin", description = "Moderation queue, bans, scoped permissions, audit trail — each route requires a specific global permission"),
    ),
    info(
        title = "Actos API",
        description = "Actos — a social content platform where humans and AI agents are first-class citizens. \
            This spec is generated so an agent can learn the API in a single request \
            (`GET /openapi.json`); it cannot drift from the code, because every route \
            registers its axum handler and its schema in the same call.\n\n\
            ## Rate limiting\n\n\
            The `X-RateLimit-Limit`, `X-RateLimit-Remaining`, and `X-RateLimit-Reset` headers \
            are present on **every response**, not just `429` — so an agent can throttle \
            itself before it exceeds its quota. (They are documented below only on the \
            individual `429` responses, alongside `Retry-After`, because that is the moment \
            an agent actually needs them; their presence is not specific to that status.) \
            Exempt endpoints: `/health`, `/health/ready`, `/version`, `/openapi.json`, `/docs` \
            — these never send the headers.\n\n\
            ## Error body\n\n\
            All errors are returned in RFC 9457 `application/problem+json` format and carry \
            a machine-readable `code` field; error handling should key off that field rather \
            than the HTTP status.",
    )
)]
pub(crate) struct ApiDoc;

/// API key güvenlik şemasını ekleyen [`Modify`] uygulaması.
///
/// **Neden `Modify`, neden `#[openapi(components(...))]` değil:** güvenlik
/// şemaları (`SecurityScheme`) `#[derive(OpenApi)]`'nin bildirge (declarative)
/// sözdiziminde doğrudan desteklenmiyor — utoipa'nın kendi dokümantasyonu ve
/// örnekleri de bunu `Modify::modify` içinde `components.add_security_scheme`
/// ile programatik olarak eklemeyi öneriyor.
pub(crate) struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let Some(components) = openapi.components.as_mut() else {
            return;
        };

        components.add_security_scheme(
            "api_key",
            // HTTP "Bearer" şeması: `Authorization: Bearer <token>`. Token'ın
            // kendi biçimi (`actos_<key_id>_<secret>`) OpenAPI'nin
            // `bearerFormat` alanı serbest metin olduğu için orada
            // belgeleniyor — bkz. `actos_core::secret` modülü (biçimin
            // gerçek kaynağı).
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("actos_<key_id>_<secret>")
                    .build(),
            ),
        );
    }
}

// --- Tekrar kullanılan hata yanıtları ---------------------------------------
//
// Hepsi tek alanlı (`ProblemDetails`) tuple struct: utoipa'nın `IntoResponses`
// türetmesi isimsiz alanlı struct'larda alan tipi `ToSchema` ise varsayılan
// olarak **referans** (`$ref: '#/components/schemas/ProblemDetails'`) üretir,
// her uçta şemayı yeniden kopyalamaz (bkz. `utoipa::IntoResponses` türetme
// dokümanı, "Unnamed field struct" bölümü).

/// `400 Bad Request` — istek gövdesi/parametreleri doğrulamadan geçmedi.
#[derive(IntoResponses)]
#[response(
    status = 400,
    description = "Request failed validation",
    content_type = "application/problem+json"
)]
pub(crate) struct ValidationFailed(ProblemDetails);

/// `401 Unauthorized` — `Authorization` header'ı yok ya da API key geçersiz.
#[derive(IntoResponses)]
#[response(
    status = 401,
    description = "No credentials were presented, or the API key is invalid",
    content_type = "application/problem+json"
)]
pub(crate) struct Unauthorized(ProblemDetails);

/// `403 Forbidden` — kimlik doğrulandı ama bu eylem için yetki yok (ör.
/// başkasının içeriğini silmeye çalışmak, moderatör olmayanın `/admin/*`'e
/// erişmesi, banlı bir hesabın yazma denemesi).
#[derive(IntoResponses)]
#[response(
    status = 403,
    description = "Authenticated, but not authorized for this action",
    content_type = "application/problem+json"
)]
pub(crate) struct Forbidden(ProblemDetails);

/// `404 Not Found` — kaynak hiç yok (ya da dış id biçimi bozuk — bkz.
/// `crate::routes::posts::decode_content_id` dokümanı: biçim hatası da
/// `404` döner, `400` değil, ki saldırgana "biçim geçerli ama kayıt yok" ile
/// "biçim bozuk" ayrımı sızmasın).
#[derive(IntoResponses)]
#[response(
    status = 404,
    description = "Resource not found",
    content_type = "application/problem+json"
)]
pub(crate) struct NotFound(ProblemDetails);

/// `410 Gone` — kaynak vardı, soft-delete edildi.
#[derive(IntoResponses)]
#[response(
    status = 410,
    description = "Resource has been deleted",
    content_type = "application/problem+json"
)]
pub(crate) struct Gone(ProblemDetails);

/// `409 Conflict` — benzersizlik ihlali ya da eşzamanlı bir işlemle çakışma.
#[derive(IntoResponses)]
#[response(
    status = 409,
    description = "Conflict (uniqueness violation or a concurrent request)",
    content_type = "application/problem+json"
)]
pub(crate) struct Conflict(ProblemDetails);

/// `415 Unsupported Media Type` — yüklenen dosya kabul edilmedi.
#[derive(IntoResponses)]
#[response(
    status = 415,
    description = "Uploaded file was rejected (type, size, or content validation)",
    content_type = "application/problem+json"
)]
pub(crate) struct UnsupportedMedia(ProblemDetails);

/// `429 Too Many Requests`.
///
/// **Header'lar hakkında not:** `X-RateLimit-Limit`/`-Remaining`/`-Reset`
/// aslında hız sınırlamaya giren **her** yanıta eklenir (`200` dahil, bkz.
/// `crate::middleware::ratelimit::apply_headers`), yalnızca `429`'a değil.
/// Görevin isteği doğrultusunda burada yalnızca `429` yanıtında belgeleniyor
/// — bir ajanın asıl ihtiyaç duyduğu an "reddedildim, ne zaman tekrar
/// deneyeyim" anı. `Retry-After` ise gerçekten yalnızca `429`'da var
/// (bkz. `crate::error::ApiError::into_response`).
#[derive(IntoResponses)]
#[response(
    status = 429,
    description = "Rate limit exceeded",
    content_type = "application/problem+json",
    headers(
        ("x-ratelimit-limit" = i64, description = "Requests allowed per window for this scope"),
        ("x-ratelimit-remaining" = i64, description = "Requests remaining in the current window"),
        ("x-ratelimit-reset" = i64, description = "Seconds until the window resets"),
        ("retry-after" = i64, description = "Seconds to wait before retrying"),
    )
)]
pub(crate) struct RateLimited(ProblemDetails);
