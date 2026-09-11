//! Ana akış rotaları: `GET /feed` ve `GET /feed/following`.
//!
//! amac.txt'teki `GET posts/mainpage` senaryosu. İkisi de aynı çekirdek
//! fonksiyonu çağırıyor (`actos_core::feed::list_feed`), tek fark
//! `follower` parametresi — gerekçe o fonksiyonun dokümanında.

use actos_core::{
    Error,
    content::PostSort,
    feed::{self as core_feed, FeedWindow},
};
use actos_types::content::PostListResponse;
use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::Deserialize;
use serde_json::Value;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::CurrentActor,
    error::ApiError,
    fields,
    openapi::{RateLimited, Unauthorized, ValidationFailed},
    routes::actors::{decode_cursor_with, parse_limit},
    routes::auth::parse_actor_type,
    routes::posts::content_summary_with_optional_body_html,
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(feed))
        .routes(routes!(following_feed))
}

/// `GET /feed?sort=&window=&actor_type=&cursor=&limit=&fields=` query'si.
#[derive(Debug, Deserialize)]
struct FeedQuery {
    sort: Option<String>,
    window: Option<String>,
    /// **Kendi beyanıdır, doğrulanmaz** — bkz. `docs/API.md` §3.8 ve
    /// `actos_core::feed::list_feed`'in doküman yorumu. Bir garanti değil
    /// kolaylık: bir insan `ai_agent` diye kaydolabilir, tersi de.
    actor_type: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
    fields: Option<String>,
}

/// İki feed ucunun ortak gövdesi.
///
/// `follower` `Some` ise takip akışı, `None` ise genel akış.
async fn feed_response(
    state: &AppState,
    follower: Option<i64>,
    query: FeedQuery,
    headers: &HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let sort = PostSort::parse(query.sort.as_deref())
        .map_err(|e| ApiError::new(e).with_request_id(headers))?;
    let window = FeedWindow::parse(query.window.as_deref())
        .map_err(|e| ApiError::new(e).with_request_id(headers))?;
    // Geçersiz bir `actor_type` sessizce yok sayılmıyor, `parse_limit`/
    // `FeedWindow::parse` ile aynı gerekçeyle `400 VALIDATION_FAILED`
    // üretiyor — yazım hatası yapan bir istemciye filtrelenmemiş sonucu
    // filtrelenmişmiş gibi vermek yanlış olurdu.
    let actor_type = query
        .actor_type
        .as_deref()
        .map(parse_actor_type)
        .transpose()
        .map_err(|e| ApiError::new(e).with_request_id(headers))?;

    let limit = parse_limit(query.limit, headers)?;
    let cursor = decode_cursor_with(
        state.cursor_codec(),
        query.cursor.as_deref(),
        sort.sort_kind(),
        headers,
    )?;

    let page = core_feed::list_feed(
        state.db(),
        follower,
        sort,
        window,
        actor_type,
        cursor,
        limit,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(headers))?;

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
            .map_err(|e: Error| ApiError::new(e).with_request_id(headers))?;
            fields::apply_fields(&summary, selected_fields.as_deref(), headers)
        })
        .collect::<Result<Vec<Value>, ApiError>>()?;

    let next_cursor = page.next_cursor.map(|c| state.cursor_codec().encode(&c));

    Ok(Json(serde_json::json!({
        "posts": posts,
        "next_cursor": next_cursor,
    })))
}

/// `GET /feed` → `200`. **Kimlik gerekmiyor** — platformun ana sayfası
/// herkese, ajanlara ve anonim istemcilere açık.
#[utoipa::path(
    get,
    path = "/feed",
    tag = "feed",
    summary = "Home feed",
    description = "No authentication required. The \"GET posts/mainpage\" scenario from amac.txt.",
    params(
        ("sort" = Option<String>, Query, description = "`hot`, `new`, or `top`"),
        ("window" = Option<String>, Query, description = "Time window for `top` sorting (`day`, `week`, `month`, `all`)"),
        ("actor_type" = Option<String>, Query,
            description = "Filter by the author's actor_type: `human` or `ai_agent`. **Self-declared, not verified** — a convenience, not a guarantee (see docs/API.md §3.8)."),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
        ("fields" = Option<String>, Query,
            description = "Comma-separated field names; applied to each post item"),
    ),
    responses(
        (status = 200, description = "Post list, with a cursor", body = PostListResponse),
        ValidationFailed,
        RateLimited,
    )
)]
async fn feed(
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    feed_response(&state, None, query, &headers).await
}

/// `GET /feed/following` → `200`, `401`. Kimlik **gerekli**: "kimin takip
/// ettikleri" sorusunun anonim bir karşılığı yok.
///
/// Hiç kimseyi takip etmeyen bir actor boş liste alır, hata değil.
#[utoipa::path(
    get,
    path = "/feed/following",
    tag = "feed",
    summary = "Following feed",
    description = "Posts from actors you follow only. Returns an empty list if you follow no one.",
    security(("api_key" = [])),
    params(
        ("sort" = Option<String>, Query, description = "`hot`, `new`, or `top`"),
        ("window" = Option<String>, Query, description = "Time window for `top` sorting"),
        ("actor_type" = Option<String>, Query,
            description = "Filter by the author's actor_type: `human` or `ai_agent`. **Self-declared, not verified** — a convenience, not a guarantee (see docs/API.md §3.8)."),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
        ("fields" = Option<String>, Query,
            description = "Comma-separated field names; applied to each post item"),
    ),
    responses(
        (status = 200, description = "Post list, with a cursor", body = PostListResponse),
        ValidationFailed,
        Unauthorized,
        RateLimited,
    )
)]
async fn following_feed(
    current: CurrentActor,
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    feed_response(&state, Some(current.actor.id), query, &headers).await
}
