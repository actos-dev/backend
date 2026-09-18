//! Post rotaları: oluşturma, tekil okuma, düzenleme, silme.
//!
//! `crate::routes::actors`'taki gibi burada da iş mantığı yok — her handler
//! `actos_core::content`'in bir fonksiyonunu çağırır, sonucu HTTP'ye çevirir.
//! `actor_summary`/`encode_actor_id` gibi ortak dönüşümler burada tekrar
//! yazılmıyor, `crate::routes::auth`'tan (`pub(crate)`) alınıyor.
//!
//! **Yorum uçları burada değil**, `crate::routes::comments`'ta — yolları
//! `/posts/{id}/comments` ile başlasa da (bkz. o modülün dokümantasyonu).
//! Bu dosyanın `content_summary`/`decode_content_id` yardımcıları oradan
//! `pub(crate)` olarak paylaşılıyor: aynı `ContentSummary` dönüşümünün iki
//! kopyası olmamalı.

use actos_core::{
    Error,
    auth::ActorRecord,
    content as core_content,
    id::{Community as CommunityIdKind, Content as ContentIdKind, IdCodec},
    idempotency::{Begin as IdempotencyBegin, StoredResponse},
};
use actos_types::{
    auth::ActorSummary,
    content::{
        CommunityRefSummary, ContentSummary, CreatePostRequest, PostListResponse, UpdatePostRequest,
    },
};
use axum::{
    Json,
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::Value;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::CurrentActor,
    error::ApiError,
    fields,
    openapi::{Conflict, Forbidden, Gone, NotFound, RateLimited, Unauthorized, ValidationFailed},
    routes::actors::{decode_cursor, parse_limit},
    routes::auth::{actor_summary, actor_type_str, encode_actor_id},
    state::AppState,
};

/// `Idempotency-Key` header'ının adı. `HeaderMap::get` zaten büyük/küçük
/// harf duyarsız (bkz. `http` crate'i) — burada sabit bir `&str` olarak
/// tutmak yalnızca yazım hatasını tek bir yere hapsetmek için.
const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

/// `DefaultBodyLimit` override for `POST /posts`/`POST /posts/{id}/comments`
/// (see [`crate::routes::comments`], which reuses this same constant).
///
/// Follows the pattern `crate::routes::actors::router` established for the
/// avatar upload route (see `crate::app::build`'s doc for why a route-level
/// `DefaultBodyLimit` layer is needed at all — the app-wide layer only sets
/// a default, it does not cap what an inner layer can raise it to). Unlike
/// the avatar route this is a fixed constant, not threaded from
/// `max_upload_bytes`: it has to cover UP TO
/// [`actos_core::attachment::MAX_ATTACHMENTS_PER_CONTENT`] files at once,
/// each up to `max_upload_bytes`, plus the JSON `payload` part and
/// multipart boundary/header overhead. 4 × 8 MiB (the default
/// `max_upload_bytes`) is already 32 MiB, so 34 MiB leaves 2 MiB of
/// headroom for everything else in the body.
pub(crate) const CREATE_CONTENT_BODY_LIMIT: usize = 34 * 1024 * 1024;

/// The multipart field carrying the same JSON body the `application/json`
/// case would carry.
const PAYLOAD_FIELD: &str = "payload";

/// The multipart field carrying an image file — repeated for each file, up
/// to [`actos_core::attachment::MAX_ATTACHMENTS_PER_CONTENT`].
const FILES_FIELD: &str = "files";

/// Reads either an `application/json` body or a `multipart/form-data` body
/// (a `payload` JSON part plus zero or more `files` parts) into `(T, files)`
/// — the shared implementation behind `POST /posts` and
/// `POST /posts/{id}/comments`'s dual content-type acceptance (REFACTOR.md
/// §4: "images travel with the post or comment that carries them, or they
/// are not sent at all" — there is no third, upload-then-attach path).
///
/// **Why this dispatches by hand instead of two separate extractors:** axum
/// picks an extractor at compile time from the handler's signature: it
/// can't itself branch on `Content-Type` between `Json<T>` and `Multipart`.
/// Taking the raw [`Request`] as the last argument and manually running
/// `Json::from_request`/`Multipart::from_request` on it (both still go
/// through axum's own extractor code, including its `DefaultBodyLimit`
/// enforcement) is the standard way to do content-type-conditional
/// extraction in axum.
///
/// A missing/unrecognized `Content-Type` is treated as the JSON case —
/// matching a plain `Json<T>` extractor's own behavior today, so a client
/// that never sends images sees no change at all.
pub(crate) async fn extract_content_payload<T>(
    request: Request,
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(T, Vec<Vec<u8>>), ApiError>
where
    T: serde::de::DeserializeOwned,
{
    let is_multipart = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("multipart/form-data"));

    if !is_multipart {
        let Json(payload) = Json::<T>::from_request(request, state).await.map_err(|e| {
            ApiError::new(Error::Validation(format!("invalid JSON body: {e}")))
                .with_request_id(headers)
        })?;
        return Ok((payload, Vec::new()));
    }

    let mut multipart = Multipart::from_request(request, state).await.map_err(|e| {
        ApiError::new(Error::Validation(format!(
            "could not read multipart body: {e}"
        )))
        .with_request_id(headers)
    })?;

    let mut payload: Option<T> = None;
    let mut files = Vec::new();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::new(Error::Validation(format!("could not read multipart: {e}")))
            .with_request_id(headers)
    })? {
        // Unknown field names are ignored, not rejected — the same policy
        // the old `POST /uploads` handler used (client libraries routinely
        // send extra fields).
        match field.name() {
            Some(PAYLOAD_FIELD) => {
                let bytes = field.bytes().await.map_err(|e| {
                    ApiError::new(Error::Validation(format!(
                        "could not read \"{PAYLOAD_FIELD}\" field: {e}"
                    )))
                    .with_request_id(headers)
                })?;
                let parsed = serde_json::from_slice(&bytes).map_err(|e| {
                    ApiError::new(Error::Validation(format!(
                        "invalid JSON in \"{PAYLOAD_FIELD}\" field: {e}"
                    )))
                    .with_request_id(headers)
                })?;
                payload = Some(parsed);
            }
            Some(FILES_FIELD) => {
                let bytes = field.bytes().await.map_err(|e| {
                    ApiError::new(Error::Validation(format!(
                        "could not read \"{FILES_FIELD}\" field: {e}"
                    )))
                    .with_request_id(headers)
                })?;
                files.push(bytes.to_vec());
            }
            _ => {}
        }
    }

    let payload = payload.ok_or_else(|| {
        ApiError::new(Error::Validation(format!(
            "multipart body is missing the \"{PAYLOAD_FIELD}\" field"
        )))
        .with_request_id(headers)
    })?;

    Ok((payload, files))
}

pub fn router() -> OpenApiRouter<AppState> {
    let create = OpenApiRouter::new()
        .routes(routes!(create_post))
        .layer(DefaultBodyLimit::max(CREATE_CONTENT_BODY_LIMIT));

    OpenApiRouter::new()
        .merge(create)
        .routes(routes!(get_post, update_post, delete_post))
        // Faz 7'den devir (bkz. PLAN.md Faz 8): `actos_core::content`'in
        // fonksiyonunu çağırdığı ve `content_summary`/`fields` gibi bu
        // dosyaya özel ortak dönüşümleri paylaştığı için mantıksal olarak
        // burada — path'in `/actors/...` ile başlaması `actos-api`'de
        // dosya/router ayrımını değiştirmiyor (bkz. `crate::routes::mod`
        // dokümantasyonu, tüm alt router'lar tek bir ağaçta `merge` edilir).
        .routes(routes!(list_actor_posts))
}

// --- Query param tipleri -------------------------------------------------

/// `GET /posts/{id}?fields=...` query'si.
#[derive(Debug, Deserialize)]
struct PostFieldsQuery {
    fields: Option<String>,
}

/// `GET /actors/{username}/posts?cursor=...&limit=...&fields=...` query'si.
///
/// `crate::routes::actors::PageQuery`'den farkı yalnızca `fields` alanı —
/// ayrı bir struct olmasının sebebi bu (paylaşılan bir `PageQuery` + ayrı
/// bir `fields` extractor'ı iki ayrı `Query<T>` extraction'ı gerektirirdi,
/// axum bunu tek bir extractor'da birleştirmeyi kolaylaştırmıyor).
#[derive(Debug, Deserialize)]
struct ActorPostsQuery {
    cursor: Option<String>,
    limit: Option<String>,
    fields: Option<String>,
}

// --- Ortak dönüşümler --------------------------------------------------

const fn content_type_str(t: core_content::ContentType) -> &'static str {
    match t {
        core_content::ContentType::Post => "post",
        core_content::ContentType::Comment => "comment",
    }
}

const fn body_format_str(f: core_content::BodyFormat) -> &'static str {
    match f {
        core_content::BodyFormat::Markdown => "markdown",
        core_content::BodyFormat::Plain => "plain",
    }
}

/// Silinmiş bir yazarın `ActorSummary`'sini maskeler.
///
/// **Karar: `username` de maskelenir, yalnızca `display_name`/`bio` değil.**
/// `actos_core::actor::get_profile` dokümanındaki gerekçeyle aynı kaynak:
/// Faz 7'de `actors.username` soft-delete'te serbest bırakılmıyor (bkz.
/// `migrations/0002_actors.up.sql` COMMENT) — yani gerçek username'i burada
/// göstermek tek başına bir impersonation riski taşımaz (isim kimseye
/// yeniden verilemez). Yine de bilinçli olarak maskeliyoruz, çünkü asıl
/// soru impersonation değil **tutarlılık**: bu username için
/// `GET /actors/{username}` zaten `410 Gone` dönüyor (profile hiç
/// ulaşılamaz durumda) — içerik yanıtında aynı username'i sanki hâlâ
/// gidilebilir bir profilmiş gibi göstermek yanıltıcı olurdu. Sabit
/// `"[deleted]"` metni istemciye "bu yazarın profili artık yok" sinyalini
/// tek bakışta veriyor; ayrıca `ContentSummary.author_deleted` aynı bilgiyi
/// programatik olarak da taşıyor, istemcinin string'i ayrıştırmasına gerek
/// yok.
///
/// `id` **maskelenmiyor**: zaten opak (Feistel permütasyonlu, bkz.
/// `actos_core::id` modül dokümantasyonu) bir string, gerçek `bigint`'i
/// sızdırmıyor — aynı deleted actor'ün yazdığı birden fazla postu bu id
/// üzerinden birbirine bağlamak (ör. moderasyonda) hâlâ mümkün olsun diye
/// korunuyor. `actor_type`/`created_at` too are kept for the same reason
/// (not sensitive).
pub(crate) fn masked_actor_summary(
    actor: &ActorRecord,
    id_codec: &IdCodec,
) -> Result<ActorSummary, Error> {
    Ok(ActorSummary {
        id: encode_actor_id(id_codec, actor.id)?,
        username: "[deleted]".to_owned(),
        actor_type: actor_type_str(actor.actor_type).to_owned(),
        display_name: None,
        bio: None,
        created_at: actor.created_at.to_rfc3339(),
        // Bilerek her zaman `None` — `display_name`/`bio` gibi girdiden
        // türetilmiyor: silinmiş bir hesabın avatarı da diğer kişisel
        // alanlar gibi görünmeye devam etmemeli (bkz. `actos_types::
        // auth::ActorSummary::avatar_url` dokümanındaki gerekçe). Bu satır,
        // içerik yazarları için avatar zaten hiç taşınmadığından (aşağıdaki
        // `content_summary_inner`'daki `None` — bkz. o çağrıdaki not)
        // bugün pratikte hep-zaten-`None`'ı maskeliyor; yine de sabit
        // `None` bırakıldı ki ileride içerik yazarlarına avatar eklenirse
        // (bkz. o notta anlatılan kapsam dışı bırakma) maskeleme
        // otomatik/garantili kalsın, "unutma"ya bağlı olmasın.
        avatar_url: None,
    })
}

/// Bir [`core_content::Content`]'i HTTP yanıt DTO'suna çevirir.
///
/// `body_html` hesaplanmıyor (`None` kalır) — bu kısa yol yalnızca
/// `?fields=body_html` desteklemeyen ya da desteklese de talep edilmediği
/// yollar için (bkz. [`content_summary_with_body_html`]).
///
/// **Savunmacı tasarım:** `content.deleted_at.is_some()` burada da
/// kontrol ediliyor (yalnızca çağıranların — bkz. `get_post`/`update_post`/
/// `create_post` — zaten hep canlı içerik vermesine güvenmek yerine),
/// çünkü [`ContentSummary`] genel bir tip ve ileride (Faz 9/12) silinmiş
/// bir öğeyi listede satır içinde `[deleted]` göstermek isteyen bir
/// çağıran bu fonksiyonu doğrudan kullanabilsin diye (bkz.
/// `actos_types::content` modül dokümantasyonu).
pub(crate) fn content_summary(
    content: &core_content::Content,
    id_codec: &IdCodec,
) -> Result<ContentSummary, Error> {
    content_summary_full(content, id_codec, None, false)
}

/// [`content_summary`]'nin ekleri de dolduran hâli.
///
/// `ekler` `None` ise DTO'daki alan da `None` kalır — "yüklenmedi" ile
/// "yok" ayrımı için bkz. `actos_types::content::ContentSummary`.
///
/// `body_html` burada da hesaplanmıyor; tekil uçlar (`GET /posts/{id}`,
/// `GET /comments/{id}`) [`content_summary_with_body_html`]'i kullanıyor.
pub(crate) fn content_summary_with(
    content: &core_content::Content,
    id_codec: &IdCodec,
    ekler: Option<(&[actos_core::attachment::Attachment], &actos_core::Storage)>,
) -> Result<ContentSummary, Error> {
    content_summary_full(content, id_codec, ekler, false)
}

/// [`content_summary_with`]'in `body_html`'i her zaman hesaplayan hâli.
///
/// Yalnızca tekil-öğe uçları (`GET /posts/{id}`, `GET /comments/{id}`)
/// çağırır — bkz. `actos_types::content::ContentSummary::body_html`
/// dokümanı "Nerede dolu döner" gerekçesi. Liste uçları bunun yerine
/// [`content_summary_with_optional_body_html`]'i kullanır: orada hesaplama
/// yalnızca istemci `?fields=body_html` ile açıkça istediyse yapılır.
pub(crate) fn content_summary_with_body_html(
    content: &core_content::Content,
    id_codec: &IdCodec,
    ekler: Option<(&[actos_core::attachment::Attachment], &actos_core::Storage)>,
) -> Result<ContentSummary, Error> {
    content_summary_full(content, id_codec, ekler, true)
}

/// Liste öğeleri için: `body_html` yalnızca `include_body_html` `true` ise
/// hesaplanır. Çağıran bunu `selected_fields`'in `"body_html"` içerip
/// içermediğine bakarak belirler (bkz. `crate::routes::posts::
/// list_actor_posts`).
pub(crate) fn content_summary_with_optional_body_html(
    content: &core_content::Content,
    id_codec: &IdCodec,
    include_body_html: bool,
) -> Result<ContentSummary, Error> {
    content_summary_full(content, id_codec, None, include_body_html)
}

fn content_summary_full(
    content: &core_content::Content,
    id_codec: &IdCodec,
    ekler: Option<(&[actos_core::attachment::Attachment], &actos_core::Storage)>,
    include_body_html: bool,
) -> Result<ContentSummary, Error> {
    let attachments = match ekler {
        None => None,
        Some((liste, storage)) => Some(
            liste
                .iter()
                .map(|ek| attachment_response(ek, id_codec, storage))
                .collect::<Result<Vec<_>, Error>>()?,
        ),
    };
    content_summary_inner(content, id_codec, attachments, include_body_html)
}

/// Bir [`actos_core::attachment::Attachment`]'i yanıt DTO'suna çevirir.
///
/// This used to live in the now-deleted `crate::routes::uploads` (the
/// standalone `POST /uploads` handler built the exact same DTO for its own
/// `201` response) — that route is gone (REFACTOR.md §4), so the only
/// remaining caller is [`content_summary_full`], which shows an
/// attachment's shape nested inside `ContentSummary.attachments`.
fn attachment_response(
    ek: &actos_core::attachment::Attachment,
    id_codec: &IdCodec,
    storage: &actos_core::Storage,
) -> Result<actos_types::upload::UploadResponse, Error> {
    Ok(actos_types::upload::UploadResponse {
        id: id_codec.encode::<actos_core::id::Attachment>(ek.id)?,
        url: storage.public_url(&ek.object_key),
        thumbnail_url: storage.public_url(&ek.thumbnail_key()),
        mime_type: ek.mime_type.clone(),
        byte_size: ek.byte_size,
        width: ek.width,
        height: ek.height,
        checksum_sha256: ek.checksum_sha256.clone(),
        created_at: ek.created_at.to_rfc3339(),
    })
}

/// `body` + `body_format`'tan sanitize edilmiş HTML üretir (bkz.
/// `actos_types::content::ContentSummary::body_html` dokümanı).
///
/// **`plain` içerikte markdown render EDİLMEZ.** `actos_core::text::
/// render_markdown` yalnızca `body_format == "markdown"` iken çağrılıyor;
/// `plain` için tek yapılan HTML özel karakterlerini escape edip tek bir
/// `<p>` ile sarmak — kullanıcının düz metin niyetiyle yazdığı `*yıldız*`
/// gibi bir gövdeyi markdown sözdizimi sanıp italikleştirmemek için.
///
/// Silinmiş içerik için ayrı bir dal YOK: `body` çağıran tarafından zaten
/// maskelenmiş (`"[deleted]"`) olarak geliyor (bkz.
/// `content_summary_inner`) — bu fonksiyon her zaman aynı iki yoldan
/// birini işlettiği için `body_html`'in `body` ile birebir aynı maskeleme
/// kuralına tabi olması otomatik garanti ediliyor, özel bir "silinmişse"
/// kontrolüne gerek kalmıyor.
fn render_body_html(body: &str, body_format: &str) -> String {
    if body_format == "plain" {
        escape_plain_body_html(body)
    } else {
        actos_core::text::render_markdown(body)
    }
}

/// `render_body_html`'in `plain` dalı: HTML özel karakterlerini escape
/// eder ve tek bir `<p>` ile sarar. Markdown render'ının aksine burada
/// `pulldown-cmark`/`ammonia` hiç devrede değil — yalnızca beş özel
/// karakterin (`&`, `<`, `>`, `"`, `'`) mekanik değişimi, bu yüzden
/// `ammonia`'yı `actos-api`'ye bağımlılık olarak eklemeye gerek yok.
fn escape_plain_body_html(body: &str) -> String {
    let mut escaped = String::with_capacity(body.len());
    for ch in body.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    format!("<p>{escaped}</p>")
}

fn content_summary_inner(
    content: &core_content::Content,
    id_codec: &IdCodec,
    attachments: Option<Vec<actos_types::upload::UploadResponse>>,
    include_body_html: bool,
) -> Result<ContentSummary, Error> {
    let id = id_codec.encode::<ContentIdKind>(content.id)?;

    // `avatar_url` burada bilerek her zaman `None`: `content.author`
    // (`ActorRecord`) avatar taşımıyor — bkz. `actos_core::auth::
    // AuthenticatedActor` dokümanındaki gerekçe (`ActorRecord`,
    // `crate::comment`/`crate::interaction`/`crate::feed`/`crate::search`
    // gibi avatarı hiç bilmeyen birçok sorgu tarafından da paylaşılıyor).
    // Yani bir içeriğin yazarının avatarını göstermek bu görevin kapsamı
    // dışında bırakıldı; profil odaklı uçlar (`GET /actors/{username}`,
    // `GET /auth/whoami`, takipçi/takip/keşif listeleri) avatarı doğru
    // taşıyor.
    let author = if content.author_deleted {
        masked_actor_summary(&content.author, id_codec)?
    } else {
        actor_summary(&content.author, None, id_codec)?
    };

    let deleted = content.deleted_at.is_some();
    let (title, body) = if deleted {
        (None, "[deleted]".to_owned())
    } else {
        (content.title.clone(), content.body.clone())
    };

    let body_format = body_format_str(content.body_format).to_owned();
    // `body`'den türetiliyor (`deleted` iken zaten yukarıda maskelenmiş) —
    // bkz. `render_body_html` dokümanı: ayrı bir "silinmişse" dalına gerek
    // yok, maskeleme otomatik devrediyor.
    let body_html = include_body_html.then(|| render_body_html(&body, &body_format));

    // Topluluk referansı: bağımsız bir post için `None` (yani `null`),
    // topluluk postu için kodlanmış id + ad. `community.name` burada ham
    // gerçek: ad topluluğun kendi kaynağından geldiği için maskelenecek
    // kişisel veri değil.
    let community = match &content.community {
        None => None,
        Some(reference) => Some(CommunityRefSummary {
            id: id_codec
                .encode::<CommunityIdKind>(reference.id)
                .map_err(|e| Error::Internal(format!("could not encode community id: {e}")))?,
            name: reference.name.clone(),
        }),
    };

    Ok(ContentSummary {
        id,
        content_type: content_type_str(content.content_type).to_owned(),
        author,
        author_deleted: content.author_deleted,
        community,
        title,
        body,
        body_format,
        body_html,
        tags: content.tags.clone(),
        score: content.score,
        upvotes: content.upvotes,
        downvotes: content.downvotes,
        comment_count: content.comment_count,
        created_at: content.created_at.to_rfc3339(),
        edited_at: content.edited_at.map(|t| t.to_rfc3339()),
        attachments,
        deleted,
    })
}

/// Ham `{id}` path segmentini iç `bigint`'e çözer.
///
/// Ayrıştırılamıyorsa (yanlış prefix, bozuk base62, ...) [`Error::NotFound`]
/// dönülür, [`Error::Validation`] değil — `crate::routes::auth::revoke_key`
/// dokümanındaki gerekçeyle aynı: "bu biçim geçerli ama böyle bir kayıt yok"
/// ile "biçim bozuk" ayrımı saldırgana bilgi verirdi.
pub(crate) fn decode_content_id(
    raw: &str,
    id_codec: &IdCodec,
    kind: &'static str,
) -> Result<i64, Error> {
    id_codec
        .decode::<ContentIdKind>(raw)
        .map_err(|_| Error::NotFound(kind))
}

// --- Handler'lar -----------------------------------------------------------

/// A documentation-only schema for the `multipart/form-data` alternative to
/// [`CreatePostRequest`] — never instantiated; the real parsing is
/// [`extract_content_payload`]. Exists only so the OpenAPI spec can
/// describe the multipart shape (same documentation-only role as
/// `crate::routes::actors::AvatarRequestBody`).
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
struct CreatePostMultipartBody {
    /// The same JSON body the `application/json` case would carry, as a
    /// single multipart part.
    payload: CreatePostRequest,
    /// Up to `MAX_ATTACHMENTS_PER_CONTENT` (4) image files. Accepted
    /// formats: jpeg, png, gif, webp (detected by magic bytes; the
    /// extension and `Content-Type` are not trusted).
    #[schema(content_media_type = "application/octet-stream")]
    files: Vec<Vec<u8>>,
}

/// Bir [`StoredResponse`]'u (daha önce tamamlanmış bir idempotent isteğin
/// saklanan sonucu) aynı HTTP yanıtına çevirir — istemci bunu ilk isteğin
/// yanıtından ayırt edemez (bkz. `actos_core::idempotency` modül
/// dokümantasyonu: "aynı `201`, aynı gövde, aynı `Location`").
fn stored_response_to_http(stored: StoredResponse) -> Response {
    let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
    let mut response = (status, Json(stored.body)).into_response();
    if let Some(location) = &stored.location
        && let Ok(value) = HeaderValue::from_str(location)
    {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

/// `POST /posts` → `201` + `Location: /posts/{id}`.
///
/// **`Idempotency-Key` desteği** (bkz. PLAN.md Faz 8, "buglu ajanlar için
/// hayat kurtarıcı"): header verilmişse, aynı actor + aynı key ile daha
/// önce tamamlanmış bir istek varsa yeni bir post oluşturmadan **aynı**
/// yanıtı aynen döner; başka bir istek aynı anda işleniyorsa `409`
/// (bkz. `actos_core::idempotency::Begin::InProgress` dokümanı — kararın
/// gerekçesi burada değil orada, Redis'e dokunan katmanda). Header hiç
/// verilmemişse davranış birinci turdakiyle birebir aynı — bu bütünüyle
/// isteğe bağlı bir katman, zorunlu değil.
#[utoipa::path(
    post,
    path = "/posts",
    tag = "posts",
    summary = "Create a new post",
    description = "If the `Idempotency-Key` header is given and a request with the same actor + \
        same key has already completed, the **same** response is returned as-is without creating a new post. \
        Accepts EITHER `application/json` (no images) OR `multipart/form-data` (the same JSON as a `payload` \
        part, plus up to 4 `files` parts).",
    security(("api_key" = [])),
    params(
        ("idempotency-key" = Option<String>, Header,
            description = "If given, repeated requests produce the same response (see the description above)"),
    ),
    request_body(
        content(
            (CreatePostRequest = "application/json"),
            (inline(CreatePostMultipartBody) = "multipart/form-data"),
        )
    ),
    responses(
        (status = 201, description = "Post created", body = ContentSummary,
            headers(("location" = String, description = "Path of the new post: /posts/{id}"))),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        Conflict,
        RateLimited,
    )
)]
async fn create_post(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, ApiError> {
    let (req, files): (CreatePostRequest, Vec<Vec<u8>>) =
        extract_content_payload(request, &state, &headers).await?;

    let idempotency_key = headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    if let Some(key) = &idempotency_key {
        match state
            .idempotency()
            .begin(current.actor.id, key)
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?
        {
            IdempotencyBegin::Completed(stored) => {
                return Ok(stored_response_to_http(stored));
            }
            IdempotencyBegin::InProgress => {
                return Err(ApiError::new(Error::Conflict(
                    "a request with the same Idempotency-Key is still being processed".to_owned(),
                ))
                .with_request_id(&headers));
            }
            // Yer tutucu bizim koyduğumuz taze bir kayıt — isteği normal
            // şekilde işleyip aşağıda `complete` ile sonucu yazıyoruz.
            IdempotencyBegin::Start => {}
        }
    }

    let content = core_content::create_post(
        state.db(),
        state.storage(),
        state.id_codec(),
        &current.actor,
        req.community.as_deref(),
        &req.title,
        &req.body,
        &req.tags,
        &files,
        state.config().server.max_upload_bytes,
        state.config().storage_quota.bytes,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let ekler = actos_core::attachment::list_for_content(state.db(), content.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let summary = content_summary_with(&content, state.id_codec(), Some((&ekler, state.storage())))
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let location = format!("/posts/{}", summary.id);

    if let Some(key) = &idempotency_key {
        // Gövdeyi `StoredResponse` için de aynı struct'tan (`summary`)
        // üretiyoruz — istemciye giden yanıtla saklanan yanıtın birbirinden
        // sapması (ör. burada bir alan eklenip orada unutulması) yapısal
        // olarak imkânsız, ikisi de tek bir serialize'dan geliyor.
        if let Ok(body) = serde_json::to_value(&summary) {
            state
                .idempotency()
                .complete(
                    current.actor.id,
                    key,
                    &StoredResponse {
                        status: StatusCode::CREATED.as_u16(),
                        location: Some(location.clone()),
                        body,
                    },
                )
                .await;
        }
    }

    let mut response = (StatusCode::CREATED, Json(summary)).into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

/// `GET /posts/{id}` → `200` (canlı), `410` (silinmiş), `404` (yok/bozuk id).
#[utoipa::path(
    get,
    path = "/posts/{id}",
    tag = "posts",
    summary = "Read a single post",
    params(
        ("id" = String, Path, description = "The post's external id (`c_...`)"),
        ("fields" = Option<String>, Query,
            description = "Comma-separated field names — only these are returned. \
                E.g. `fields=id,title,score`."),
    ),
    responses(
        (status = 200, description = "Post (default: all fields, narrowed with `?fields=`)", body = ContentSummary),
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn get_post(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PostFieldsQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec(), "post")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let content = core_content::get_post(state.db(), content_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let ekler = actos_core::attachment::list_for_content(state.db(), content_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    // Tekil uç: `body_html` `?fields=`'ten bağımsız her zaman hesaplanır
    // (bkz. `actos_types::content::ContentSummary::body_html` "Nerede dolu
    // döner"); `?fields=` yalnızca sonraki `apply_fields` adımında hangi
    // anahtarların yanıta gireceğini daraltır.
    let summary =
        content_summary_with_body_html(&content, state.id_codec(), Some((&ekler, state.storage())))
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let selected_fields = fields::parse_fields(query.fields.as_deref());
    let json = fields::apply_fields(&summary, selected_fields.as_deref(), &headers)?;

    Ok(Json(json))
}

/// `PATCH /posts/{id}` → `200`. Sahibi değilse `403`, yoksa `404`, silinmişse
/// `410`.
#[utoipa::path(
    patch,
    path = "/posts/{id}",
    tag = "posts",
    summary = "Edit a post",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The post's external id (`c_...`)"),
    ),
    request_body = UpdatePostRequest,
    responses(
        (status = 200, description = "Updated post", body = ContentSummary),
        Unauthorized,
        Forbidden,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn update_post(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdatePostRequest>,
) -> Result<Json<ContentSummary>, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec(), "post")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let content = core_content::update_post(
        state.db(),
        content_id,
        current.actor.id,
        req.title,
        req.body,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let summary = content_summary(&content, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(summary))
}

/// `DELETE /posts/{id}` → `204`. Sahibi veya moderatör/admin; yetkisiz
/// biri için `403`, yoksa `404`, zaten silinmişse `410`.
#[utoipa::path(
    delete,
    path = "/posts/{id}",
    tag = "posts",
    summary = "Delete a post (soft-delete)",
    description = "Callable by its owner or a moderator/admin.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The post's external id (`c_...`)"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        Unauthorized,
        Forbidden,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn delete_post(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec(), "post")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    core_content::delete_post(
        state.db(),
        content_id,
        current.actor.id,
        &current.permissions,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /actors/{username}/posts` → `200` (cursor'lu, en yeni post önce),
/// `410` (actor silinmiş), `404` (böyle bir username hiç yok). Silinmiş
/// postlar listede görünmez — bkz. `actos_core::content::
/// list_posts_by_actor` dokümanı.
///
/// Yanıt gövdesi `actos_types::content::PostListResponse`'un şekliyle
/// (`{"posts": [...], "next_cursor": ...}`) aynı, ama `Json<Value>` olarak
/// elle kuruluyor: `?fields=` yalnızca `posts` dizisindeki her öğeye
/// uygulanıyor, sarmalayıcıya değil (bkz. `crate::fields` modül
/// dokümantasyonu) — bu da öğe başına ayrı bir `apply_fields` çağrısı
/// gerektiriyor, tek bir `Json<PostListResponse>` ile ifade edilemez.
#[utoipa::path(
    get,
    path = "/actors/{username}/posts",
    tag = "posts",
    summary = "List an actor's posts",
    description = "Newest post first. Deleted posts don't appear in the list.",
    params(
        ("username" = String, Path, description = "The actor's username"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
        ("fields" = Option<String>, Query,
            description = "Comma-separated field names; applied to each post item, not the envelope"),
    ),
    responses(
        (status = 200, description = "Post list, with a cursor", body = PostListResponse),
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn list_actor_posts(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Query(query): Query<ActorPostsQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_content::list_posts_by_actor(state.db(), &username, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let selected_fields = fields::parse_fields(query.fields.as_deref());
    let include_body_html = fields::wants_body_html(selected_fields.as_deref());

    let posts = page
        .items
        .iter()
        .map(|content| {
            let summary = content_summary_with_optional_body_html(
                content,
                state.id_codec(),
                include_body_html,
            )
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;
            fields::apply_fields(&summary, selected_fields.as_deref(), &headers)
        })
        .collect::<Result<Vec<Value>, ApiError>>()?;

    let next_cursor = page.next_cursor.map(|c| state.cursor_codec().encode(&c));

    Ok(Json(serde_json::json!({
        "posts": posts,
        "next_cursor": next_cursor,
    })))
}
