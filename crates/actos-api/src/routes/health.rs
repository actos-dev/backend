//! Sağlık ve hazırlık kontrolleri.
//!
//! **Hız sınırından ve kimlik doğrulamadan muaf** (bkz.
//! `crate::middleware::ratelimit::classify` ve `crate::routes` modül
//! dokümantasyonu): bir orkestratörün liveness/readiness probe'u öngörülebilir
//! aralıklarla istek atar, hız sınırına takılıp sağlıklı bir instance'ı
//! "unhealthy" göstermemeli.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use utoipa::ToSchema;

use crate::state::AppState;

/// `GET /health` → `200`. Liveness: süreç ayakta mı?
///
/// Bağımlılıkları **bilerek** kontrol etmez. Veritabanı düştüğünde
/// orkestratörün süreci yeniden başlatması işe yaramaz, sadece gereksiz
/// restart döngüsü yaratır.
#[utoipa::path(
    get,
    path = "/health",
    tag = "meta",
    summary = "Liveness kontrolü",
    description = "Süreç ayakta mı? Bağımlılıklara (DB/Redis/Storage) hiç bakmaz — bkz. handler dokümantasyonu.",
    responses(
        (status = 200, description = "Süreç ayakta", body = LivenessResponse,
            content_type = "application/json", example = json!({"status": "ok"})),
    )
)]
pub async fn live() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

/// `GET /health` yanıt şekli — yalnızca dokümantasyon için, handler
/// gerçekte `serde_json::json!` ile ham `Value` üretiyor (bkz. `live`).
#[derive(Debug, Serialize, ToSchema)]
struct LivenessResponse {
    status: String,
}

#[derive(Debug, Serialize, ToSchema)]
struct Readiness {
    status: &'static str,
    database: Check,
    redis: Check,
    storage: Check,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
enum Check {
    Up,
    Down { error: String },
}

impl Check {
    fn from_result<E: std::fmt::Display>(result: Result<(), E>) -> Self {
        match result {
            Ok(()) => Self::Up,
            Err(e) => Self::Down {
                error: e.to_string(),
            },
        }
    }

    const fn is_up(&self) -> bool {
        matches!(self, Self::Up)
    }
}

/// `GET /health/ready` → `200` (hepsi ayakta), `503` (en az biri düşük).
///
/// Readiness: trafiği karşılamaya hazır mı? Üç bağımlılığı da paralel yoklar.
#[utoipa::path(
    get,
    path = "/health/ready",
    tag = "meta",
    summary = "Readiness kontrolü",
    description = "Veritabanı, Redis ve nesne depolamayı paralel yoklar; biri bile düşükse 503 döner ki \
        yük dengeleyici bu instance'a istek yönlendirmesin.",
    responses(
        (status = 200, description = "Üçü de ayakta", body = Readiness),
        (status = 503, description = "En az bir bağımlılık düşük", body = Readiness),
    )
)]
pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let (db, redis, storage) = tokio::join!(
        actos_core::db::ping(state.db()),
        actos_core::cache::ping(state.redis()),
        state.storage().ping(),
    );

    let body = Readiness {
        status: "",
        database: Check::from_result(db),
        redis: Check::from_result(redis),
        storage: Check::from_result(storage),
    };

    let all_up = body.database.is_up() && body.redis.is_up() && body.storage.is_up();
    let body = Readiness {
        status: if all_up { "ready" } else { "degraded" },
        ..body
    };

    let status = if all_up {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status, Json(body))
}
