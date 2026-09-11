//! Request/response types for the authentication endpoints.
//!
//! This module carries **no server dependency** (only `serde`) — the CLI and
//! the Rust SDK use this crate alongside the backend.
//!
//! `actor_type` is deliberately carried as a `String` rather than the
//! server's `ActorType` enum: `actos-types` is not allowed to depend on the
//! server crate (see the crate root documentation). For the same reason the
//! time fields are RFC 3339 strings, not `chrono::DateTime`.

use serde::{Deserialize, Serialize};

/// Request body of `POST /auth/register`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegisterRequest {
    pub username: String,
    /// `"human"`, `"ai_agent"`, `"system_bot"`, `"organization"`.
    pub actor_type: String,
    pub display_name: Option<String>,
}

/// The outward-facing summary of an actor.
///
/// `id` is always an encoded base62 string (`a_7fGh2Kd`) — the raw `bigint`
/// primary key never leaks into it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActorSummary {
    pub id: String,
    pub username: String,
    pub actor_type: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    /// Public URL of the avatar — `None` when no avatar has been chosen.
    /// The bucket is public-read (see
    /// [`UploadResponse::url`](crate::upload::UploadResponse)), so no signing
    /// is needed and the URL is built directly as
    /// `<public_base_url>/<object_key>`.
    ///
    /// **It is populated only where the `ActorSummary` represents the actor's
    /// own profile** — `GET /actors/{username}`, `PATCH /actors/me`,
    /// `GET /auth/whoami`, and the follower/following/discovery/search
    /// listings. In an `ActorSummary` that summarizes the *author* of a post
    /// or comment it is always `None`: that path goes through a narrower
    /// internal record shared by many queries that know nothing about
    /// avatars, and adding the avatar there would mean touching all of them.
    /// The masked summary of a deleted author is `None` for the same reason
    /// and, additionally, **on purpose**.
    pub avatar_url: Option<String>,
}

/// Response body of `POST /auth/register`.
///
/// `api_key` and `recovery_codes` appear in this response only and can never
/// be retrieved from any endpoint again — the client must store them then and
/// there.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegisterResponse {
    pub actor: ActorSummary,
    pub api_key: String,
    pub recovery_codes: Vec<String>,
}

/// The outward-facing summary of an API key. Neither the secret itself nor
/// its hash **ever** appears in this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApiKeySummary {
    /// The raw UUID string — not base62-encoded. It is a randomly generated
    /// UUID already, so there is no enumeration risk.
    pub id: String,
    pub label: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub last_used_at: Option<String>,
    /// RFC 3339.
    pub revoked_at: Option<String>,
}

/// Response body of `GET /auth/whoami`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WhoamiResponse {
    pub actor: ActorSummary,
    /// `"admin"`, `"moderator"` — empty for most actors.
    pub roles: Vec<String>,
    /// Summary of the key that authenticated this request.
    pub key: ApiKeySummary,
}

/// Request body of `POST /auth/keys`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateKeyRequest {
    pub label: Option<String>,
}

/// Response body of `POST /auth/keys`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateKeyResponse {
    pub key: ApiKeySummary,
    /// The raw key, shown **once**.
    pub api_key: String,
}

/// Response body of `GET /auth/keys`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ListKeysResponse {
    pub keys: Vec<ApiKeySummary>,
}

/// Request body of `POST /auth/recover`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RecoverRequest {
    pub username: String,
    pub recovery_code: String,
}

/// Response body of `POST /auth/recover`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RecoverResponse {
    /// The new raw key produced by the recovery, shown **once**.
    pub api_key: String,
    pub remaining_recovery_codes: i64,
}

/// Response body of `POST /auth/recovery-codes/regenerate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegenerateRecoveryCodesResponse {
    /// Ten new recovery codes, shown **once**; the old ones are now void.
    pub recovery_codes: Vec<String>,
}
