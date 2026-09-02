//! HTTP rotaları.

pub mod actors;
pub mod admin;
pub mod auth;
pub mod comments;
pub mod feed;
pub mod health;
pub mod interactions;
pub mod meta;
pub mod posts;
pub mod search;
pub mod tags;
pub mod uploads;

use axum::{Router, http::HeaderMap, routing::get};

use crate::{error::ApiError, state::AppState};

/// Uygulamanın rota ağacı. Katmanlar burada değil, `app` içinde eklenir.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::live))
        .route("/health/ready", get(health::ready))
        .route("/version", get(meta::version))
        .merge(auth::router())
        .merge(actors::router())
        .merge(posts::router())
        .merge(comments::router())
        .merge(tags::router())
        .merge(search::router())
        .merge(interactions::router())
        .merge(feed::router())
        .merge(uploads::router())
        .merge(admin::router())
        .fallback(not_found)
}

/// Eşleşmeyen rotalar için de aynı hata biçimi.
///
/// Varsayılan davranış boş gövdeli bir 404 döndürmek olurdu; istemcilerin
/// (özellikle ajanların) her hatayı tek bir şemayla ayrıştırabilmesi için
/// burada da `application/problem+json` üretiyoruz.
async fn not_found(headers: HeaderMap) -> ApiError {
    ApiError::new(actos_core::Error::NotFound("rota")).with_request_id(&headers)
}
