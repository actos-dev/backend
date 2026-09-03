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
    summary = "Liveness check",
    description = "Is the process up? Never looks at dependencies (DB/Redis/Storage) — see the handler documentation.",
    responses(
        (status = 200, description = "Process is up", body = LivenessResponse,
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
    summary = "Readiness check",
    description = "Polls the database, Redis, and object storage in parallel; returns 503 if even one \
        is down, so a load balancer stops routing traffic to this instance.",
    responses(
        (status = 200, description = "All three are up", body = Readiness),
        (status = 503, description = "At least one dependency is down", body = Readiness),
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
