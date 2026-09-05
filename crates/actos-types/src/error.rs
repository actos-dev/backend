use serde::{Deserialize, Serialize};

/// Machine-readable error codes the API can return.
///
/// Carried as a string in the `code` field of the response body
/// (`"RATE_LIMITED"`). This list is a contract: the meaning of an existing
/// code is never changed, only new ones are added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
// Deliberately not `non_exhaustive`: when a new code is added we want the
// `match`es that map it to an HTTP status and title to stop compiling.
// Adding a code is a change to the API contract anyway.
pub enum ErrorCode {
    /// The request body or parameters failed validation.
    ValidationFailed,
    /// The `Authorization` header is missing or malformed.
    MissingCredentials,
    /// The API key is invalid, revoked or unknown.
    InvalidKey,
    /// Authenticated, but not authorized for this action.
    Forbidden,
    /// The account is suspended.
    Banned,
    /// The resource does not exist.
    NotFound,
    /// The resource existed and was deleted.
    Gone,
    /// Uniqueness violation (username taken, same report filed twice, ...).
    Conflict,
    /// Rate limit exceeded — see the `Retry-After` header.
    RateLimited,
    /// The uploaded file was rejected (type, size or content validation).
    UnsupportedMedia,
    /// The pagination cursor is malformed or belongs to a different sort.
    InvalidCursor,
    /// Server-side error. The detail is in the log, not in the response.
    Internal,
}

impl ErrorCode {
    /// The HTTP status code corresponding to this error code.
    ///
    /// Returns a plain `u16` so that this crate does not depend on the `http`
    /// crate — the SDKs and the CLI use it too, and we do not force an HTTP
    /// dependency on them.
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
