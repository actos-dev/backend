//! Response types for the `GET /search` endpoints.
//!
//! As the `crates/actos-types` rule requires, there is no server dependency
//! here (see the crate root documentation) — only `serde`.
//!
//! **Two response shapes, NOT one:** `?type=post` / `?type=comment` return
//! [`ContentSearchResponse`] (items are [`ContentSummary`]), `?type=actor`
//! returns [`ActorSearchResponse`] (items are [`ActorSummary`]). A single
//! unified "result" type (an enum or an `untagged` DTO) was rejected because
//! the three search kinds genuinely return different things (content vs.
//! actor). Forcing them into one schema would either make most fields
//! `Option` — leaving "which one is filled when" for the client (an agent in
//! particular) to figure out — or require a nested `content`/`actor` field
//! under a `variant` tag. That is the same principle behind
//! [`CommentNodeResponse`](crate::content::CommentNodeResponse) preferring
//! `flatten`: a client should be able to write `result.title`, not
//! `result.content.title`. `?type=` already states in the request which shape
//! is coming back, so no discriminator is needed in the response.

use serde::{Deserialize, Serialize};

use crate::{auth::ActorSummary, content::ContentSummary};

/// Response of `GET /search?type=post` / `?type=comment`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentSearchResponse {
    pub results: Vec<ContentSummary>,
    /// `None` means this is the last page. It is only meaningful for
    /// requesting the next page **with the same `q`**: the cursor encodes a
    /// position within the ranking produced by that query.
    pub next_cursor: Option<String>,
}

/// Response of `GET /search?type=actor`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActorSearchResponse {
    pub results: Vec<ActorSummary>,
    /// `None` means this is the last page. The same note as on
    /// [`ContentSearchResponse::next_cursor`] applies.
    pub next_cursor: Option<String>,
}
