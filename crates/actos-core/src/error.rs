use actos_types::ErrorCode;

/// Actos'un domain hata tipi.
///
/// HTTP'yi bilmez — taşıma katmanı bunu kendi yanıt biçimine çevirir
/// (bkz. `actos-api::error::ApiError`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("doğrulama başarısız: {0}")]
    Validation(String),

    #[error("kimlik bilgisi sunulmadı")]
    MissingCredentials,

    #[error("API anahtarı geçersiz veya iptal edilmiş")]
    InvalidKey,

    #[error("bu eylem için yetkin yok")]
    Forbidden,

    #[error("hesap askıya alınmış")]
    Banned,

    #[error("{0} bulunamadı")]
    NotFound(&'static str),

    #[error("{0} silinmiş")]
    Gone(&'static str),

    #[error("çakışma: {0}")]
    Conflict(String),

    #[error("hız limiti aşıldı")]
    RateLimited { retry_after_secs: u64 },

    #[error("kabul edilmeyen dosya: {0}")]
    UnsupportedMedia(String),

    #[error("sayfalama cursor'ı geçersiz")]
    InvalidCursor,

    // --- Aşağıdakiler istemciye asla detaylandırılmaz, sadece loglanır ---
    #[error("veritabanı hatası")]
    Database(#[from] sqlx::Error),

    #[error("iç hata: {0}")]
    Internal(String),
}

impl Error {
    /// Bu hatanın makine-okunur kodu.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::Validation(_) => ErrorCode::ValidationFailed,
            Self::MissingCredentials => ErrorCode::MissingCredentials,
            Self::InvalidKey => ErrorCode::InvalidKey,
            Self::Forbidden => ErrorCode::Forbidden,
            Self::Banned => ErrorCode::Banned,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Gone(_) => ErrorCode::Gone,
            Self::Conflict(_) => ErrorCode::Conflict,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::UnsupportedMedia(_) => ErrorCode::UnsupportedMedia,
            Self::InvalidCursor => ErrorCode::InvalidCursor,
            Self::Database(_) | Self::Internal(_) => ErrorCode::Internal,
        }
    }

    /// İstemciye gösterilebilecek açıklama.
    ///
    /// `None` dönerse yanıtta detay yer almaz: iç hataların (SQL metni, dosya
    /// yolu, bağlantı dizesi...) dışarı sızmaması bilinçli bir karardır.
    /// O bilgi loga gider, gövdeye değil.
    #[must_use]
    pub fn public_detail(&self) -> Option<String> {
        match self {
            Self::Database(_) | Self::Internal(_) => None,
            other => Some(other.to_string()),
        }
    }

    /// Sunucu tarafı hatası mı? (loglama seviyesini bu belirler)
    #[must_use]
    pub const fn is_internal(&self) -> bool {
        matches!(self, Self::Database(_) | Self::Internal(_))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
