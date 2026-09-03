use actos_types::ErrorCode;

/// Actos'un domain hata tipi.
///
/// HTTP'yi bilmez — taşıma katmanı bunu kendi yanıt biçimine çevirir
/// (bkz. `actos-api::error::ApiError`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("validation failed: {0}")]
    Validation(String),

    #[error("no credentials provided")]
    MissingCredentials,

    #[error("API key is invalid or revoked")]
    InvalidKey,

    #[error("you are not authorized to perform this action")]
    Forbidden,

    #[error("account is suspended")]
    Banned,

    #[error("{0} not found")]
    NotFound(&'static str),

    #[error("{0} has been deleted")]
    Gone(&'static str),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("rate limit exceeded")]
    RateLimited { retry_after_secs: u64 },

    #[error("unsupported file: {0}")]
    UnsupportedMedia(String),

    #[error("invalid pagination cursor")]
    InvalidCursor,

    // --- Aşağıdakiler istemciye asla detaylandırılmaz, sadece loglanır ---
    #[error("database error")]
    Database(#[from] sqlx::Error),

    #[error("internal error: {0}")]
    Internal(String),
}

impl From<crate::storage::StorageError> for Error {
    /// Depolama hatası istemcinin girdisiyle ilgili değil, sunucunun bir
    /// bağımlılığının erişilemez olmasıyla ilgili — bu yüzden
    /// [`Error::Internal`]. Mesaj yalnızca loglara gidiyor
    /// ([`Error::public_detail`] `Internal` için genel bir metin döner),
    /// yani MinIO'nun iç adresleri ya da hata ayrıntıları istemciye
    /// sızmıyor.
    fn from(err: crate::storage::StorageError) -> Self {
        Self::Internal(format!("storage: {err}"))
    }
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
