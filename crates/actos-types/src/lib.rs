//! Shared types of the Actos API.
//!
//! This crate carries **no mandatory** server dependency (no database, no
//! HTTP framework) — because the CLI, the TUI and the Rust SDK use the same
//! types. When the shape of the API changes, all of them break at compile
//! time, so nobody has to track the synchronization by hand.
//!
//! ## The `openapi` feature — the principle holds, it stays optional
//!
//! The OpenAPI spec of `actos-api` needs a `utoipa::ToSchema` derive on every
//! DTO, and that derive requires a compile-time dependency on `utoipa`.
//! Adding it to this crate unconditionally would break the principle above:
//! consumers with nothing to do with an HTTP server — the CLI, the TUI, the
//! SDK — would have to compile `utoipa` and its proc-macro dependencies
//! (`syn`, `quote`, ...) for nothing.
//!
//! The solution: the `utoipa` dependency is `optional = true` and is pulled
//! in only when this crate's `openapi` feature is enabled (see `Cargo.toml`).
//! The derive on each DTO is conditional to match:
//! ```ignore
//! #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
//! ```
//! With the feature off (the default — what the CLI and the SDK get) that
//! line leaves no trace and the type exists with `serde` alone. Only
//! `actos-api` enables the feature, so the principle — no longer "no server
//! dependency" but "no **mandatory** server dependency" — is enforced by the
//! feature flag itself.

/// Request/response types for actor profiles and the directory endpoints.
pub mod actor;

/// Request/response types for the authentication endpoints.
pub mod auth;

/// Request/response types for the content (post + comment) endpoints.
pub mod content;

/// Request/response types for the community endpoints.
pub mod community;

/// Request/response types for the vote, follow and save endpoints.
pub mod interaction;

/// Request/response types for the report and admin endpoints.
pub mod moderation;

/// Request/response types for `GET /me/inbox` and the mark-as-read endpoints.
pub mod notification;

/// Response types for the file upload endpoints.
pub mod upload;

/// Machine-readable error codes.
///
/// More important than the human-readable message for an AI agent handling
/// errors programmatically: the message text may change, these codes do not.
pub mod error;

/// Response types for the `GET /search` endpoints.
pub mod search;

/// Response types for the tag endpoints.
pub mod tag;

pub use error::ErrorCode;
