//! Request/response types for the report and admin endpoints.

use serde::{Deserialize, Serialize};

/// Request body of `POST /reports`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateReportRequest {
    /// `"post"` or `"comment"`. Must match the content's actual type.
    pub target_type: String,
    pub target_id: String,
    pub reason: String,
}

/// A report record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReportSummary {
    pub id: String,
    pub target_type: String,
    pub target_id: String,
    pub reason: String,
    /// One of `"pending"`, `"resolved"` or `"dismissed"`.
    pub status: String,
    pub notes: Option<String>,
    /// Name of the community the reported content belongs to; `None` for an
    /// independent post.
    pub community: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339. `None` means it has not been resolved yet.
    pub resolved_at: Option<String>,
}

/// Response of `GET /admin/reports`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReportListResponse {
    pub reports: Vec<ReportSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}

/// Request body of `PATCH /admin/reports/{id}`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateReportRequest {
    pub status: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Request body of `DELETE /admin/contents/{id}`.
///
/// The reason is **required**: it is what gets written to the audit trail,
/// and a trail without the answer to "why was this deleted" is useless.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ModerateDeleteRequest {
    pub reason: String,
}

/// Request body of `POST /admin/bans`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateBanRequest {
    pub username: String,
    pub reason: String,
    /// RFC 3339. When omitted, the ban is permanent.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Community name for a community-scoped ban. Omitted or `null` means a
    /// platform-wide ban.
    #[serde(default)]
    pub community: Option<String>,
    /// Also queue the deletion of this actor's posts in the community.
    /// Only valid together with `community`; rejected with 400 otherwise.
    #[serde(default)]
    pub delete_posts: bool,
}

/// A ban record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BanSummary {
    pub username: String,
    pub reason: String,
    /// RFC 3339.
    pub banned_at: String,
    /// RFC 3339. `None` means permanent.
    pub expires_at: Option<String>,
    /// Name of the community this ban applies to; `None` means platform-wide.
    pub community: Option<String>,
}

/// Request body of `PUT /admin/permissions` (grant) and
/// `DELETE /admin/permissions` (revoke).
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SetPermissionRequest {
    pub username: String,
    /// Dotted permission name, e.g. `"content.delete"`.
    pub permission: String,
    /// Community name for a community-scoped grant. Omitted or `null` means
    /// a global grant. Rejected with 400 until communities exist
    /// (COMMUNITY_PLAN.md phase 2).
    #[serde(default)]
    pub community: Option<String>,
}

/// An audit trail record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminActionSummary {
    pub id: String,
    /// Username of the admin who performed the action — readable, rather
    /// than a raw id.
    pub admin_username: String,
    pub action_type: String,
    pub target_type: String,
    pub target_id: i64,
    pub reason: Option<String>,
    /// RFC 3339.
    pub created_at: String,
}

/// Response of `GET /admin/actions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminActionListResponse {
    pub actions: Vec<AdminActionSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}
