//! Dosya yükleme uçlarının yanıt tipleri.

use serde::{Deserialize, Serialize};

/// `POST /uploads` yanıtı ve bir içeriğin eklerinin gösterimi.
///
/// `url` ve `thumbnail_url` **doğrudan kullanılabilir**: bucket public-read
/// olduğu için imzalama ya da ikinci bir çağrı gerekmiyor (bkz. PLAN.md
/// Faz 13 — ileride private + presigned URL'ye geçilebilir, o zaman bu
/// alanların anlamı değil yalnızca ömrü değişir).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResponse {
    pub id: String,
    pub url: String,
    pub thumbnail_url: String,
    /// Normalize sonrası her zaman `image/webp`.
    pub mime_type: String,
    pub byte_size: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// Saklanan (normalize edilmiş) dosyanın SHA-256'sı, hex.
    pub checksum_sha256: String,
    /// RFC 3339.
    pub created_at: String,
}
