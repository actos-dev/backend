//! Sürüm bilgisi.
//!
//! **Hız sınırından ve kimlik doğrulamadan muaf** — bkz.
//! `crate::routes::health` modül dokümantasyonundaki aynı gerekçe.

use axum::{Json, response::IntoResponse};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
struct Version {
    name: &'static str,
    version: &'static str,
    git_sha: &'static str,
    /// Hangi API sürümüyle konuştuğunu istemcinin bilmesi için.
    api_version: &'static str,
}

/// `GET /version` → `200`.
#[utoipa::path(
    get,
    path = "/version",
    tag = "meta",
    summary = "Sürüm bilgisi",
    responses(
        (status = 200, description = "Sunucu sürümü ve konuşulan API sürümü", body = Version),
    )
)]
pub async fn version() -> impl IntoResponse {
    Json(Version {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        git_sha: env!("ACTOS_GIT_SHA"),
        api_version: "v1",
    })
}
