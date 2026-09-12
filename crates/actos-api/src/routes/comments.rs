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
    extract::{DefaultBodyLimit, Path, Query, Request, State},
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
    routes::posts::{
        CREATE_CONTENT_BODY_LIMIT, content_summary, content_summary_with_body_html,
        content_summary_with_optional_body_html, decode_content_id, extract_content_payload,
    },
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    let create = OpenApiRouter::new()
        .routes(routes!(create_comment, list_comments))
        .layer(DefaultBodyLimit::max(CREATE_CONTENT_BODY_LIMIT));

    OpenApiRouter::new()
        .merge(create)
        .routes(routes!(get_comment, update_comment, delete_comment))
        // Faz 7'den devir (bkz. PLAN.md Faz 9).
        .routes(routes!(list_actor_comments))
}

// --- Query param tipleri ---------------------------------------------------

/// `GET /posts/{id}/comments?sort=&depth=&parent=&cursor=&limit=&body_html=`
/// query'si.
///
/// Sayısal alanlar (ve `body_html`) `String` olarak alınıp elle
/// ayrıştırılıyor: axum'un `Query<T>` ile ürettiği tip hatası düz metin bir
/// `400` döndürür, oysa bu API'de her hata RFC 9457
/// `application/problem+json` olmalı (bkz.
/// `crate::routes::actors::parse_limit` üzerindeki aynı gerekçe).
#[derive(Debug, Deserialize)]
struct CommentTreeQuery {
    sort: Option<String>,
    depth: Option<String>,
    parent: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
    /// `?body_html=true` ile ağaçtaki her düğüm için `body_html` hesaplanır
    /// (bkz. [`parse_body_html`] ve bu modülün `list_comments`
    /// dokümanındaki "neden ayrı bir parametre" gerekçesi).
    body_html: Option<String>,
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
            ApiError::new(Error::Validation(format!("invalid depth: \"{s}\"")))
                .with_request_id(headers)
        }),
    }
}

/// Ham `body_html` query değerini çözer.
///
/// Verilmezse `false` (varsayılan) — bugünkü davranış (ağaçta `body_html`
/// hiç hesaplanmaz) sessizce korunuyor, mevcut istemciler bu parametreden
/// habersiz olsa da yanıt biçimleri değişmez. `Query<T>`'nin kendi `bool`
/// ayrıştırması yerine burada da elle çözmemizin sebebi `parse_depth` /
/// `parse_limit` ile aynı: tip hatasının düz metin `400` değil RFC 9457
/// `application/problem+json` dönmesi gerekiyor.
fn parse_body_html(raw: Option<String>, headers: &HeaderMap) -> Result<bool, ApiError> {
    match raw {
        None => Ok(false),
        Some(s) => s.trim().parse::<bool>().map_err(|_| {
            ApiError::new(Error::Validation(format!("invalid body_html: \"{s}\"")))
                .with_request_id(headers)
        }),
    }
}

/// Bir [`core_comment::CommentNode`] ağacını yanıt DTO'suna çevirir.
///
/// Özyineleme burada: ağacın derinliği `actos_core::comment::
/// MAX_COMMENT_DEPTH` (32) ile şema seviyesinde sınırlı olduğu için yığın
/// taşması riski yok — sınırsız derinlikte bir ağaç kurulamıyor.
///
/// `include_body_html` her seviyede aynen aktarılır: `?body_html=true`
/// "yalnızca kökler" ya da "yalnızca yapraklar" gibi kısmi bir taahhüt
/// değil, ağacın tamamı için tek bir açma/kapama anahtarı — istemci zaten
/// tüm düğümleri render edeceği için kısmi hesaplama hem API'yi
/// karmaşıklaştırır hem de pratik bir tasarruf sağlamaz (bkz. bu dosyanın
/// `list_comments` dokümanı).
fn comment_node(
    node: &core_comment::CommentNode,
    id_codec: &actos_core::id::IdCodec,
    include_body_html: bool,
) -> Result<CommentNodeResponse, Error> {
    Ok(CommentNodeResponse {
        content: content_summary_with_optional_body_html(
            &node.content,
            id_codec,
            include_body_html,
        )?,
        replies: node
            .replies
            .iter()
            .map(|child| comment_node(child, id_codec, include_body_html))
            .collect::<Result<Vec<_>, Error>>()?,
    })
}

// --- Handler'lar -----------------------------------------------------------

/// A documentation-only schema for the `multipart/form-data` alternative to
/// [`CreateCommentRequest`] — see `crate::routes::posts::
/// CreatePostMultipartBody`'s doc for why this struct exists and is never
/// instantiated.
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
struct CreateCommentMultipartBody {
    /// The same JSON body the `application/json` case would carry, as a
    /// single multipart part.
    payload: CreateCommentRequest,
    /// Up to `MAX_ATTACHMENTS_PER_CONTENT` (4) image files. Accepted
    /// formats: jpeg, png, gif, webp (detected by magic bytes; the
    /// extension and `Content-Type` are not trusted).
    #[schema(content_media_type = "application/octet-stream")]
    files: Vec<Vec<u8>>,
}

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
    summary = "Add a comment to a post (or to another comment)",
    description = "If `parent_id` is omitted, the comment becomes a direct child of the post; if given, it replies to that comment. \
        Accepts EITHER `application/json` (no images) OR `multipart/form-data` (the same JSON as a `payload` \
        part, plus up to 4 `files` parts).",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The post's external id (`c_...`)"),
    ),
    request_body(
        content(
            (CreateCommentRequest = "application/json"),
            (inline(CreateCommentMultipartBody) = "multipart/form-data"),
        )
    ),
    responses(
        (status = 201, description = "Comment created", body = ContentSummary,
            headers(("location" = String, description = "Path of the new comment: /comments/{id}"))),
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
    request: Request,
) -> Result<Response, ApiError> {
    let post_id = decode_content_id(&post_id, state.id_codec(), "post")
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let (req, files): (CreateCommentRequest, Vec<Vec<u8>>) =
        extract_content_payload(request, &state, &headers).await?;

    let parent_id = req
        .parent_id
        .as_deref()
        .map(|raw| decode_content_id(raw, state.id_codec(), "comment"))
        .transpose()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let content = core_comment::create_comment(
        state.db(),
        state.storage(),
        state.id_codec(),
        &current.actor,
        post_id,
        parent_id,
        &req.body,
        &files,
        state.config().server.max_upload_bytes,
        state.config().storage_quota.bytes,
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
/// eleyip ağacı düzleştirebilirdi. Ağaç uçlarında genel bir alan seçimi hâlâ
/// bilinçli olarak kapsam dışı (bkz. PLAN.md Faz 9 notları).
///
/// **`body_html` için ayrı bir opt-in: `?body_html=true`.** `?fields=`
/// kullanılamadığı için `body_html`'i açan mekanizma da `?fields=body_html`
/// olamıyor (bkz. `crate::routes::posts::list_actor_posts` — orada bu
/// şekilde çalışıyor). Bunun yerine tek amaçlı bir bayrak: `true` ise
/// ağaçtaki **her düğüm** için `body_html` hesaplanır (bkz.
/// [`comment_node`] dokümanı — kısmi hesaplama yok), `false`/verilmemişse
/// hepsi `None` kalır ve bugünkü davranış birebir korunur. İki ayrı
/// mekanizmanın (liste uçlarında `?fields=body_html`, ağaç ucunda
/// `?body_html=true`) bir arada var olması kafa karıştırıcı görünebilir
/// ama kök sebep aynı: `?fields=` zaten bu uçta **hiç yok**, dolayısıyla
/// `body_html`'i onun bir alt kümesi gibi sunmak mümkün değil — burada
/// eklenen yeni bir genel alan seçim mekanizması değil, yalnızca
/// `body_html` için nokta atışı bir kapı.
///
/// **Ata zinciri (breadcrumb) bu parametreden etkilenmez, çünkü bu uçta
/// ata zinciri diye bir şey YOK** — `ancestors` yalnızca `GET
/// /comments/{id}` yanıtında var (bkz. `get_comment`) ve orası zaten
/// `?body_html=`'den bağımsız çalışıyor, kendi ata listesini hiçbir zaman
/// `body_html` ile doldurmuyor: atalar bağlam sağlamak için orada, okunan
/// asıl kaynak değiller (bkz. `get_comment` dokümanındaki "`ancestors`
/// bilerek dışarıda bırakılıyor" gerekçesi). Bu yüzden bu değişiklik o
/// davranışa hiç dokunmuyor.
#[utoipa::path(
    get,
    path = "/posts/{id}/comments",
    tag = "comments",
    summary = "List a post's comment tree",
    description = "`?fields=` is **not supported** on this endpoint (it would break the tree's `replies` \
        field). `body_html` is instead opted into with a separate `?body_html=true` flag — not \
        `?fields=body_html`, because `?fields=` doesn't exist here at all.",
    params(
        ("id" = String, Path, description = "The post's external id (`c_...`)"),
        ("sort" = Option<String>, Query, description = "`new` or `top`"),
        ("depth" = Option<String>, Query, description = "How many levels deep the tree should go (default: `DEFAULT_TREE_DEPTH`)"),
        ("parent" = Option<String>, Query, description = "If given, only that comment's subtree is returned"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor` (paginates the top level only)"),
        ("limit" = Option<String>, Query, description = "Top-level comments per page"),
        ("body_html" = Option<bool>, Query,
            description = "If `true`, `body_html` is computed for every node in the tree (default: `false`, not computed). \
                It's a separate parameter because `?fields=` isn't supported on this endpoint (see above) — the field \
                filter doesn't exist here since it would break the tree's `replies` structure, so `body_html` is opted \
                into with this single-purpose flag instead of `?fields=body_html`."),
    ),
    responses(
        (status = 200, description = "Nested comment tree, with a cursor", body = CommentThreadResponse),
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
    let include_body_html = parse_body_html(query.body_html, &headers)?;

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
        .map(|node| comment_node(node, state.id_codec(), include_body_html))
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
/// **Silinmiş yorum `410` DÖNMEZ**, `deleted: true` ve `[deleted]` gövdesi
/// ile `200` döner — `GET /posts/{id}`'in aksine. Gerekçe
/// `actos_core::comment::get_comment` dokümanında: silinen yorumun
/// çocukları yaşamaya devam ediyor, dolayısıyla düğümün kendisi de
/// erişilebilir kalmalı ki bir yanıtın breadcrumb'ı ortadan kopmasın.
#[utoipa::path(
    get,
    path = "/comments/{id}",
    tag = "comments",
    summary = "Read a single comment, with its ancestor chain",
    description = "A deleted comment does NOT return `410` — it returns `200` with `deleted: true` and \
        a `[deleted]` body, because its children continue to live and the node itself must stay reachable.",
    params(
        ("id" = String, Path, description = "The comment's external id (`c_...`)"),
    ),
    responses(
        (status = 200, description = "Comment plus the ancestor chain from the root down to it", body = CommentDetailResponse),
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

    // Tekil uç: `body_html` her zaman hesaplanır (bkz.
    // `actos_types::content::ContentSummary::body_html` "Nerede dolu
    // döner"). `ancestors` bilerek dışarıda bırakılıyor — bunlar
    // breadcrumb'ın kalan zinciri, uç doğrudan bunlar için çekilmiyor.
    let comment =
        content_summary_with_body_html(&comment, state.id_codec(), Some((&ekler, state.storage())))
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
    summary = "Edit a comment",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The comment's external id (`c_...`)"),
    ),
    request_body = UpdateCommentRequest,
    responses(
        (status = 200, description = "Updated comment", body = ContentSummary),
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
    summary = "Delete a comment (soft-delete)",
    description = "Callable by its owner or a moderator/admin. The node stays in the tree; its children continue to live.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The comment's external id (`c_...`)"),
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
    summary = "List an actor's comments",
    description = "Newest first, flat list (not a tree).",
    params(
        ("username" = String, Path, description = "The actor's username"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
        ("fields" = Option<String>, Query,
            description = "Comma-separated field names; applied to each comment item"),
    ),
    responses(
        (status = 200, description = "Comment list, with a cursor", body = CommentListResponse),
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
    let include_body_html = fields::wants_body_html(selected_fields.as_deref());

    let comments = page
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
        "comments": comments,
        "next_cursor": next_cursor,
    })))
}
