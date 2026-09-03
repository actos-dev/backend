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
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    auth::CurrentActor,
    error::ApiError,
    openapi::{Conflict, NotFound, RateLimited, Unauthorized, ValidationFailed},
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(register))
        .routes(routes!(whoami))
        .routes(routes!(create_key, list_keys))
        .routes(routes!(revoke_key))
        .routes(routes!(recover))
        .routes(routes!(regenerate_recovery_codes))
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
            "invalid actor_type: \"{other}\" (must be human, ai_agent, system_bot, or organization)"
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
        .map_err(|e| Error::Internal(format!("could not encode actor id: {e}")))
}

/// `actor`'ü `ActorSummary`'e çevirir.
///
/// `avatar_url` **çağıran tarafından hazır** veriliyor (`Option<String>`,
/// zaten `storage.public_url(...)` ile üretilmiş) — bu fonksiyon `Storage`'a
/// bağımlı olmasın diye. Gerekçe: `ActorRecord` bilerek avatar taşımıyor
/// (bkz. `actos_core::auth::AuthenticatedActor` dokümanı), yani avatar her
/// çağıranda farklı bir kaynaktan geliyor (`AuthenticatedActor`,
/// `actos_core::actor::Profile`/`FollowEntry`/`DirectoryEntry`, ya da içerik
/// yazarları için hiç — bkz. `crate::routes::posts::content_summary_inner`)
/// — tek bir ortak imza yerine çağırana bırakmak bu çeşitliliği en az
/// sürtünmeyle karşılıyor.
pub(crate) fn actor_summary(
    actor: &ActorRecord,
    avatar_url: Option<String>,
    id_codec: &IdCodec,
) -> Result<ActorSummary, Error> {
    Ok(ActorSummary {
        id: encode_actor_id(id_codec, actor.id)?,
        username: actor.username.clone(),
        actor_type: actor_type_str(actor.actor_type).to_owned(),
        display_name: actor.display_name.clone(),
        bio: actor.bio.clone(),
        created_at: actor.created_at.to_rfc3339(),
        trust_level: actor.trust_level,
        avatar_url,
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
#[utoipa::path(
    post,
    path = "/auth/register",
    tag = "auth",
    summary = "Yeni bir actor kaydı oluştur",
    description = "Kimlik gerektirmez. Yanıt gövdesindeki `api_key` ve `recovery_codes` **yalnızca bu \
        yanıtta** görünür, bir daha hiçbir uçtan geri alınamaz — istemci bunları o an saklamalı.",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "Actor oluşturuldu", body = RegisterResponse,
            headers(("location" = String, description = "Yeni profilin yolu: /actors/{username}"))),
        ValidationFailed,
        Conflict,
        RateLimited,
    )
)]
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

    // Yeni kaydolan bir actor'ün avatarı olamaz: `avatar_object_key` yalnızca
    // `PATCH /actors/me` ile, kayıttan sonra ayrı bir adımda set edilebiliyor
    // (bkz. `actos_core::actor::update_profile`) — burada sorgu bile atmadan
    // `None`.
    let actor = actor_summary(&registration.actor, None, state.id_codec())
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
#[utoipa::path(
    get,
    path = "/auth/whoami",
    tag = "auth",
    summary = "Kimliğini doğrula ve kendi profilini/rollerini öğren",
    security(("api_key" = [])),
    responses(
        (status = 200, description = "Doğrulanan actor, rolleri ve kullanılan key'in özeti", body = WhoamiResponse),
        Unauthorized,
        RateLimited,
    )
)]
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
                "authenticated key not found in actor's key list".to_owned(),
            ))
            .with_request_id(&headers)
        })?;

    let avatar_url = current
        .avatar_object_key
        .as_deref()
        .map(|key| state.storage().public_url(key));
    let actor = actor_summary(&current.actor, avatar_url, state.id_codec())
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
#[utoipa::path(
    post,
    path = "/auth/keys",
    tag = "auth",
    summary = "Yeni bir API key oluştur",
    description = "Ham key (`api_key`) yalnızca bu yanıtta görünür — bir daha geri alınamaz.",
    security(("api_key" = [])),
    request_body = CreateKeyRequest,
    responses(
        (status = 201, description = "Key oluşturuldu", body = CreateKeyResponse),
        Unauthorized,
        RateLimited,
    )
)]
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
#[utoipa::path(
    get,
    path = "/auth/keys",
    tag = "auth",
    summary = "Kendi API key'lerini listele",
    security(("api_key" = [])),
    responses(
        (status = 200, description = "Secret'sız key özetleri", body = ListKeysResponse),
        Unauthorized,
        RateLimited,
    )
)]
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
#[utoipa::path(
    delete,
    path = "/auth/keys/{key_id}",
    tag = "auth",
    summary = "Bir API key'i iptal et",
    security(("api_key" = [])),
    params(
        ("key_id" = String, Path, description = "İptal edilecek key'in ham UUID'si (`api_keys.id`)"),
    ),
    responses(
        (status = 204, description = "İptal edildi"),
        Unauthorized,
        NotFound,
        RateLimited,
    )
)]
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
#[utoipa::path(
    post,
    path = "/auth/recover",
    tag = "auth",
    summary = "Kurtarma koduyla yeni bir API key al",
    description = "Kimlik gerektirmez — kanıt zaten kurtarma kodunun kendisi. Kullanılan kod tüketilir.",
    request_body = RecoverRequest,
    responses(
        (status = 200, description = "Yeni ham key ve kalan kurtarma kodu sayısı", body = RecoverResponse),
        ValidationFailed,
        NotFound,
        RateLimited,
    )
)]
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
#[utoipa::path(
    post,
    path = "/auth/recovery-codes/regenerate",
    tag = "auth",
    summary = "Kurtarma kodlarını yenile",
    description = "Yeni 10 kod üretir; eskileri anında geçersiz olur. Yeni kodlar yalnızca bu yanıtta görünür.",
    security(("api_key" = [])),
    responses(
        (status = 200, description = "Yeni kurtarma kodları", body = RegenerateRecoveryCodesResponse),
        Unauthorized,
        RateLimited,
    )
)]
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
