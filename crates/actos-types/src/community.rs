//! Request/response types for the community endpoints.
//!
//! This module carries **no server dependency** (only `serde`) — the SDKs,
//! the CLI and the TUI use this crate alongside the backend. `visibility` is
//! therefore a `String` (`"public"`/`"private"`), not the server's enum, and
//! the timestamps are RFC 3339 strings — the same rule as `crate::auth`.
//!
//! `id` strings are always encoded base62 (`m_...`); the raw `bigint`
//! primary key never leaks.

use serde::{Deserialize, Serialize};

use crate::auth::ActorSummary;
use crate::content::CommunityRefSummary;

/// Request body of `POST /communities`.
///
/// `visibility` is optional (`"public"` by default). `"private"` creates an
/// unlisted community (COMMUNITY_PLAN.md §2): it is absent from the
/// directory and only its members and moderators can see inside.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateCommunityRequest {
    pub name: String,
    /// Markdown; stored as text and length-validated (1-10000 characters).
    pub description: String,
    /// `"public"` (default) or `"private"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
}

/// Request body of `PATCH /communities/{name}`.
///
/// The name is the community's address and is not editable.
///
/// `visibility` is optional. The move is **one-way** (§2): a public
/// community may become private, never the reverse — going public would
/// expose conversations held under an expectation of privacy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateCommunityRequest {
    /// New description. Absent means "leave it unchanged".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `"public"` or `"private"`. Absent means "leave visibility alone".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
}

/// Request body of `PUT /communities/{name}/successor`.
///
/// The owner designates who inherits the community when they leave or
/// delete their account (COMMUNITY_PLAN.md §4). Any live actor is accepted,
/// including the owner themselves.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SuccessorRequest {
    pub username: String,
}

/// The outward-facing summary of a community.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommunitySummary {
    /// The encoded external id (`m_...`).
    pub id: String,
    pub name: String,
    /// Markdown. The server does not render it; clients decide how to show
    /// it (COMMUNITY_PLAN.md §11).
    pub description: String,
    /// `"public"` or `"private"`. A `private` community a viewer may not
    /// see inside returns a cover: the same name and description, but
    /// `is_member = false` and both counts zero.
    pub visibility: String,
    /// The community's single owner.
    pub owner: ActorSummary,
    pub member_count: i64,
    pub post_count: i64,
    /// Whether the requesting actor is a member. `false` for anonymous
    /// requests, and `false` on directory listings (there is no actor to
    /// ask about).
    pub is_member: bool,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub updated_at: String,
}

/// Response body of `GET /communities`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommunityListResponse {
    pub communities: Vec<CommunitySummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}

/// One member in the member list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommunityMemberSummary {
    pub actor: ActorSummary,
    /// RFC 3339. The list is ordered by this ascending (longest-serving
    /// first) — see `actos_core::community::list_members`.
    pub joined_at: String,
}

/// Response body of `GET /communities/{name}/members`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommunityMemberListResponse {
    pub members: Vec<CommunityMemberSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}

/// Request body of `POST /communities/{name}/invitations`.
///
/// The invitee is named by username, not by id: an invitation is a
/// human/agent act of adding someone, and usernames are what people know
/// (COMMUNITY_PLAN.md §3). Invitations exist only for private communities;
/// a public community is joined instantly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateInvitationRequest {
    pub username: String,
}

/// One pending invitation in `GET /me/invitations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InvitationSummary {
    /// The encoded external id (`i_...`) — used to accept or decline.
    pub id: String,
    pub community: CommunityRefSummary,
    /// The moderator who sent the invitation.
    pub invited_by: ActorSummary,
    /// RFC 3339.
    pub created_at: String,
}

/// Response body of `GET /me/invitations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InvitationListResponse {
    pub invitations: Vec<InvitationSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}

/// Request body of `POST /communities/{name}/applications`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateApplicationRequest {
    /// Why the applicant wants in. 1-2000 characters; this is the whole
    /// thing the moderators have to judge.
    pub reason: String,
}

/// One application in the moderation queue (`GET /communities/{name}/applications`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApplicationSummary {
    /// The encoded external id (`p_...`).
    pub id: String,
    pub community: CommunityRefSummary,
    pub applicant: ActorSummary,
    pub reason: String,
    /// `"pending"`, `"accepted"` or `"rejected"`.
    pub status: String,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339; `None` while pending.
    pub resolved_at: Option<String>,
}

/// Response body of `GET /communities/{name}/applications`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApplicationListResponse {
    pub applications: Vec<ApplicationSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}
