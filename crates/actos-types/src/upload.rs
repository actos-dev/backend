//! How a content's attachments are shown in a response.
//!
//! There is no standalone upload endpoint (REFACTOR.md §4: "this is not an
//! image host") — an attachment is only ever created as a side effect of
//! `POST /posts`/`POST /posts/{id}/comments` (multipart form, see
//! `actos_types::content::CreatePostRequest`'s documentation), so this type
//! only ever appears nested inside `ContentSummary.attachments`, never as a
//! response on its own.

use serde::{Deserialize, Serialize};

/// One attachment, as shown in `ContentSummary.attachments`.
///
/// `url` and `thumbnail_url` are **directly usable**: the bucket is
/// public-read, so neither signing nor a second call is needed (see PLAN.md
/// phase 13 — a move to private + presigned URLs is possible later, and it
/// would change only the lifetime of these fields, not their meaning).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadResponse {
    pub id: String,
    pub url: String,
    pub thumbnail_url: String,
    /// Always `image/webp` after normalization.
    pub mime_type: String,
    pub byte_size: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// SHA-256 of the stored (normalized) file, hex-encoded.
    pub checksum_sha256: String,
    /// RFC 3339.
    pub created_at: String,
}
