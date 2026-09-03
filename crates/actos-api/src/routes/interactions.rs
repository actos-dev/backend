//! Oy, takip ve kaydetme rotaları.
//!
//! Üçü de **idempotent** `PUT`/`DELETE` (bkz. `actos_core::interaction`
//! modül dokümantasyonu): aynı isteği iki kez göndermek yeni bir şey
//! yaratmıyor, sayaçları kaydırmıyor.
//!
//! `content_summary`/`decode_content_id` `crate::routes::posts`'tan
//! `pub(crate)` alınıyor.

use std::collections::BTreeMap;

use actos_core::{
    Error,
    id::{Content as ContentIdKind, IdCodec},
    interaction as core_interaction,
};
use actos_types::interaction::{SaveListResponse, VoteMapResponse, VoteRequest, VoteResponse};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::Value;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::CurrentActor,
    error::ApiError,
    fields,
    openapi::{Forbidden, Gone, NotFound, RateLimited, Unauthorized, ValidationFailed},
    routes::actors::{decode_cursor, parse_limit},
    routes::posts::{content_summary_with_optional_body_html, decode_content_id},
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(set_vote))
        .routes(routes!(save, unsave))
        .routes(routes!(follow, unfollow))
        .routes(routes!(list_saves))
        .routes(routes!(list_votes))
}

/// `GET /me/saves?cursor=&limit=&fields=` query'si.
#[derive(Debug, Deserialize)]
struct SavesQuery {
    cursor: Option<String>,
    limit: Option<String>,
    fields: Option<String>,
}

/// `GET /me/votes?content_ids=c_a,c_b` query'si.
#[derive(Debug, Deserialize)]
struct VotesQuery {
    content_ids: Option<String>,
}

/// Tek istekte sorulabilecek azami içerik sayısı.
///
/// Bir feed sayfası en fazla `MAX_PAGE_SIZE` (100) öğe döndürdüğü için
/// istemcinin bundan fazlasını tek seferde sorması için sebep yok; sınır
/// hem sorguyu hem URL uzunluğunu makul tutuyor.
const MAX_VOTE_LOOKUP: usize = 100;

// --- Handler'lar -----------------------------------------------------------

/// `PUT /contents/{id}/vote` → `200`, `400` (geçersiz değer), `403` (kendi
/// içeriği), `404`, `410`.
#[utoipa::path(
    put,
    path = "/contents/{id}/vote",
    tag = "interactions",
    summary = "Vote on a piece of content (or retract your vote)",
    description = "Idempotent. `value`: `1` (up), `-1` (down), `0` (retract vote). You cannot vote on your own content.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The content's external id (`c_...`, post or comment)"),
    ),
    request_body = VoteRequest,
    responses(
        (status = 200, description = "The content's counters after the operation", body = VoteResponse),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn set_vote(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<VoteRequest>,
) -> Result<Json<VoteResponse>, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec(), "content")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let sonuc = core_interaction::set_vote(state.db(), current.actor.id, content_id, req.value)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(VoteResponse {
        value: sonuc.value,
        score: sonuc.score,
        upvotes: sonuc.upvotes,
        downvotes: sonuc.downvotes,
    }))
}

/// `GET /me/votes?content_ids=` → `200`.
///
/// Çözülemeyen bir id **sessizce atlanıyor**, hata üretmiyor: bu bir toplu
/// arama ucu, tek bozuk id yüzünden bütün sayfanın oy durumunu kaybetmek
/// istemciye zarar verir. Zaten yanıtta olmayan id "oy yok" demek.
#[utoipa::path(
    get,
    path = "/me/votes",
    tag = "interactions",
    summary = "Bulk-query your own votes on the given content items",
    description = "An id that can't be resolved, or has no vote, is silently skipped — its absence from the response means \"no vote\".",
    security(("api_key" = [])),
    params(
        ("content_ids" = Option<String>, Query,
            description = "Comma-separated external content ids (max 100, see MAX_VOTE_LOOKUP)"),
    ),
    responses(
        (status = 200, description = "Map of id -> vote value (voted items only)", body = VoteMapResponse),
        ValidationFailed,
        Unauthorized,
        RateLimited,
    )
)]
async fn list_votes(
    current: CurrentActor,
    State(state): State<AppState>,
    Query(query): Query<VotesQuery>,
    headers: HeaderMap,
) -> Result<Json<VoteMapResponse>, ApiError> {
    let Some(raw) = query.content_ids else {
        return Ok(Json(VoteMapResponse {
            votes: BTreeMap::new(),
        }));
    };

    let dis_idler: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if dis_idler.len() > MAX_VOTE_LOOKUP {
        return Err(ApiError::new(Error::Validation(format!(
            "at most {MAX_VOTE_LOOKUP} content items can be queried per request"
        )))
        .with_request_id(&headers));
    }

    let ic_idler: Vec<i64> = dis_idler
        .iter()
        .filter_map(|s| state.id_codec().decode::<ContentIdKind>(s).ok())
        .collect();

    let oylar = core_interaction::votes_for(state.db(), current.actor.id, &ic_idler)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let votes = oylar
        .into_iter()
        .map(|(id, value)| encode_content_id(state.id_codec(), id).map(|dis| (dis, value)))
        .collect::<Result<BTreeMap<String, i16>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(VoteMapResponse { votes }))
}

/// İç `bigint` içerik id'sini dış id'ye çevirir.
///
/// `IdError` → `actos_core::Error` dönüşümü burada yapılıyor ki çağıran
/// tek bir hata tipiyle çalışsın (`crate::routes::posts`'taki
/// `encode_actor_id` ile aynı desen).
fn encode_content_id(id_codec: &IdCodec, id: i64) -> Result<String, Error> {
    Ok(id_codec.encode::<ContentIdKind>(id)?)
}

/// `PUT /contents/{id}/save` → `204`, `404`, `410`. İdempotent.
#[utoipa::path(
    put,
    path = "/contents/{id}/save",
    tag = "interactions",
    summary = "Add a piece of content to your saved list",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The content's external id (`c_...`)"),
    ),
    responses(
        (status = 204, description = "Saved (same result if already saved)"),
        Unauthorized,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn save(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec(), "content")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    core_interaction::save(state.db(), current.actor.id, content_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /contents/{id}/save` → `204`. İdempotent; silinmiş içeriğin
/// kaydı da kaldırılabilir (bkz. `actos_core::interaction::unsave`).
#[utoipa::path(
    delete,
    path = "/contents/{id}/save",
    tag = "interactions",
    summary = "Remove a piece of content from your saved list",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The content's external id (`c_...`)"),
    ),
    responses(
        (status = 204, description = "Removed (same result if not saved)"),
        Unauthorized,
        RateLimited,
    )
)]
async fn unsave(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let content_id = decode_content_id(&id, state.id_codec(), "content")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    core_interaction::unsave(state.db(), current.actor.id, content_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /actors/{username}/follow` → `204`, `400` (kendini takip), `404`,
/// `410`. İdempotent.
#[utoipa::path(
    put,
    path = "/actors/{username}/follow",
    tag = "interactions",
    summary = "Follow an actor",
    security(("api_key" = [])),
    params(
        ("username" = String, Path, description = "Username of the actor to follow"),
    ),
    responses(
        (status = 204, description = "Followed (same result if already following)"),
        ValidationFailed,
        Unauthorized,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn follow(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    core_interaction::follow(state.db(), current.actor.id, &username)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /actors/{username}/follow` → `204`, `404`. İdempotent; silinmiş
/// hesap da takipten çıkarılabilir.
#[utoipa::path(
    delete,
    path = "/actors/{username}/follow",
    tag = "interactions",
    summary = "Unfollow an actor",
    security(("api_key" = [])),
    params(
        ("username" = String, Path, description = "Username of the actor to unfollow"),
    ),
    responses(
        (status = 204, description = "Unfollowed (same result if not following)"),
        Unauthorized,
        NotFound,
        RateLimited,
    )
)]
async fn unfollow(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    core_interaction::unfollow(state.db(), current.actor.id, &username)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /me/saves` → `200` (cursor'lu, en son kaydedilen önce).
///
/// Yanıt gövdesi `actos_types::interaction::SaveListResponse` şeklinde ama
/// `Json<Value>` olarak elle kuruluyor: `?fields=` yalnızca dizideki
/// öğelere uygulanıyor, sarmalayıcıya değil (bkz.
/// `crate::routes::posts::list_actor_posts`'taki aynı desen).
#[utoipa::path(
    get,
    path = "/me/saves",
    tag = "interactions",
    summary = "List what you've saved",
    description = "Most recently saved first. Posts and comments can be mixed together.",
    security(("api_key" = [])),
    params(
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
        ("fields" = Option<String>, Query,
            description = "Comma-separated field names; applied to each item"),
    ),
    responses(
        (status = 200, description = "Saved item list, with a cursor", body = SaveListResponse),
        Unauthorized,
        RateLimited,
    )
)]
async fn list_saves(
    current: CurrentActor,
    State(state): State<AppState>,
    Query(query): Query<SavesQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_interaction::list_saves(state.db(), current.actor.id, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let selected_fields = fields::parse_fields(query.fields.as_deref());
    let include_body_html = fields::wants_body_html(selected_fields.as_deref());

    let saves = page
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
        "saves": saves,
        "next_cursor": next_cursor,
    })))
}
