//! Sürüm bilgisi.

use axum::{Json, response::IntoResponse};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Version {
    name: &'static str,
    version: &'static str,
    git_sha: &'static str,
    /// Hangi API sürümüyle konuştuğunu istemcinin bilmesi için.
    api_version: &'static str,
}

/// `GET /version`
pub async fn version() -> impl IntoResponse {
    Json(Version {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        git_sha: env!("ACTOS_GIT_SHA"),
        api_version: "v1",
    })
}
