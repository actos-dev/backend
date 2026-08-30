//! Sağlık ve hazırlık kontrolleri.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;

use crate::state::AppState;

/// Liveness: süreç ayakta mı?
///
/// Bağımlılıkları **bilerek** kontrol etmez. Veritabanı düştüğünde
/// orkestratörün süreci yeniden başlatması işe yaramaz, sadece gereksiz
/// restart döngüsü yaratır.
pub async fn live() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

#[derive(Debug, Serialize)]
struct Readiness {
    status: &'static str,
    database: Check,
    redis: Check,
    storage: Check,
}

#[derive(Debug, Serialize)]
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

/// Readiness: trafiği karşılamaya hazır mı?
///
/// Üç bağımlılığı da paralel yoklar. Biri bile düşükse 503 döner ki
/// yük dengeleyici bu instance'a istek yönlendirmesin.
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
