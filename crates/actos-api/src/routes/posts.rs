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
/// `"[silindi]"` metni istemciye "bu yazarın profili artık yok" sinyalini
/// tek bakışta veriyor; ayrıca `ContentSummary.author_deleted` aynı bilgiyi
/// programatik olarak da taşıyor, istemcinin string'i ayrıştırmasına gerek
/// yok.
///
/// `id` **maskelenmiyor**: zaten opak (Feistel permütasyonlu, bkz.
/// `actos_core::id` modül dokümantasyonu) bir string, gerçek `bigint`'i
/// sızdırmıyor — aynı deleted actor'ün yazdığı birden fazla postu bu id
/// üzerinden birbirine bağlamak (ör. moderasyonda) hâlâ mümkün olsun diye
/// korunuyor. `actor_type`/`created_at` de aynı gerekçeyle (hassas değil)
/// korunuyor.
fn masked_actor_summary(actor: &ActorRecord, id_codec: &IdCodec) -> Result<ActorSummary, Error> {
    Ok(ActorSummary {
        id: encode_actor_id(id_codec, actor.id)?,
        username: "[silindi]".to_owned(),
        actor_type: actor_type_str(actor.actor_type).to_owned(),
        display_name: None,
        bio: None,
        created_at: actor.created_at.to_rfc3339(),
    })
}

/// Bir [`core_content::Content`]'i HTTP yanıt DTO'suna çevirir.
///
/// **Savunmacı tasarım:** `content.deleted_at.is_some()` burada da
/// kontrol ediliyor (yalnızca çağıranların — bkz. `get_post`/`update_post`/
/// `create_post` — zaten hep canlı içerik vermesine güvenmek yerine),
/// çünkü [`ContentSummary`] genel bir tip ve ileride (Faz 9/12) silinmiş
/// bir öğeyi listede satır içinde `[silindi]` göstermek isteyen bir
/// çağıran bu fonksiyonu doğrudan kullanabilsin diye (bkz.
/// `actos_types::content` modül dokümantasyonu).
pub(crate) fn content_summary(
    content: &core_content::Content,
    id_codec: &IdCodec,
) -> Result<ContentSummary, Error> {
    content_summary_with(content, id_codec, None)
}

/// [`content_summary`]'nin ekleri de dolduran hâli.
///
/// `ekler` `None` ise DTO'daki alan da `None` kalır — "yüklenmedi" ile
/// "yok" ayrımı için bkz. `actos_types::content::ContentSummary`.
pub(crate) fn content_summary_with(
    content: &core_content::Content,
    id_codec: &IdCodec,
    ekler: Option<(&[actos_core::attachment::Attachment], &actos_core::Storage)>,
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
    content_summary_inner(content, id_codec, attachments)
}

fn content_summary_inner(
    content: &core_content::Content,
    id_codec: &IdCodec,
    attachments: Option<Vec<actos_types::upload::UploadResponse>>,
) -> Result<ContentSummary, Error> {
    let id = id_codec.encode::<ContentIdKind>(content.id)?;

    let author = if content.author_deleted {
        masked_actor_summary(&content.author, id_codec)?
    } else {
        actor_summary(&content.author, id_codec)?
    };

    let deleted = content.deleted_at.is_some();
    // `title`/`body` ile aynı gerekçe: `metadata` de gerçek gövdenin bir
    // parçası — bir link post'unun URL önizlemesi gibi veri taşıyabilir.
    // Silinmiş bir içerik `[silindi]` gövdesiyle görünürken `metadata`'yı
    // olduğu gibi bırakmak, maskelemeyi yarım bırakan bir yan kanal olurdu.
    let (title, body, metadata) = if deleted {
        (
            None,
            "[silindi]".to_owned(),
            serde_json::Value::Object(serde_json::Map::new()),
        )
    } else {
        (
            content.title.clone(),
            content.body.clone(),
            content.metadata.clone(),
        )
    };

    Ok(ContentSummary {
        id,
        content_type: content_type_str(content.content_type).to_owned(),
        author,
        author_deleted: content.author_deleted,
        title,
        body,
        body_format: body_format_str(content.body_format).to_owned(),
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
    summary = "Yeni bir post oluştur",
    description = "`Idempotency-Key` header'ı verilirse aynı actor + aynı key ile daha önce \
        tamamlanmış bir istek varsa yeni bir post oluşturmadan **aynı** yanıt aynen döner.",
    security(("api_key" = [])),
    params(
        ("idempotency-key" = Option<String>, Header,
            description = "Verilirse tekrarlanan istekler aynı yanıtı üretir (bkz. üstteki açıklama)"),
    ),
    request_body = CreatePostRequest,
    responses(
        (status = 201, description = "Post oluşturuldu", body = ContentSummary,
            headers(("location" = String, description = "Yeni post'un yolu: /posts/{id}"))),
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
                    "aynı Idempotency-Key ile bir istek hâlâ işleniyor".to_owned(),
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
    summary = "Tekil bir post oku",
    params(
        ("id" = String, Path, description = "Post'un dış id'si (`c_...`)"),
        ("fields" = Option<String>, Query,
            description = "Virgülle ayrılmış alan adları — yalnızca bunlar döner (bkz. `crate::fields`). \
                Örn. `fields=id,title,score`."),
    ),
    responses(
        (status = 200, description = "Post (varsayılan: tüm alanlar, `?fields=` ile daraltılabilir)", body = ContentSummary),
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

    let summary = content_summary_with(&content, state.id_codec(), Some((&ekler, state.storage())))
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
    summary = "Bir post'u düzenle",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Post'un dış id'si (`c_...`)"),
    ),
    request_body = UpdatePostRequest,
    responses(
        (status = 200, description = "Güncellenmiş post", body = ContentSummary),
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
    summary = "Bir post'u sil (soft-delete)",
    description = "Sahibi ya da moderatör/admin çağırabilir.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Post'un dış id'si (`c_...`)"),
    ),
    responses(
        (status = 204, description = "Silindi"),
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
    summary = "Bir actor'ün post'larını listele",
    description = "En yeni post önce. Silinmiş post'lar listede görünmez.",
    params(
        ("username" = String, Path, description = "Actor'ün kullanıcı adı"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı"),
        ("fields" = Option<String>, Query,
            description = "Virgülle ayrılmış alan adları; her post öğesine uygulanır, sarmalayıcıya değil"),
    ),
    responses(
        (status = 200, description = "Post listesi, cursor'lu", body = PostListResponse),
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

    let posts = page
        .items
        .iter()
        .map(|content| {
            let summary = content_summary(content, state.id_codec())
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
