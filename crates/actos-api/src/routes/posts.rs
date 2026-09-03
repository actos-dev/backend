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
    id::{Content as ContentIdKind, IdCodec},
    idempotency::{Begin as IdempotencyBegin, StoredResponse},
};
use actos_types::{
    auth::ActorSummary,
    content::{ContentSummary, CreatePostRequest, PostListResponse, UpdatePostRequest},
};
use axum::{
    Json,
    extract::{Path, Query, State},
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

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_post))
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
/// korunuyor. `actor_type`/`created_at`/`trust_level` de aynı gerekçeyle
/// (hassas değil) korunuyor.
fn masked_actor_summary(actor: &ActorRecord, id_codec: &IdCodec) -> Result<ActorSummary, Error> {
    Ok(ActorSummary {
        id: encode_actor_id(id_codec, actor.id)?,
        username: "[deleted]".to_owned(),
        actor_type: actor_type_str(actor.actor_type).to_owned(),
        display_name: None,
        bio: None,
        created_at: actor.created_at.to_rfc3339(),
        trust_level: actor.trust_level,
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
                .map(|ek| crate::routes::uploads::upload_response(ek, id_codec, storage))
                .collect::<Result<Vec<_>, Error>>()?,
        ),
    };
    content_summary_inner(content, id_codec, attachments, include_body_html)
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
    // `title`/`body` ile aynı gerekçe: `metadata` de gerçek gövdenin bir
    // parçası — bir link post'unun URL önizlemesi gibi veri taşıyabilir.
    // Silinmiş bir içerik `[deleted]` gövdesiyle görünürken `metadata`'yı
    // olduğu gibi bırakmak, maskelemeyi yarım bırakan bir yan kanal olurdu.
    let (title, body, metadata) = if deleted {
        (
            None,
            "[deleted]".to_owned(),
            serde_json::Value::Object(serde_json::Map::new()),
        )
    } else {
        (
            content.title.clone(),
            content.body.clone(),
            content.metadata.clone(),
        )
    };

    let body_format = body_format_str(content.body_format).to_owned();
    // `body`'den türetiliyor (`deleted` iken zaten yukarıda maskelenmiş) —
    // bkz. `render_body_html` dokümanı: ayrı bir "silinmişse" dalına gerek
    // yok, maskeleme otomatik devrediyor.
    let body_html = include_body_html.then(|| render_body_html(&body, &body_format));

    Ok(ContentSummary {
        id,
        content_type: content_type_str(content.content_type).to_owned(),
        author,
        author_deleted: content.author_deleted,
        title,
        body,
        body_format,
        body_html,
        metadata,
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

/// Ek dış id'lerini iç `bigint`'lere çözer.
///
/// Bozuk bir id burada **hata üretiyor**, sessizce atlanmıyor
/// (`GET /me/votes`'un toplu aramasının aksine): istemci bir ek göndermek
/// istediğini açıkça söylüyor, onu sessizce düşürmek gönderdiğinden farklı
/// bir post yaratmak olurdu.
///
/// `pub(crate)`: `crate::routes::comments` de aynı çözümü kullanıyor.
pub(crate) fn decode_attachment_ids(
    raw: Option<&[String]>,
    id_codec: &IdCodec,
) -> Result<Vec<i64>, Error> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.iter()
        .map(|s| {
            id_codec
                .decode::<actos_core::id::Attachment>(s)
                .map_err(|_| Error::NotFound("attachment"))
        })
        .collect()
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
        same key has already completed, the **same** response is returned as-is without creating a new post.",
    security(("api_key" = [])),
    params(
        ("idempotency-key" = Option<String>, Header,
            description = "If given, repeated requests produce the same response (see the description above)"),
    ),
    request_body = CreatePostRequest,
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
    Json(req): Json<CreatePostRequest>,
) -> Result<Response, ApiError> {
    let attachment_ids = decode_attachment_ids(req.attachment_ids.as_deref(), state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

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
        &current.actor,
        &req.title,
        &req.body,
        &req.tags,
        req.metadata,
        &attachment_ids,
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
            description = "Comma-separated field names — only these are returned (see `crate::fields`). \
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

    core_content::delete_post(state.db(), content_id, current.actor.id, &current.roles)
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
