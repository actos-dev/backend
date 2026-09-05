//! Response types for the tag endpoints.
//!
//! As the `crates/actos-types` rule requires, there is no server dependency
//! here (see the crate root documentation) — only `serde`.

use serde::{Deserialize, Serialize};

/// A single tag in the `GET /tags` listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagSummary {
    pub name: String,
    /// Number of **live** posts carrying this tag (deleted ones excluded).
    pub post_count: i32,
    /// RFC 3339.
    pub created_at: String,
}

/// Response of `GET /tags`: ordered by popularity, cursor-paginated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagListResponse {
    pub tags: Vec<TagSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}

/// A single match in the `GET /tags/search` response.
///
/// There is **no** `post_count`: the autocomplete query does not count posts
/// per tag on every keystroke, and sending an uncomputed number as `0` would
/// carry a wrong value as if it were right.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagMatch {
    pub name: String,
}

/// Response of `GET /tags/search?q=`.
///
/// No pagination: the number of results is bounded by a fixed server-side
/// ceiling — there is no such thing as a second page of an autocomplete
/// list, the user narrows it by typing more.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagSearchResponse {
    pub tags: Vec<TagMatch>,
}
