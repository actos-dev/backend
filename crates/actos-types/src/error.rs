use serde::{Deserialize, Serialize};

/// API'nin döndürebileceği makine-okunur hata kodları.
///
/// Yanıt gövdesinde `code` alanında string olarak taşınır (`"RATE_LIMITED"`).
/// Bu liste bir sözleşmedir: var olan bir kodun anlamı değiştirilmez, sadece
/// yenisi eklenir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
// Bilerek `non_exhaustive` değil: yeni bir kod eklendiğinde onu HTTP durumuna
// ve başlığa eşleyen `match`'lerin derlenmemesini istiyoruz. Yeni kod eklemek
// zaten API sözleşmesinde bir değişiklik.
pub enum ErrorCode {
    /// İstek gövdesi/parametreleri doğrulamadan geçmedi.
    ValidationFailed,
    /// `Authorization` header'ı yok ya da biçimi bozuk.
    MissingCredentials,
    /// API key geçersiz, iptal edilmiş veya bilinmiyor.
    InvalidKey,
    /// Kimlik doğrulandı ama bu eylem için yetki yok.
    Forbidden,
    /// Hesap askıya alınmış.
    Banned,
    /// Kaynak yok.
    NotFound,
    /// Kaynak vardı, silindi.
    Gone,
    /// Benzersizlik ihlali (kullanıcı adı alınmış, aynı rapor tekrar açılmış...).
    Conflict,
    /// Hız limiti aşıldı — `Retry-After` header'ına bak.
    RateLimited,
    /// Yüklenen dosya kabul edilmedi (tip, boyut veya içerik doğrulaması).
    UnsupportedMedia,
    /// Sayfalama cursor'ı bozuk ya da başka bir sıralamaya ait.
    InvalidCursor,
    /// Sunucu tarafı hata. Detay yanıtta değil, logda.
    Internal,
}

impl ErrorCode {
    /// Bu hata koduna karşılık gelen HTTP durum kodu.
    ///
    /// `http` crate'ine bağımlı olmamak için düz `u16` döner — bu crate'i
    /// SDK'lar ve CLI de kullanıyor, onlara HTTP bağımlılığı dayatmıyoruz.
    #[must_use]
    pub const fn http_status(self) -> u16 {
        match self {
            Self::ValidationFailed | Self::InvalidCursor => 400,
            Self::MissingCredentials | Self::InvalidKey => 401,
            Self::Forbidden | Self::Banned => 403,
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::Gone => 410,
            Self::UnsupportedMedia => 415,
            Self::RateLimited => 429,
            Self::Internal => 500,
        }
    }
}
