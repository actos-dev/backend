//! Yorum rotaları: oluşturma, iç içe ağaç listeleme, tekil okuma
//! (breadcrumb ile), düzenleme, silme.
//!
//! **Yolların bir kısmı `/posts/...` ile başlasa da bu dosyada:**
//! `POST /posts/{id}/comments` ve `GET /posts/{id}/comments` bir post'un
//! altında yaşıyor ama döndürdükleri şey yorum; `crate::routes::posts`'un
//! `GET /actors/{username}/posts`'u aynı sebeple orada duruyor (bkz. o
//! modülün router yorumu). Router ağacı zaten tek bir `merge` ile
//! birleştiği için dosya ayrımı yol biçimini hiç etkilemiyor — ölçüt
//! "hangi kavramın kodu" olmalı, "yol nasıl başlıyor" değil.
//!
//! `content_summary` ve `decode_content_id` burada yeniden yazılmıyor,
//! `crate::routes::posts`'tan `pub(crate)` olarak alınıyor: aynı
//! `ContentSummary` dönüşümünün iki kopyası zamanla birbirinden ayrılırdı.

use actos_core::{
    Error,
    comment::{self as core_comment, CommentSort},
    cursor::SortKind,
};
use actos_types::content::{
    CommentDetailResponse, CommentListResponse, CommentNodeResponse, CommentThreadResponse,
    ContentSummary, CreateCommentRequest, UpdateCommentRequest,
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
    openapi::{Forbidden, Gone, NotFound, RateLimited, Unauthorized, ValidationFailed},
    routes::actors::{decode_cursor, decode_cursor_with, parse_limit},
    routes::posts::{content_summary, decode_content_id},
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_comment, list_comments))
        .routes(routes!(get_comment, update_comment, delete_comment))
        // Faz 7'den devir (bkz. PLAN.md Faz 9).
        .routes(routes!(list_actor_comments))
}

// --- Query param tipleri ---------------------------------------------------

/// `GET /posts/{id}/comments?sort=&depth=&parent=&cursor=&limit=` query'si.
///
/// Sayısal alanlar `String` olarak alınıp elle ayrıştırılıyor: axum'un
/// `Query<T>` ile ürettiği tip hatası düz metin bir `400` döndürür, oysa bu
/// API'de her hata RFC 9457 `application/problem+json` olmalı (bkz.
/// `crate::routes::actors::parse_limit` üzerindeki aynı gerekçe).
#[derive(Debug, Deserialize)]
struct CommentTreeQuery {
    sort: Option<String>,
    depth: Option<String>,
    parent: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

/// `GET /actors/{username}/comments?cursor=&limit=&fields=` query'si.
#[derive(Debug, Deserialize)]
struct ActorCommentsQuery {
    cursor: Option<String>,
    limit: Option<String>,
    fields: Option<String>,
}

/// Ham `depth` query değerini çözer.
///
/// Verilmezse [`core_comment::DEFAULT_TREE_DEPTH`]. Negatif ya da çok büyük
/// değerler reddedilmiyor, `list_comment_tree` içinde aralığa sıkıştırılıyor
/// — `parse_limit`'teki kararla aynı: "her şeyi tek istekte al" deneyen bir
/// ajanı reddetmek yerine tavana çekmek daha yardımcı bir davranış. Yalnızca
/// *sayı olmayan* bir değer hata üretir.
fn parse_depth(raw: Option<String>, headers: &HeaderMap) -> Result<i32, ApiError> {
    match raw {
        None => Ok(core_comment::DEFAULT_TREE_DEPTH),
        Some(s) => s.trim().parse::<i32>().map_err(|_| {
            ApiError::new(Error::Validation(format!("geçersiz depth: \"{s}\"")))
                .with_request_id(headers)
        }),
    }
}

/// Bir [`core_comment::CommentNode`] ağacını yanıt DTO'suna çevirir.
///
/// Özyineleme burada: ağacın derinliği `actos_core::comment::
/// MAX_COMMENT_DEPTH` (32) ile şema seviyesinde sınırlı olduğu için yığın
/// taşması riski yok — sınırsız derinlikte bir ağaç kurulamıyor.
fn comment_node(
    node: &core_comment::CommentNode,
    id_codec: &actos_core::id::IdCodec,
) -> Result<CommentNodeResponse, Error> {
    Ok(CommentNodeResponse {
        content: content_summary(&node.content, id_codec)?,
        replies: node
            .replies
            .iter()
            .map(|child| comment_node(child, id_codec))
            .collect::<Result<Vec<_>, Error>>()?,
    })
}

// --- Handler'lar -----------------------------------------------------------

/// `POST /posts/{id}/comments` → `201` + `Location: /comments/{id}`.
///
/// `Idempotency-Key` **desteklenmiyor** (post oluşturmanın aksine): plan
/// bunu yalnızca `POST /posts` için istiyor. Yorum tekrarının bedeli bir
/// post tekrarınınkinden düşük ve yorumlar çok daha sık yazılıyor — her
/// yorum için Redis'e fazladan iki tur atmak, karşılığında kazanılandan
/// pahalı olurdu.
#[utoipa::path(
    post,
    path = "/posts/{id}/comments",
    tag = "comments",
    summary = "Bir post'a (ya da başka bir yoruma) yorum ekle",
    description = "`parent_id` verilmezse yorum post'un doğrudan çocuğu olur; verilirse o yoruma yanıt olur.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Post'un dış id'si (`c_...`)"),
    ),
    request_body = CreateCommentRequest,
    responses(
        (status = 201, description = "Yorum oluşturuldu", body = ContentSummary,
            headers(("location" = String, description = "Yeni yorumun yolu: /comments/{id}"))),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn create_comment(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(post_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CreateCommentRequest>,
) -> Result<Response, ApiError> {
    let post_id = decode_content_id(&post_id, state.id_codec(), "post")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let parent_id = req
        .parent_id
        .as_deref()
        .map(|raw| decode_content_id(raw, state.id_codec(), "comment"))
        .transpose()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let attachment_ids = crate::routes::posts::decode_attachment_ids(
        req.attachment_ids.as_deref(),
        state.id_codec(),
    )
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let content = core_comment::create_comment(
        state.db(),
        &current.actor,
        post_id,
        parent_id,
        &req.body,
        &attachment_ids,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let ekler = actos_core::attachment::list_for_content(state.db(), content.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let summary = crate::routes::posts::content_summary_with(
        &content,
        state.id_codec(),
        Some((&ekler, state.storage())),
    )
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let location = format!("/comments/{}", summary.id);
    let mut response = (StatusCode::CREATED, Json(summary)).into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }

    Ok(response)
}

/// `GET /posts/{id}/comments` → `200` (iç içe ağaç, cursor'lu),
/// `404` (post yok), `410` (post silinmiş).
///
/// Yanıt `actos_types::content::CommentThreadResponse` şeklinde ama
/// `Json<Value>` olarak elle kuruluyor — `crate::routes::posts::
/// list_actor_posts`'takiyle aynı sebep değil: burada `?fields=`
/// **desteklenmiyor**, çünkü alan filtresi düğümün `replies` anahtarını da
/// eleyip ağacı düzleştirebilirdi. Ağaç uçlarında alan seçimi ayrı bir
/// tasarım kararı gerektiriyor; şimdilik bilinçli olarak kapsam dışı
/// (bkz. PLAN.md Faz 9 notları).
#[utoipa::path(
    get,
    path = "/posts/{id}/comments",
    tag = "comments",
    summary = "Bir post'un yorum ağacını listele",
    description = "`?fields=` bu uçta **desteklenmiyor** (ağacın `replies` alanını bozardı).",
    params(
        ("id" = String, Path, description = "Post'un dış id'si (`c_...`)"),
        ("sort" = Option<String>, Query, description = "`new` ya da `top`"),
        ("depth" = Option<String>, Query, description = "Ağacın kaç seviye derine ineceği (varsayılan: `DEFAULT_TREE_DEPTH`)"),
        ("parent" = Option<String>, Query, description = "Verilirse yalnızca bu yorumun alt ağacı döner"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı (yalnızca üst seviyeyi sayfalar)"),
        ("limit" = Option<String>, Query, description = "Sayfa başına üst seviye yorum sayısı"),
    ),
    responses(
        (status = 200, description = "İç içe yorum ağacı, cursor'lu", body = CommentThreadResponse),
        ValidationFailed,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn list_comments(
    State(state): State<AppState>,
    Path(post_id): Path<String>,
    Query(query): Query<CommentTreeQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let post_id = decode_content_id(&post_id, state.id_codec(), "post")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let sort = CommentSort::parse(query.sort.as_deref())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let parent = query
        .parent
        .as_deref()
        .map(|raw| decode_content_id(raw, state.id_codec(), "comment"))
        .transpose()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let depth = parse_depth(query.depth, &headers)?;
    let limit = parse_limit(query.limit, &headers)?;

    // Cursor'ın imzası hangi sıralamaya ait olduğunu taşıyor; `?sort=` ile
    // uyuşmayan bir cursor burada reddedilir (bkz. `decode_cursor_with`).
    let sort_kind = match sort {
        CommentSort::New => SortKind::New,
        CommentSort::Top => SortKind::Top,
    };
    let cursor = decode_cursor_with(
        state.cursor_codec(),
        query.cursor.as_deref(),
        sort_kind,
        &headers,
    )?;

    let page =
        core_comment::list_comment_tree(state.db(), post_id, parent, sort, depth, cursor, limit)
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let comments = page
        .items
        .iter()
        .map(|node| comment_node(node, state.id_codec()))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let next_cursor = page.next_cursor.map(|c| state.cursor_codec().encode(&c));

    Ok(Json(serde_json::json!({
        "comments": comments,
        "next_cursor": next_cursor,
    })))
}

/// `GET /comments/{id}` → `200` (yorum + ata zinciri), `404`.
///
/// **Silinmiş yorum `410` DÖNMEZ**, `deleted: true` ve `[silindi]` gövdesi
/// ile `200` döner — `GET /posts/{id}`'in aksine. Gerekçe
/// `actos_core::comment::get_comment` dokümanında: silinen yorumun
/// çocukları yaşamaya devam ediyor, dolayısıyla düğümün kendisi de
/// erişilebilir kalmalı ki bir yanıtın breadcrumb'ı ortadan kopmasın.
#[utoipa::path(
    get,
    path = "/comments/{id}",
    tag = "comments",
    summary = "Tekil bir yorumu, ata zinciriyle birlikte oku",
    description = "Silinmiş bir yorum `410` DÖNMEZ, `deleted: true` ve `[silindi]` gövdesiyle `200` döner — \
        çocukları yaşamaya devam ettiği için düğümün kendisi erişilebilir kalmalı.",
    params(
        ("id" = String, Path, description = "Yorumun dış id'si (`c_...`)"),
    ),
    responses(
        (status = 200, description = "Yorum + kökten kendisine kadar ata zinciri", body = CommentDetailResponse),
        NotFound,
        RateLimited,
    )
)]
async fn get_comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CommentDetailResponse>, ApiError> {
    let comment_id = decode_content_id(&id, state.id_codec(), "comment")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let comment = core_comment::get_comment(state.db(), comment_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let ancestors = core_comment::ancestors_of(state.db(), comment_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let ekler = actos_core::attachment::list_for_content(state.db(), comment_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let comment = crate::routes::posts::content_summary_with(
        &comment,
        state.id_codec(),
        Some((&ekler, state.storage())),
    )
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let ancestors = ancestors
        .iter()
        .map(|content| content_summary(content, state.id_codec()))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(CommentDetailResponse { comment, ancestors }))
}

/// `PATCH /comments/{id}` → `200`, `403` (sahibi değil), `404`, `410`.
#[utoipa::path(
    patch,
    path = "/comments/{id}",
    tag = "comments",
    summary = "Bir yorumu düzenle",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Yorumun dış id'si (`c_...`)"),
    ),
    request_body = UpdateCommentRequest,
    responses(
        (status = 200, description = "Güncellenmiş yorum", body = ContentSummary),
        Unauthorized,
        Forbidden,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn update_comment(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateCommentRequest>,
) -> Result<Json<actos_types::content::ContentSummary>, ApiError> {
    let comment_id = decode_content_id(&id, state.id_codec(), "comment")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let content = core_comment::update_comment(state.db(), comment_id, current.actor.id, &req.body)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let summary = content_summary(&content, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(summary))
}

/// `DELETE /comments/{id}` → `204`, `403`, `404`, `410`.
///
/// Soft-delete: düğüm ağaçta kalır, çocukları yaşamaya devam eder.
#[utoipa::path(
    delete,
    path = "/comments/{id}",
    tag = "comments",
    summary = "Bir yorumu sil (soft-delete)",
    description = "Sahibi ya da moderatör/admin çağırabilir. Düğüm ağaçta kalır, çocukları yaşamaya devam eder.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Yorumun dış id'si (`c_...`)"),
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
async fn delete_comment(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let comment_id = decode_content_id(&id, state.id_codec(), "comment")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    core_comment::delete_comment(state.db(), comment_id, current.actor.id, &current.roles)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /actors/{username}/comments` → `200` (cursor'lu, en yeni önce),
/// `404`, `410` (Faz 7'den devir).
///
/// Burada `?fields=` **destekleniyor** (ağaç ucunun aksine): bu düz bir
/// liste, `replies` anahtarı yok, filtrenin bozacağı bir yapı da yok.
#[utoipa::path(
    get,
    path = "/actors/{username}/comments",
    tag = "comments",
    summary = "Bir actor'ün yorumlarını listele",
    description = "En yeni önce, düz liste (ağaç değil).",
    params(
        ("username" = String, Path, description = "Actor'ün kullanıcı adı"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı"),
        ("fields" = Option<String>, Query,
            description = "Virgülle ayrılmış alan adları; her yorum öğesine uygulanır"),
    ),
    responses(
        (status = 200, description = "Yorum listesi, cursor'lu", body = CommentListResponse),
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn list_actor_comments(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Query(query): Query<ActorCommentsQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_comment::list_comments_by_actor(state.db(), &username, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let selected_fields = fields::parse_fields(query.fields.as_deref());

    let comments = page
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
        "comments": comments,
        "next_cursor": next_cursor,
    })))
}
