//! Etiket uçlarının yanıt tipleri.
//!
//! `crates/actos-types` kuralı gereği burada hiçbir sunucu bağımlılığı yok
//! (bkz. crate kök dokümantasyonu) — yalnızca `serde`.

use serde::{Deserialize, Serialize};

/// `GET /tags` listesindeki tek etiket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagSummary {
    pub name: String,
    /// Bu etiketi taşıyan **canlı** post sayısı (silinmişler sayılmaz).
    pub post_count: i32,
    /// RFC 3339.
    pub created_at: String,
}

/// `GET /tags` yanıtı: popülerliğe göre sıralı, cursor'lu.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagListResponse {
    pub tags: Vec<TagSummary>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
}

/// `GET /tags/search` yanıtındaki tek eşleşme.
///
/// `post_count` **yok**: otomatik tamamlama sorgusu her tuş vuruşunda
/// etiket başına post saymıyor (bkz. `actos_core::tag::TagMatch`), ve
/// hesaplanmamış bir sayıyı `0` olarak göndermek yanlış bir değeri
/// doğruymuş gibi taşımak olurdu.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagMatch {
    pub name: String,
}

/// `GET /tags/search?q=` yanıtı.
///
/// Sayfalama yok: sonuç sayısı `actos_core::tag::SEARCH_LIMIT` ile sabit
/// bir tavana bağlı — otomatik tamamlama listesinin ikinci sayfası diye bir
/// şey yok, kullanıcı yazmaya devam ederek daraltır.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TagSearchResponse {
    pub tags: Vec<TagMatch>,
}
