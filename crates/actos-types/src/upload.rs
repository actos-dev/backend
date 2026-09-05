//! Response types for the file upload endpoints.

use serde::{Deserialize, Serialize};

/// Response of `POST /uploads`, and how a content's attachments are shown.
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
