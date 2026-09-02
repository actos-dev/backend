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
    routes::posts::content_summary,
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(feed))
        .routes(routes!(following_feed))
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
#[utoipa::path(
    get,
    path = "/feed",
    tag = "feed",
    summary = "Ana akış",
    description = "Kimlik gerekmez. amac.txt'teki \"GET posts/mainpage\" senaryosu.",
    params(
        ("sort" = Option<String>, Query, description = "`hot`, `new` ya da `top`"),
        ("window" = Option<String>, Query, description = "`top` sıralaması için zaman penceresi (`day`, `week`, `month`, `all`)"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı"),
        ("fields" = Option<String>, Query,
            description = "Virgülle ayrılmış alan adları; her post öğesine uygulanır"),
    ),
    responses(
        (status = 200, description = "Post listesi, cursor'lu", body = PostListResponse),
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
    summary = "Takip akışı",
    description = "Yalnızca takip edilen actor'ların post'ları. Hiç kimseyi takip etmiyorsan boş liste döner.",
    security(("api_key" = [])),
    params(
        ("sort" = Option<String>, Query, description = "`hot`, `new` ya da `top`"),
        ("window" = Option<String>, Query, description = "`top` sıralaması için zaman penceresi"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı"),
        ("fields" = Option<String>, Query,
            description = "Virgülle ayrılmış alan adları; her post öğesine uygulanır"),
    ),
    responses(
        (status = 200, description = "Post listesi, cursor'lu", body = PostListResponse),
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
