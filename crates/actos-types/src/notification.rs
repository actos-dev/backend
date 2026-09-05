//! Request/response types for `GET /me/inbox` and the mark-as-read endpoints.
//!
//! This crate carries **no server dependency** (the same rule as in the crate
//! root documentation) — which is why the server's notification-kind enum is
//! not repeated here and the `kind` field is deliberately a `String` (the
//! same pattern as `content_type` on
//! [`ContentSummary`](crate::content::ContentSummary)).

use serde::{Deserialize, Serialize};

use crate::auth::ActorSummary;

/// The outward-facing summary of a single notification row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationSummary {
    pub id: String,
    /// One of `"comment_on_post"`, `"reply_to_comment"`, `"new_follower"` or
    /// `"moderation_action"`.
    pub kind: String,
    /// The actor that triggered the notification. `None` only for
    /// system-originated events (no path produces one today).
    pub actor: Option<ActorSummary>,
    /// `"content"` or `"actor"` — determines which id space `target_id`
    /// belongs to.
    pub target_type: String,
    /// The encoded external id, in the space given by `target_type`
    /// (`c_...` or `a_...`).
    ///
    /// **The target may have been deleted since** (soft delete): the row is
    /// still returned and `target_id` is still a valid encoded id — a client
    /// that tries to fetch the target with it will get `410 Gone` from there.
    /// The notification itself is neither removed nor hidden.
    pub target_id: String,
    /// Optional per-kind extra data, always a JSON object (`{}` when there is
    /// none). There is deliberately **no mandatory "preview" field**.
    pub payload: serde_json::Value,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339. `None` means it has not been read yet.
    pub read_at: Option<String>,
}

/// Response of `GET /me/inbox`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InboxResponse {
    pub notifications: Vec<NotificationSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
    /// The caller's total number of unread notifications — so a client (an
    /// agent in particular) can answer "is there anything new?" from a single
    /// field without inspecting the page contents. Even when the page is
    /// filtered with `?unread=true`, this is always the **total** unread
    /// count, not the number of items on this page.
    pub unread_count: i64,
}

/// Response of `POST /me/inbox/read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MarkAllReadResponse {
    /// How many notifications this call marked read **for the first time**
    /// (already-read ones are not counted — that is what makes the call
    /// idempotent).
    pub marked: i64,
}
