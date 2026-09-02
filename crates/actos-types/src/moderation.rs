//! Şikayet ve admin uçlarının istek-yanıt tipleri.

use serde::{Deserialize, Serialize};

/// `POST /reports` isteği.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateReportRequest {
    /// `"post"` veya `"comment"`. İçeriğin gerçek türüyle uyuşmalı.
    pub target_type: String,
    pub target_id: String,
    pub reason: String,
}

/// Bir şikayet kaydı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReportSummary {
    pub id: String,
    pub target_type: String,
    pub target_id: String,
    pub reason: String,
    /// `"pending"`, `"resolved"` veya `"dismissed"`.
    pub status: String,
    pub notes: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339. `None` ise henüz çözülmedi.
    pub resolved_at: Option<String>,
}

/// `GET /admin/reports` yanıtı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReportListResponse {
    pub reports: Vec<ReportSummary>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
}

/// `PATCH /admin/reports/{id}` isteği.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateReportRequest {
    pub status: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// `DELETE /admin/contents/{id}` isteği.
///
/// Gerekçe **zorunlu**: denetim izine yazılan şey bu, ve "neden silindi"
/// sorusunun cevabı olmadan iz işe yaramaz.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ModerateDeleteRequest {
    pub reason: String,
}

/// `POST /admin/bans` isteği.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateBanRequest {
    pub username: String,
    pub reason: String,
    /// RFC 3339. Verilmezse ban kalıcı.
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// Bir ban kaydı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BanSummary {
    pub username: String,
    pub reason: String,
    /// RFC 3339.
    pub banned_at: String,
    /// RFC 3339. `None` ise kalıcı.
    pub expires_at: Option<String>,
}

/// `POST /admin/roles` isteği.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SetRoleRequest {
    pub username: String,
    /// `"admin"`, `"moderator"` ya da `null` (rolü kaldır).
    #[serde(default)]
    pub role: Option<String>,
}

/// Bir denetim izi kaydı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminActionSummary {
    pub id: String,
    /// Eylemi yapan admin'in kullanıcı adı — ham id yerine okunabilir olan.
    pub admin_username: String,
    pub action_type: String,
    pub target_type: String,
    pub target_id: i64,
    pub reason: Option<String>,
    /// RFC 3339.
    pub created_at: String,
}

/// `GET /admin/actions` yanıtı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminActionListResponse {
    pub actions: Vec<AdminActionSummary>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
}
