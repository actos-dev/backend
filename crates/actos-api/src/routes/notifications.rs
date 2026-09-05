//! Bildirim rotaları: gelen kutusu (`GET /me/inbox`) ve okundu işaretleme.
//!
//! `actos_core::notification`'ın domain katmanını HTTP'ye çeviren ince bir
//! katman — `crate::routes::comments`/`interactions` ile aynı desen. Ortak
//! dönüşümler (`actor_summary`/`encode_actor_id`) burada tekrar yazılmıyor,
//! `crate::routes::auth`'tan alınıyor (bkz. o modülün `pub(crate)` alanları).

use actos_core::{
    Error,
    id::{Actor as ActorIdKind, Content as ContentIdKind, Notification as NotificationIdKind},
    notification as core_notification,
};
use actos_types::notification::{InboxResponse, MarkAllReadResponse, NotificationSummary};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::CurrentActor,
    error::ApiError,
    openapi::{NotFound, RateLimited, Unauthorized, ValidationFailed},
    routes::actors::{decode_cursor, parse_limit},
    routes::auth::actor_summary,
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_inbox))
        .routes(routes!(mark_read))
        .routes(routes!(mark_all_read))
}

/// `GET /me/inbox?unread=&cursor=&limit=` query'si.
#[derive(Debug, Deserialize)]
struct InboxQuery {
    unread: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

/// `POST /me/inbox/read?cursor=` query'si.
///
/// **Gövde değil query parametresi:** bu API'de her cursor her zaman
/// `?cursor=` query'sinden geliyor (bkz. `docs/API.md` §3.2), `POST`
/// olması bunu değiştirmiyor — istemcinin `Content-Type`/JSON gövde
/// kurmasına hiç gerek kalmıyor, boş bir `POST` (gövdesiz) tüm okunmamışları
/// işaretlemek için yeterli.
#[derive(Debug, Deserialize)]
struct MarkAllReadQuery {
    #[serde(default)]
    cursor: Option<String>,
}

/// Ham `unread` query değerini çözer. Verilmezse `false` (filtre yok —
/// hem okunmuş hem okunmamış bildirimler döner). `crate::routes::comments::
/// parse_body_html` ile aynı desen: `Query<T>`'nin kendi `bool`
/// ayrıştırması yerine elle çözmemizin sebebi, tip hatasının düz metin
/// `400` değil RFC 9457 `application/problem+json` dönmesi gerekmesi.
fn parse_unread(raw: Option<String>, headers: &HeaderMap) -> Result<bool, ApiError> {
    match raw {
        None => Ok(false),
        Some(s) => s.trim().parse::<bool>().map_err(|_| {
            ApiError::new(Error::Validation(format!("invalid unread: \"{s}\"")))
                .with_request_id(headers)
        }),
    }
}

/// `target_type`'a göre `target_id`'yi doğru id uzayında kodlar.
///
/// `"content"`/`"actor"` dışında bir değer bu koddan asla çıkmamalı — bu
/// crate'teki her yazma yolu (`crate::comment::create_comment`,
/// `crate::interaction::follow`, `crate::moderation`) yalnızca bu ikisini
/// yazıyor (bkz. `actos_core::notification` modül dokümantasyonu). Yine de
/// bir tutarsızlık olursa panik değil [`Error::Internal`] — istemciye asla
/// detaylandırılmaz, yalnızca loglanır.
fn encode_target_id(
    id_codec: &actos_core::id::IdCodec,
    target_type: &str,
    target_id: i64,
) -> Result<String, Error> {
    match target_type {
        "content" => Ok(id_codec.encode::<ContentIdKind>(target_id)?),
        "actor" => Ok(id_codec.encode::<ActorIdKind>(target_id)?),
        other => Err(Error::Internal(format!(
            "notifications: unrecognized target_type: \"{other}\""
        ))),
    }
}

fn notification_summary(
    n: &core_notification::Notification,
    id_codec: &actos_core::id::IdCodec,
) -> Result<NotificationSummary, Error> {
    let actor = n
        .actor
        .as_ref()
        .map(|a| actor_summary(a, None, id_codec))
        .transpose()?;

    Ok(NotificationSummary {
        id: id_codec.encode::<NotificationIdKind>(n.id)?,
        kind: n.kind.as_str().to_owned(),
        actor,
        target_type: n.target_type.clone(),
        target_id: encode_target_id(id_codec, &n.target_type, n.target_id)?,
        payload: n.payload.clone(),
        created_at: n.created_at.to_rfc3339(),
        read_at: n.read_at.map(|t| t.to_rfc3339()),
    })
}

// --- Handler'lar -------------------------------------------------------

/// `GET /me/inbox` → `200`, `401`.
#[utoipa::path(
    get,
    path = "/me/inbox",
    tag = "notifications",
    summary = "List your inbox (notifications)",
    description = "Newest first, keyset-cursor paginated (the same scheme as everywhere else — no new \
        scheme was invented). `?unread=true` returns unread notifications only. `unread_count` is \
        always the TOTAL unread count, not the number of items on this page.",
    security(("api_key" = [])),
    params(
        ("unread" = Option<bool>, Query, description = "If `true`, unread notifications only (default: `false`, all)"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Notification list, with a cursor, plus the unread count", body = InboxResponse),
        ValidationFailed,
        Unauthorized,
        RateLimited,
    )
)]
async fn get_inbox(
    current: CurrentActor,
    State(state): State<AppState>,
    Query(query): Query<InboxQuery>,
    headers: HeaderMap,
) -> Result<Json<InboxResponse>, ApiError> {
    let unread_only = parse_unread(query.unread, &headers)?;
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page =
        core_notification::list_inbox(state.db(), current.actor.id, unread_only, cursor, limit)
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let unread_count = core_notification::count_unread(state.db(), current.actor.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let notifications = page
        .items
        .iter()
        .map(|n| notification_summary(n, state.id_codec()))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(InboxResponse {
        notifications,
        next_cursor: page.next_cursor.map(|c| state.cursor_codec().encode(&c)),
        unread_count,
    }))
}

/// `PATCH /me/inbox/{id}/read` → `204`, `401`, `404`. İdempotent.
#[utoipa::path(
    patch,
    path = "/me/inbox/{id}/read",
    tag = "notifications",
    summary = "Mark a single notification as read",
    description = "Idempotent: applying it again to an already-read notification does not push \
        `read_at` forward, and still returns `204`. Another actor's notification returns `404` \
        (no existence information leaks).",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The notification's external id (`n_...`)"),
    ),
    responses(
        (status = 204, description = "Marked as read (same result if already read)"),
        Unauthorized,
        NotFound,
        RateLimited,
    )
)]
async fn mark_read(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let notification_id = state
        .id_codec()
        .decode::<NotificationIdKind>(&id)
        .map_err(|_| ApiError::new(Error::NotFound("notification")).with_request_id(&headers))?;

    core_notification::mark_read(state.db(), current.actor.id, notification_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /me/inbox/read` → `200`, `400`, `401`. İdempotent.
///
/// **Toplu okundu işaretleme, tekil `PATCH`'in yanında ayrıca şart:** 200
/// bildirimi tek tek işaretlemek 200 istek demek — `cursor` verilmezse
/// TÜM okunmamışlar, verilirse `GET /me/inbox`'ın döndürdüğü o cursor'a
/// kadar olanlar tek istekte işaretlenir (bkz.
/// `actos_core::notification::mark_all_read` dokümanı).
#[utoipa::path(
    post,
    path = "/me/inbox/read",
    tag = "notifications",
    summary = "Bulk-mark notifications as read",
    description = "If `cursor` is omitted, all unread notifications are marked read; if given, only \
        those up to the cursor returned by `GET /me/inbox` are. Idempotent.",
    security(("api_key" = [])),
    params(
        ("cursor" = Option<String>, Query,
            description = "If omitted, ALL unread notifications; if given, only those up to the \
                cursor returned by `GET /me/inbox` are marked read"),
    ),
    responses(
        (status = 200, description = "Number of notifications newly marked read by this call", body = MarkAllReadResponse),
        ValidationFailed,
        Unauthorized,
        RateLimited,
    )
)]
async fn mark_all_read(
    current: CurrentActor,
    State(state): State<AppState>,
    Query(query): Query<MarkAllReadQuery>,
    headers: HeaderMap,
) -> Result<Json<MarkAllReadResponse>, ApiError> {
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let marked = core_notification::mark_all_read(state.db(), current.actor.id, cursor)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(MarkAllReadResponse {
        marked: marked as i64,
    }))
}
