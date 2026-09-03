//! Şikayet ve moderasyon rotaları.
//!
//! `POST /reports` **herkese açık** (kimlikli her actor şikayet edebilir);
//! `/admin/*` altındaki her şey [`ModeratorActor`] ya da [`AdminActor`]
//! extractor'ı gerektiriyor — yetki kontrolü handler gövdesinde değil tip
//! imzasında, böylece unutulamıyor (bkz. o extractor'ların dokümantasyonu).
//!
//! Denetim izi yazımı bu dosyada **hiç görünmüyor**: her admin işlemi kendi
//! kaydını `actos_core::moderation` içinde, kendi transaction'ında yazıyor.
//! Gerekçe o modülün dokümantasyonunda — buraya bırakılsaydı yeni bir uç
//! eklerken atlanabilirdi.

use actos_core::{
    Error,
    auth::AdminRole,
    id::{Content as ContentIdKind, Report as ReportIdKind},
    moderation::{self as core_mod, ReportStatus, ReportTargetType},
};
use actos_types::moderation::{
    AdminActionListResponse, AdminActionSummary, BanSummary, CreateBanRequest, CreateReportRequest,
    ModerateDeleteRequest, ReportListResponse, ReportSummary, SetRoleRequest, UpdateReportRequest,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::{AdminActor, CurrentActor, ModeratorActor},
    error::ApiError,
    openapi::{Conflict, Forbidden, Gone, NotFound, RateLimited, Unauthorized, ValidationFailed},
    routes::actors::{decode_cursor, parse_limit},
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_report))
        .routes(routes!(list_reports))
        .routes(routes!(update_report))
        .routes(routes!(moderate_delete_content))
        .routes(routes!(create_ban))
        .routes(routes!(remove_ban))
        .routes(routes!(set_role))
        .routes(routes!(list_actions))
}

/// `GET /admin/reports?status=&cursor=&limit=` query'si.
#[derive(Debug, Deserialize)]
struct ReportListQuery {
    status: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

/// `GET /admin/actions?cursor=&limit=` query'si.
#[derive(Debug, Deserialize)]
struct ActionListQuery {
    cursor: Option<String>,
    limit: Option<String>,
}

/// `"post"`/`"comment"` metnini enum'a çevirir.
fn parse_target_type(raw: &str) -> Result<ReportTargetType, Error> {
    match raw {
        "post" => Ok(ReportTargetType::Post),
        "comment" => Ok(ReportTargetType::Comment),
        other => Err(Error::Validation(format!(
            "invalid target_type: \"{other}\" (expected: post, comment)"
        ))),
    }
}

fn target_type_str(t: ReportTargetType) -> &'static str {
    match t {
        ReportTargetType::Post => "post",
        ReportTargetType::Comment => "comment",
    }
}

fn status_str(s: ReportStatus) -> &'static str {
    match s {
        ReportStatus::Pending => "pending",
        ReportStatus::Resolved => "resolved",
        ReportStatus::Dismissed => "dismissed",
    }
}

/// Bir [`core_mod::Report`]'u yanıt DTO'suna çevirir.
///
/// `reporter_actor_id` **kasıtlı olarak yanıtta yok**: şikayet edenin
/// kimliği moderatörün kararını etkilememeli ve yanıt bir şekilde dışarı
/// sızarsa misilleme riski doğururdu. Gerektiğinde denetim izinden ya da
/// doğrudan veritabanından bakılabilir.
fn report_summary(
    rapor: &core_mod::Report,
    id_codec: &actos_core::id::IdCodec,
) -> Result<ReportSummary, Error> {
    Ok(ReportSummary {
        id: id_codec.encode::<ReportIdKind>(rapor.id)?,
        target_type: target_type_str(rapor.target_type).to_owned(),
        target_id: id_codec.encode::<ContentIdKind>(rapor.target_id)?,
        reason: rapor.reason.clone(),
        status: status_str(rapor.status).to_owned(),
        notes: rapor.notes.clone(),
        created_at: rapor.created_at.to_rfc3339(),
        resolved_at: rapor.resolved_at.map(|t| t.to_rfc3339()),
    })
}

// --- Şikayet (herkese açık) --------------------------------------------------

/// `POST /reports` → `201`, `400`, `404`, `409` (aynı hedefi tekrar
/// şikayet), `410`.
#[utoipa::path(
    post,
    path = "/reports",
    tag = "moderation",
    summary = "Bir post ya da yorumu şikayet et",
    description = "Herkese açık: kimlikli her actor şikayet edebilir.",
    security(("api_key" = [])),
    request_body = CreateReportRequest,
    responses(
        (status = 201, description = "Şikayet oluşturuldu", body = ReportSummary),
        ValidationFailed,
        Unauthorized,
        NotFound,
        Conflict,
        Gone,
        RateLimited,
    )
)]
async fn create_report(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateReportRequest>,
) -> Result<(StatusCode, Json<ReportSummary>), ApiError> {
    let target_type = parse_target_type(&req.target_type)
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let target_id = state
        .id_codec()
        .decode::<ContentIdKind>(&req.target_id)
        .map_err(|_| ApiError::new(Error::NotFound("content")).with_request_id(&headers))?;

    let rapor = core_mod::create_report(
        state.db(),
        current.actor.id,
        target_type,
        target_id,
        &req.reason,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let yanit = report_summary(&rapor, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok((StatusCode::CREATED, Json(yanit)))
}

// --- Moderasyon kuyruğu -----------------------------------------------------

/// `GET /admin/reports` → `200`, `401`, `403`.
#[utoipa::path(
    get,
    path = "/admin/reports",
    tag = "admin",
    summary = "Moderasyon kuyruğunu listele",
    description = "Moderatör veya admin gerektirir.",
    security(("api_key" = [])),
    params(
        ("status" = Option<String>, Query, description = "`pending`, `resolved` ya da `dismissed`"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı"),
    ),
    responses(
        (status = 200, description = "Şikayet listesi, cursor'lu", body = ReportListResponse),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        RateLimited,
    )
)]
async fn list_reports(
    _moderator: ModeratorActor,
    State(state): State<AppState>,
    Query(query): Query<ReportListQuery>,
    headers: HeaderMap,
) -> Result<Json<ReportListResponse>, ApiError> {
    let status = query
        .status
        .as_deref()
        .map(ReportStatus::parse)
        .transpose()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let sayfa = core_mod::list_reports(state.db(), status, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let reports = sayfa
        .items
        .iter()
        .map(|r| report_summary(r, state.id_codec()))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(ReportListResponse {
        reports,
        next_cursor: sayfa.next_cursor.map(|c| state.cursor_codec().encode(&c)),
    }))
}

/// `PATCH /admin/reports/{id}` → `200`, `401`, `403`, `404`.
#[utoipa::path(
    patch,
    path = "/admin/reports/{id}",
    tag = "admin",
    summary = "Bir şikayeti çöz/reddet",
    description = "Moderatör veya admin gerektirir.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Şikayetin dış id'si"),
    ),
    request_body = UpdateReportRequest,
    responses(
        (status = 200, description = "Güncellenmiş şikayet", body = ReportSummary),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        NotFound,
        RateLimited,
    )
)]
async fn update_report(
    moderator: ModeratorActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateReportRequest>,
) -> Result<Json<ReportSummary>, ApiError> {
    let report_id = state
        .id_codec()
        .decode::<ReportIdKind>(&id)
        .map_err(|_| ApiError::new(Error::NotFound("report")).with_request_id(&headers))?;

    let status =
        ReportStatus::parse(&req.status).map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let rapor = core_mod::update_report(
        state.db(),
        moderator.actor.id,
        report_id,
        status,
        req.notes.as_deref(),
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let yanit = report_summary(&rapor, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(yanit))
}

/// `DELETE /admin/contents/{id}` → `204`, `401`, `403`, `404`, `410`.
///
/// Gerekçe gövdede zorunlu; `DELETE`'in gövde taşıması alışılmadık ama
/// alternatifler daha kötüydü: gerekçeyi query string'e koymak onu
/// sunucu erişim loglarına düşürürdü, ayrı bir `POST` ucu ise aynı işi iki
/// isimle yapmak olurdu.
#[utoipa::path(
    delete,
    path = "/admin/contents/{id}",
    tag = "admin",
    summary = "Moderatör olarak bir içeriği sil",
    description = "Moderatör veya admin gerektirir. Gerekçe gövdede zorunlu (denetim izine yazılır).",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "İçeriğin dış id'si (`c_...`)"),
    ),
    request_body = ModerateDeleteRequest,
    responses(
        (status = 204, description = "Silindi"),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn moderate_delete_content(
    moderator: ModeratorActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ModerateDeleteRequest>,
) -> Result<StatusCode, ApiError> {
    let content_id = state
        .id_codec()
        .decode::<ContentIdKind>(&id)
        .map_err(|_| ApiError::new(Error::NotFound("content")).with_request_id(&headers))?;

    core_mod::moderate_delete_content(state.db(), moderator.actor.id, content_id, &req.reason)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

// --- Ban'ler ----------------------------------------------------------------

/// `POST /admin/bans` → `201`, `400`, `401`, `403`, `404`.
#[utoipa::path(
    post,
    path = "/admin/bans",
    tag = "admin",
    summary = "Bir actor'ü banla",
    description = "Moderatör veya admin gerektirir. `expires_at` verilmezse ban kalıcı.",
    security(("api_key" = [])),
    request_body = CreateBanRequest,
    responses(
        (status = 201, description = "Ban oluşturuldu", body = BanSummary),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        NotFound,
        RateLimited,
    )
)]
async fn create_ban(
    moderator: ModeratorActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateBanRequest>,
) -> Result<(StatusCode, Json<BanSummary>), ApiError> {
    let expires_at = req
        .expires_at
        .as_deref()
        .map(|raw| {
            chrono::DateTime::parse_from_rfc3339(raw)
                .map(|t| t.with_timezone(&chrono::Utc))
                .map_err(|_| {
                    Error::Validation(format!("invalid expires_at: \"{raw}\" (expected RFC 3339)"))
                })
        })
        .transpose()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let ban = core_mod::ban_actor(
        state.db(),
        moderator.actor.id,
        &req.username,
        &req.reason,
        expires_at,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok((
        StatusCode::CREATED,
        Json(BanSummary {
            username: ban.username,
            reason: ban.reason,
            banned_at: ban.banned_at.to_rfc3339(),
            expires_at: ban.expires_at.map(|t| t.to_rfc3339()),
        }),
    ))
}

/// `DELETE /admin/bans/{username}` → `204`, `401`, `403`, `404`.
/// İdempotent: ban yoksa da başarı döner.
#[utoipa::path(
    delete,
    path = "/admin/bans/{username}",
    tag = "admin",
    summary = "Bir actor'ün banını kaldır",
    description = "Moderatör veya admin gerektirir. İdempotent: ban yoksa da başarı döner.",
    security(("api_key" = [])),
    params(
        ("username" = String, Path, description = "Banı kaldırılacak actor'ün kullanıcı adı"),
    ),
    responses(
        (status = 204, description = "Ban kaldırıldı (ya da zaten yoktu)"),
        Unauthorized,
        Forbidden,
        NotFound,
        RateLimited,
    )
)]
async fn remove_ban(
    moderator: ModeratorActor,
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    core_mod::unban_actor(state.db(), moderator.actor.id, &username)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

// --- Roller (yalnızca admin) ------------------------------------------------

/// `POST /admin/roles` → `204`, `400`, `401`, `403`, `404`.
///
/// [`AdminActor`] gerektiriyor, [`ModeratorActor`] değil: bir moderatörün
/// kendine ya da başkasına admin verebilmesi yetki sınırını anlamsız
/// kılardı.
#[utoipa::path(
    post,
    path = "/admin/roles",
    tag = "admin",
    summary = "Bir actor'e rol ata (ya da rolünü kaldır)",
    description = "Yalnızca **admin** çağırabilir (moderatör yeterli değil). `role: null` mevcut rolü kaldırır.",
    security(("api_key" = [])),
    request_body = SetRoleRequest,
    responses(
        (status = 204, description = "Rol güncellendi"),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        NotFound,
        RateLimited,
    )
)]
async fn set_role(
    admin: AdminActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetRoleRequest>,
) -> Result<StatusCode, ApiError> {
    let role = match req.role.as_deref() {
        None => None,
        Some("admin") => Some(AdminRole::Admin),
        Some("moderator") => Some(AdminRole::Moderator),
        Some(other) => {
            return Err(ApiError::new(Error::Validation(format!(
                "invalid role: \"{other}\" (expected: admin, moderator, or null)"
            )))
            .with_request_id(&headers));
        }
    };

    core_mod::set_role(state.db(), admin.actor.id, &req.username, role)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

// --- Denetim izi ------------------------------------------------------------

/// `GET /admin/actions` → `200`, `401`, `403`.
///
/// `target_id` ham `bigint` olarak dönüyor, dış id'ye çevrilmiyor: iz
/// polimorfik (hedef actor da içerik de olabilir, bkz.
/// `migrations/0015`'te FK olmaması) ve hangi id uzayına ait olduğu
/// `target_type`'tan anlaşılıyor. Yanlış uzayla kodlamak, var olmayan bir
/// kaydı işaret eden bir id üretirdi.
#[utoipa::path(
    get,
    path = "/admin/actions",
    tag = "admin",
    summary = "Denetim izini listele",
    description = "Moderatör veya admin gerektirir. `target_id` ham `bigint` olarak döner (polimorfik hedef).",
    security(("api_key" = [])),
    params(
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı"),
    ),
    responses(
        (status = 200, description = "Denetim izi kayıtları, cursor'lu", body = AdminActionListResponse),
        ValidationFailed,
        Unauthorized,
        Forbidden,
        RateLimited,
    )
)]
async fn list_actions(
    _moderator: ModeratorActor,
    State(state): State<AppState>,
    Query(query): Query<ActionListQuery>,
    headers: HeaderMap,
) -> Result<Json<AdminActionListResponse>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let sayfa = core_mod::list_actions(state.db(), cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let actions = sayfa
        .items
        .into_iter()
        .map(|a| AdminActionSummary {
            id: a.id.to_string(),
            admin_username: a.admin_username,
            action_type: a.action_type,
            target_type: a.target_type,
            target_id: a.target_id,
            reason: a.reason,
            created_at: a.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(AdminActionListResponse {
        actions,
        next_cursor: sayfa.next_cursor.map(|c| state.cursor_codec().encode(&c)),
    }))
}
