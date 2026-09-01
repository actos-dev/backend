//! Post rotaları: oluşturma, tekil okuma, düzenleme, silme.
//!
//! `crate::routes::actors`'taki gibi burada da iş mantığı yok — her handler
//! `actos_core::content`'in bir fonksiyonunu çağırır, sonucu HTTP'ye çevirir.
//! `actor_summary`/`encode_actor_id` gibi ortak dönüşümler burada tekrar
//! yazılmıyor, `crate::routes::auth`'tan (`pub(crate)`) alınıyor.
//!
//! **Yorumlar burada yok** — `POST /posts/{id}/comments`, `GET
//! /posts/{id}/comments` vb. Faz 9'un konusu (bkz. `actos_core::content`
//! modül dokümantasyonu).

use actos_core::{
    Error,
    auth::ActorRecord,
    content as core_content,
    id::{Content as ContentIdKind, IdCodec},
};
use actos_types::{
    auth::ActorSummary,
    content::{ContentSummary, CreatePostRequest, UpdatePostRequest},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use crate::{
    auth::CurrentActor,
    error::ApiError,
    routes::auth::{actor_summary, actor_type_str, encode_actor_id},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/posts", post(create_post)).route(
        "/posts/{id}",
        get(get_post).patch(update_post).delete(delete_post),
    )
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
fn content_summary(
    content: &core_content::Content,
    id_codec: &IdCodec,
) -> Result<ContentSummary, Error> {
    let id = id_codec.encode::<ContentIdKind>(content.id)?;

    let author = if content.author_deleted {
        masked_actor_summary(&content.author, id_codec)?
    } else {
        actor_summary(&content.author, id_codec)?
    };

    let deleted = content.deleted_at.is_some();
    let (title, body) = if deleted {
        (None, "[silindi]".to_owned())
    } else {
        (content.title.clone(), content.body.clone())
    };

    Ok(ContentSummary {
        id,
        content_type: content_type_str(content.content_type).to_owned(),
        author,
        author_deleted: content.author_deleted,
        title,
        body,
        body_format: body_format_str(content.body_format).to_owned(),
        tags: content.tags.clone(),
        score: content.score,
        upvotes: content.upvotes,
        downvotes: content.downvotes,
        comment_count: content.comment_count,
        created_at: content.created_at.to_rfc3339(),
        edited_at: content.edited_at.map(|t| t.to_rfc3339()),
        deleted,
    })
}

/// Ham `{id}` path segmentini iç `bigint`'e çözer.
///
/// Ayrıştırılamıyorsa (yanlış prefix, bozuk base62, ...) [`Error::NotFound`]
/// dönülür, [`Error::Validation`] değil — `crate::routes::auth::revoke_key`
/// dokümanındaki gerekçeyle aynı: "bu biçim geçerli ama böyle bir kayıt yok"
/// ile "biçim bozuk" ayrımı saldırgana bilgi verirdi.
fn decode_content_id(raw: &str, id_codec: &IdCodec) -> Result<i64, Error> {
    id_codec
        .decode::<ContentIdKind>(raw)
        .map_err(|_| Error::NotFound("post"))
}

// --- Handler'lar -----------------------------------------------------------

/// `POST /posts` → `201` + `Location: /posts/{id}`.
async fn create_post(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreatePostRequest>,
) -> Result<Response, ApiError> {
    let content = core_content::create_post(
        state.db(),
        &current.actor,
        &req.title,
        &req.body,
        &req.tags,
        req.metadata,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let summary = content_summary(&content, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let location = format!("/posts/{}", summary.id);
    let mut response = (StatusCode::CREATED, Json(summary)).into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

/// `GET /posts/{id}` → `200` (canlı), `410` (silinmiş), `404` (yok/bozuk id).
async fn get_post(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ContentSummary>, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let content = core_content::get_post(state.db(), content_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let summary = content_summary(&content, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(summary))
}

/// `PATCH /posts/{id}` → `200`. Sahibi değilse `403`, yoksa `404`, silinmişse
/// `410`.
async fn update_post(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdatePostRequest>,
) -> Result<Json<ContentSummary>, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec())
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
async fn delete_post(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    core_content::delete_post(state.db(), content_id, current.actor.id, &current.roles)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}
