//! Kimlik doğrulama rotaları: kayıt, whoami, API key yönetimi, kurtarma.
//!
//! Burada iş mantığı yok — her handler `actos_core::auth`'un bir
//! fonksiyonunu çağırır, sonucu HTTP'ye (durum kodu, header, gövde) çevirir.
//! Actor id'leri yanıtta **her zaman** [`IdCodec`] ile kodlanmış döner; ham
//! `bigint` asla dışarı sızmaz. Sırlar (`api_key`, `recovery_codes`)
//! yalnızca yanıt gövdesinde taşınır, hiçbir `tracing` çağrısına geçirilmez.

use actos_core::{
    Error,
    auth::{self as core_auth, ActorRecord, ActorType, AdminRole, ApiKeyRecord},
    id::{Actor as ActorIdKind, IdCodec},
};
use actos_types::auth::{
    ActorSummary, ApiKeySummary, CreateKeyRequest, CreateKeyResponse, ListKeysResponse,
    RecoverRequest, RecoverResponse, RegenerateRecoveryCodesResponse, RegisterRequest,
    RegisterResponse, WhoamiResponse,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use uuid::Uuid;

use crate::{auth::CurrentActor, error::ApiError, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/whoami", get(whoami))
        .route("/auth/keys", post(create_key).get(list_keys))
        .route("/auth/keys/{key_id}", delete(revoke_key))
        .route("/auth/recover", post(recover))
        .route(
            "/auth/recovery-codes/regenerate",
            post(regenerate_recovery_codes),
        )
}

// --- Ortak dönüşümler --------------------------------------------------

/// `actor_type` string'ini domain enum'una çevirir.
///
/// `actos-types::RegisterRequest::actor_type` bilerek `String` (bkz.
/// `actos-types/src/auth.rs`'in kök yorumu — o crate `actos-core`'a bağımlı
/// olamıyor), burada domain tipine çevrilir.
pub(crate) fn parse_actor_type(raw: &str) -> Result<ActorType, Error> {
    match raw {
        "human" => Ok(ActorType::Human),
        "ai_agent" => Ok(ActorType::AiAgent),
        "system_bot" => Ok(ActorType::SystemBot),
        "organization" => Ok(ActorType::Organization),
        other => Err(Error::Validation(format!(
            "geçersiz actor_type: \"{other}\" (human, ai_agent, system_bot, organization olmalı)"
        ))),
    }
}

pub(crate) const fn actor_type_str(t: ActorType) -> &'static str {
    match t {
        ActorType::Human => "human",
        ActorType::AiAgent => "ai_agent",
        ActorType::SystemBot => "system_bot",
        ActorType::Organization => "organization",
    }
}

const fn admin_role_str(r: AdminRole) -> &'static str {
    match r {
        AdminRole::Admin => "admin",
        AdminRole::Moderator => "moderator",
    }
}

/// İç `bigint` actor id'sini dış base62 ID'ye kodlar.
///
/// Burada hata pratikte hiç oluşmaz (veritabanından gelen id her zaman
/// geçerli, negatif olmayan bir `bigint`'tir) — yine de panikle değil,
/// `Error::Internal` ile karşılanır: bu, çağıranın değil sunucunun bir
/// tutarsızlığı olurdu (`Error::Validation` yanlış olur, kullanıcı burada
/// hiçbir şey yanlış yapmadı).
pub(crate) fn encode_actor_id(id_codec: &IdCodec, internal: i64) -> Result<String, Error> {
    id_codec
        .encode::<ActorIdKind>(internal)
        .map_err(|e| Error::Internal(format!("actor id kodlanamadı: {e}")))
}

pub(crate) fn actor_summary(
    actor: &ActorRecord,
    id_codec: &IdCodec,
) -> Result<ActorSummary, Error> {
    Ok(ActorSummary {
        id: encode_actor_id(id_codec, actor.id)?,
        username: actor.username.clone(),
        actor_type: actor_type_str(actor.actor_type).to_owned(),
        display_name: actor.display_name.clone(),
        bio: actor.bio.clone(),
        created_at: actor.created_at.to_rfc3339(),
    })
}

fn key_summary(key: &ApiKeyRecord) -> ApiKeySummary {
    ApiKeySummary {
        id: key.id.to_string(),
        label: key.label.clone(),
        created_at: key.created_at.to_rfc3339(),
        last_used_at: key.last_used_at.map(|t| t.to_rfc3339()),
        revoked_at: key.revoked_at.map(|t| t.to_rfc3339()),
    }
}

// --- Handler'lar ---------------------------------------------------------

/// `POST /auth/register` → `201` + `Location: /actors/{username}`.
async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> Result<Response, ApiError> {
    let actor_type = parse_actor_type(&req.actor_type)
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let registration = core_auth::register(
        state.db(),
        &req.username,
        actor_type,
        req.display_name.as_deref(),
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let actor = actor_summary(&registration.actor, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let location = format!("/actors/{}", actor.username);
    let body = RegisterResponse {
        actor,
        api_key: registration.api_key,
        recovery_codes: registration.recovery_codes,
    };

    let mut response = (StatusCode::CREATED, Json(body)).into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

/// `GET /auth/whoami` → `200`.
async fn whoami(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<WhoamiResponse>, ApiError> {
    let keys = core_auth::list_keys(state.db(), current.actor.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    // İsteği doğrulamakta kullanılan key, bu actor'ün kendi `list_keys`
    // sonucunda mutlaka bulunur — `authenticate` zaten aynı key üzerinden
    // geçti. Bulunamaması, çağıranın değil sunucunun bir tutarsızlığıdır.
    let key = keys
        .into_iter()
        .find(|k| k.id == current.key_id)
        .ok_or_else(|| {
            ApiError::new(Error::Internal(
                "doğrulanan key, actor'ün key listesinde bulunamadı".to_owned(),
            ))
            .with_request_id(&headers)
        })?;

    let actor = actor_summary(&current.actor, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let roles = current
        .roles
        .iter()
        .map(|r| admin_role_str(*r).to_owned())
        .collect();

    Ok(Json(WhoamiResponse {
        actor,
        roles,
        key: key_summary(&key),
    }))
}

/// `POST /auth/keys` → `201`.
async fn create_key(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateKeyRequest>,
) -> Result<Response, ApiError> {
    let (record, plaintext) =
        core_auth::issue_key(state.db(), current.actor.id, req.label.as_deref())
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let body = CreateKeyResponse {
        key: key_summary(&record),
        api_key: plaintext,
    };
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

/// `GET /auth/keys` → `200`. Secret hiçbir zaman listede yer almaz.
async fn list_keys(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ListKeysResponse>, ApiError> {
    let keys = core_auth::list_keys(state.db(), current.actor.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(ListKeysResponse {
        keys: keys.iter().map(key_summary).collect(),
    }))
}

/// `DELETE /auth/keys/{key_id}` → `204`.
///
/// `key_id` ham UUID string'i olarak ayrıştırılır (base62 değil — zaten
/// rastgele bir UUID, numaralandırma riski yok). Ayrıştırılamıyorsa
/// [`Error::NotFound`] dönülür, [`Error::Validation`] değil: "bu biçim
/// geçerli ama böyle bir key yok" ile "biçim bozuk" ayrımı saldırgana bilgi
/// verirdi.
async fn revoke_key(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(key_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let key_id = Uuid::parse_str(&key_id)
        .map_err(|_| ApiError::new(Error::NotFound("API key")).with_request_id(&headers))?;

    core_auth::revoke_key(state.db(), current.actor.id, key_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /auth/recover` → `200`.
async fn recover(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RecoverRequest>,
) -> Result<Json<RecoverResponse>, ApiError> {
    let (api_key, remaining_recovery_codes) =
        core_auth::recover(state.db(), &req.username, &req.recovery_code)
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(RecoverResponse {
        api_key,
        remaining_recovery_codes,
    }))
}

/// `POST /auth/recovery-codes/regenerate` → `200`.
async fn regenerate_recovery_codes(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<RegenerateRecoveryCodesResponse>, ApiError> {
    let recovery_codes = core_auth::regenerate_recovery_codes(state.db(), current.actor.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(RegenerateRecoveryCodesResponse { recovery_codes }))
}
