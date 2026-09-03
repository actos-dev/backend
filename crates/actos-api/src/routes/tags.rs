//! Etiket rotaları: popülerlik listesi, otomatik tamamlama ve etikete göre
//! post listesi.
//!
//! `GET /tags/{name}/posts` post döndürdüğü hâlde burada, `posts.rs`'te
//! değil: `crate::routes::comments`'ta açıklanan ölçütle aynı — dosya
//! ayrımı "hangi kavramın uçları" sorusuna göre, yolun nasıl başladığına
//! göre değil. Bu uç bir etiketin içeriğini gezmenin yolu, `posts.rs`
//! ise post kaynağının kendi CRUD'u.
//!
//! `content_summary` `crate::routes::posts`'tan `pub(crate)` alınıyor.

use actos_core::{
    Error,
    content::{self as core_content, PostSort},
    tag as core_tag,
};
use actos_types::content::PostListResponse;
use actos_types::tag::{TagListResponse, TagMatch, TagSearchResponse, TagSummary};
use axum::http::HeaderMap;
use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::Deserialize;
use serde_json::Value;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    error::ApiError,
    fields,
    openapi::{NotFound, RateLimited, ValidationFailed},
    routes::actors::{decode_cursor_with, parse_limit},
    routes::posts::content_summary_with_optional_body_html,
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        // `/tags/search` ile `/tags/{name}/posts` çakışmıyor: biri iki,
        // diğeri üç segment. Yine de `search` üstte duruyor ki ileride
        // `/tags/{name}` eklenirse sıralama şimdiden doğru olsun.
        .routes(routes!(search_tags))
        .routes(routes!(list_tags))
        .routes(routes!(list_tag_posts))
}

// --- Query param tipleri ---------------------------------------------------

/// `GET /tags?cursor=&limit=` query'si.
#[derive(Debug, Deserialize)]
struct TagListQuery {
    cursor: Option<String>,
    limit: Option<String>,
}

/// `GET /tags/search?q=` query'si.
#[derive(Debug, Deserialize)]
struct TagSearchQuery {
    q: Option<String>,
}

/// `GET /tags/{name}/posts?sort=&cursor=&limit=&fields=` query'si.
#[derive(Debug, Deserialize)]
struct TagPostsQuery {
    sort: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
    fields: Option<String>,
}

// --- Handler'lar -----------------------------------------------------------

/// `GET /tags` → `200`, en çok kullanılan etiketler önce, cursor'lu.
///
/// Popülerlik cursor'ı [`actos_core::cursor::SortKey::Top`] üzerinden
/// taşınıyor — orada "skor" olarak adlandırılan sayı burada post sayısı
/// (bkz. `actos_core::tag::list_popular`).
#[utoipa::path(
    get,
    path = "/tags",
    tag = "tags",
    summary = "List tags ordered by popularity",
    params(
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Tag list, with a cursor", body = TagListResponse),
        ValidationFailed,
        RateLimited,
    )
)]
async fn list_tags(
    State(state): State<AppState>,
    Query(query): Query<TagListQuery>,
    headers: HeaderMap,
) -> Result<Json<TagListResponse>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor_with(
        state.cursor_codec(),
        query.cursor.as_deref(),
        actos_core::cursor::SortKind::Top,
        &headers,
    )?;

    let page = core_tag::list_popular(state.db(), cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let tags = page
        .items
        .into_iter()
        .map(|t| TagSummary {
            name: t.name,
            post_count: t.post_count,
            created_at: t.created_at.to_rfc3339(),
        })
        .collect();

    let next_cursor = page.next_cursor.map(|c| state.cursor_codec().encode(&c));

    Ok(Json(TagListResponse { tags, next_cursor }))
}

/// `GET /tags/search?q=` → `200`, otomatik tamamlama.
///
/// `q` verilmezse ya da hiçbir etiketle eşleşemeyecek bir değerse **boş
/// liste** döner, hata değil (bkz. `actos_core::tag::search` — kullanıcı
/// henüz yazarken hata göstermek yanlış olurdu).
#[utoipa::path(
    get,
    path = "/tags/search",
    tag = "tags",
    summary = "Tag autocomplete",
    description = "Returns an empty list if `q` is omitted or nothing matches, not an error. No pagination.",
    params(
        ("q" = Option<String>, Query, description = "The tag prefix to search for"),
    ),
    responses(
        (status = 200, description = "Matching tags (capped at `actos_core::tag::SEARCH_LIMIT`)", body = TagSearchResponse),
        RateLimited,
    )
)]
async fn search_tags(
    State(state): State<AppState>,
    Query(query): Query<TagSearchQuery>,
    headers: HeaderMap,
) -> Result<Json<TagSearchResponse>, ApiError> {
    let Some(q) = query.q else {
        return Ok(Json(TagSearchResponse { tags: Vec::new() }));
    };

    let matches = core_tag::search(state.db(), &q)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(TagSearchResponse {
        tags: matches
            .into_iter()
            .map(|m| TagMatch { name: m.name })
            .collect(),
    }))
}

/// `GET /tags/{name}/posts` → `200` (cursor'lu), `404` (böyle bir etiket
/// yok). amac.txt'teki `GET posts/nvidia` senaryosu.
///
/// Var olup hiç canlı post'u kalmamış bir etiket boş liste döner, `404`
/// değil — "böyle bir etiket yok" ile "bu etikette şu an içerik yok"
/// istemci için farklı bilgiler.
///
/// `?fields=` destekleniyor; `crate::routes::posts::list_actor_posts` ile
/// aynı desen (filtre sarmalayıcıya değil, dizideki her öğeye uygulanıyor).
#[utoipa::path(
    get,
    path = "/tags/{name}/posts",
    tag = "tags",
    summary = "List a tag's posts",
    description = "A tag that exists but has no live posts left returns an empty list, not `404`.",
    params(
        ("name" = String, Path, description = "Tag name"),
        ("sort" = Option<String>, Query, description = "`new`, `top`, or `hot`"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
        ("fields" = Option<String>, Query,
            description = "Comma-separated field names; applied to each post item"),
    ),
    responses(
        (status = 200, description = "Post list, with a cursor", body = PostListResponse),
        ValidationFailed,
        NotFound,
        RateLimited,
    )
)]
async fn list_tag_posts(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<TagPostsQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let sort = PostSort::parse(query.sort.as_deref())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor_with(
        state.cursor_codec(),
        query.cursor.as_deref(),
        sort.sort_kind(),
        &headers,
    )?;

    let page = core_content::list_posts_by_tag(state.db(), &name, sort, cursor, limit)
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
            .map_err(|e: Error| ApiError::new(e).with_request_id(&headers))?;
            fields::apply_fields(&summary, selected_fields.as_deref(), &headers)
        })
        .collect::<Result<Vec<Value>, ApiError>>()?;

    let next_cursor = page.next_cursor.map(|c| state.cursor_codec().encode(&c));

    Ok(Json(serde_json::json!({
        "posts": posts,
        "next_cursor": next_cursor,
    })))
}
