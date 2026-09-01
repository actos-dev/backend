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
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    auth::CurrentActor,
    error::ApiError,
    fields,
    routes::actors::{decode_cursor_with, parse_limit},
    routes::posts::content_summary,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/feed", get(feed))
        .route("/feed/following", get(following_feed))
}

/// `GET /feed?sort=&window=&cursor=&limit=&fields=` query'si.
#[derive(Debug, Deserialize)]
struct FeedQuery {
    sort: Option<String>,
    window: Option<String>,
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

    let limit = parse_limit(query.limit, headers)?;
    let cursor = decode_cursor_with(
        state.cursor_codec(),
        query.cursor.as_deref(),
        sort.sort_kind(),
        headers,
    )?;

    let page = core_feed::list_feed(state.db(), follower, sort, window, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(headers))?;

    let selected_fields = fields::parse_fields(query.fields.as_deref());

    let posts = page
        .items
        .iter()
        .map(|content| {
            let summary = content_summary(content, state.id_codec())
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
async fn following_feed(
    current: CurrentActor,
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    feed_response(&state, Some(current.actor.id), query, &headers).await
}
