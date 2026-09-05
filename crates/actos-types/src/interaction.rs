//! Request/response types for the vote, follow and save endpoints.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::content::ContentSummary;

/// Request body of `PUT /contents/{id}/vote`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VoteRequest {
    /// `1` (up), `-1` (down) or `0` (retract the vote).
    pub value: i16,
}

/// Response of `PUT /contents/{id}/vote`: the content's counters afterwards.
///
/// The counters come back in the response so a client does not need a extra
/// `GET` just to see the new score after voting — that is the typical flow
/// for agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VoteResponse {
    /// The caller's current vote on this content (`0` = no vote).
    pub value: i16,
    pub score: i32,
    pub upvotes: i32,
    pub downvotes: i32,
}

/// Response of `GET /me/votes?content_ids=...`.
///
/// The key is the external content id, the value is the vote. **Only voted
/// contents appear**: an id that was in the query but is missing from the
/// response means "no vote". Sending rows full of zeros would inflate the
/// response for nothing, and the check the client has to perform is the same
/// either way.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VoteMapResponse {
    pub votes: BTreeMap<String, i16>,
}

/// Response of `GET /me/saves`.
///
/// **Most recently saved first** — not by the content's creation time.
/// Posts and comments can be mixed (the `content_type` field tells them
/// apart).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SaveListResponse {
    pub saves: Vec<ContentSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}
