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

/// Request body of `POST /communities`.
///
/// There is deliberately **no `visibility` field**: phase 2 creates public
/// communities only (COMMUNITY_PLAN.md §13), and the server rejects
/// `private` at the core level. A field that can only ever hold one value
/// would be a lie in the contract; phase 4 adds it together with the gate
/// that makes the second value real.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateCommunityRequest {
    pub name: String,
    /// Markdown; stored as text and length-validated (1-10000 characters).
    pub description: String,
}

/// Request body of `PATCH /communities/{name}`.
///
/// Only the description is editable — the name is the community's address
/// and the owner cannot change it in phase 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateCommunityRequest {
    pub description: String,
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
    /// `"public"` or `"private"`. Phase 2 always returns `"public"`.
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
