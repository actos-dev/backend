//! `GET /search` ucu: içerik (post/yorum) ve actor araması.
//!
//! İş mantığı burada yok — `actos_core::search`'ün bir fonksiyonunu çağırıp
//! sonucu HTTP'ye çevirir (bkz. `crate::routes::posts`/`crate::routes::tags`
//! ile aynı katman ayrımı).
//!
//! **Tek uç, iki yanıt şekli:** `?type=post`/`?type=comment` içerik
//! (`ContentSummary`) döner, `?type=actor` actor (`ActorSummary`) döner —
//! bkz. `actos_types::search` modül dokümantasyonu "İki ayrı yanıt şekli"
//! bölümü. Bu yüzden yanıt burada `Json<ContentSearchResponse>` gibi tek
//! bir tipe bağlanmıyor, `crate::routes::tags::list_tag_posts` deseniyle
//! aynı şekilde elle `Json<Value>` kuruluyor — `?fields=` filtresi her iki
//! şekilde de `results` dizisindeki öğelere uygulanıyor, sarmalayıcıya değil
//! (bkz. `crate::fields` modül dokümantasyonu).

use actos_core::{
    Error,
    content::ContentType,
    search::{self as core_search, SearchTarget},
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    error::ApiError,
    fields,
    routes::actors::{decode_cursor_with, parse_limit},
    routes::auth::actor_summary,
    routes::posts::content_summary,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/search", get(search))
}

/// `GET /search?q=&type=&cursor=&limit=&fields=` query'si.
#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    #[serde(rename = "type")]
    target: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
    fields: Option<String>,
}

/// `GET /search` → `200`. `type` zorunlu; verilmemiş ya da tanınmayan bir
/// değerse `400` (bkz. [`SearchTarget::parse`] — `content::PostSort::parse`
/// deseniyle aynı, sessiz bir varsayılana düşülmüyor).
///
/// `q` verilmemişse (ya da normalize edildikten sonra boşsa —
/// `actos_core::search` bunu zaten kendi içinde ele alıyor) boş bir sonuç
/// listesi döner, hata değil — `crate::routes::tags::search_tags` ile aynı
/// karar (bkz. o handler'ın dokümanı): bir arama kutusu henüz yazılırken
/// `400` göstermemeli.
async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = SearchTarget::parse(query.target.as_deref())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor_with(
        state.cursor_codec(),
        query.cursor.as_deref(),
        actos_core::cursor::SortKind::Hot,
        &headers,
    )?;
    let selected_fields = fields::parse_fields(query.fields.as_deref());

    let Some(q) = query.q else {
        return Ok(Json(json!({ "results": [], "next_cursor": Value::Null })));
    };

    // `Post`/`Comment` ikisi de `actos_core::content::ContentType`'a
    // karşılık gelirken `Actor` bambaşka bir tabloyu (`search_actors`)
    // hedef alıyor — bu eşlemeyi burada, tek bir yerde yapıp aşağıda
    // `if let` ile dallanıyoruz (bkz. `SearchTarget::Actor => None`).
    let content_type = match target {
        SearchTarget::Post => Some(ContentType::Post),
        SearchTarget::Comment => Some(ContentType::Comment),
        SearchTarget::Actor => None,
    };

    if let Some(content_type) = content_type {
        let page = core_search::search_content(state.db(), &q, content_type, cursor, limit)
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

        let results = page
            .items
            .iter()
            .map(|content| {
                let summary = content_summary(content, state.id_codec())
                    .map_err(|e: Error| ApiError::new(e).with_request_id(&headers))?;
                fields::apply_fields(&summary, selected_fields.as_deref(), &headers)
            })
            .collect::<Result<Vec<Value>, ApiError>>()?;

        let next_cursor = page.next_cursor.map(|c| state.cursor_codec().encode(&c));

        return Ok(Json(
            json!({ "results": results, "next_cursor": next_cursor }),
        ));
    }

    let page = core_search::search_actors(state.db(), &q, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let results = page
        .items
        .iter()
        .map(|actor| {
            let summary = actor_summary(actor, state.id_codec())
                .map_err(|e: Error| ApiError::new(e).with_request_id(&headers))?;
            fields::apply_fields(&summary, selected_fields.as_deref(), &headers)
        })
        .collect::<Result<Vec<Value>, ApiError>>()?;

    let next_cursor = page.next_cursor.map(|c| state.cursor_codec().encode(&c));

    Ok(Json(
        json!({ "results": results, "next_cursor": next_cursor }),
    ))
}
