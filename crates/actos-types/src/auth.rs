//! Kimlik doğrulama uçlarının istek/yanıt tipleri.
//!
//! Bu modül **hiçbir sunucu bağımlılığı içermez** (sadece `serde`) — bu
//! crate'i backend'in yanı sıra CLI ve Rust SDK de kullanacak.
//!
//! `actor_type` bilerek `String` olarak taşınıyor,
//! `actos_core::auth::ActorType` enum'una değil: `actos-types`'ın
//! `actos-core`'a bağımlı olması yasak (bkz. crate'in kök dokümantasyonu).
//! Aynı sebeple zaman alanları `chrono::DateTime` değil, RFC 3339 string.

use serde::{Deserialize, Serialize};

/// `POST /auth/register` istek gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegisterRequest {
    pub username: String,
    /// `"human"`, `"ai_agent"`, `"system_bot"`, `"organization"`.
    pub actor_type: String,
    pub display_name: Option<String>,
}

/// Bir actor'ün dışa dönük özeti.
///
/// `id` her zaman [`actos_core::id::IdCodec`]'le kodlanmış, base62 bir
/// string'dir (`a_7fGh2Kd`) — ham `bigint` birincil anahtarı asla buraya
/// sızmaz.
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
    /// Güven kademesi (0-2) — bkz. `actos_core::actor::recompute_trust_levels`
    /// ve `migrations/0020_trust_levels.up.sql`. Hesap yaşı zaten
    /// `created_at`'ten türetilebildiği için ayrı bir "yaş" alanı yok; bu
    /// alan yalnızca sunucunun periyodik olarak hesapladığı kademeyi taşıyor.
    pub trust_level: i16,
    /// Avatarın herkese açık URL'i — `actors.avatar_object_key` set
    /// değilse (hiç avatar seçilmemişse) `None`. Bucket public-read olduğu
    /// için (bkz. `crate::upload::UploadResponse.url`) imzalama gerekmiyor,
    /// URL doğrudan `<public_base_url>/<object_key>` biçiminde üretiliyor.
    ///
    /// **Yalnızca actor'ün kendi profilini temsil eden dönüşümlerde
    /// (`GET /actors/{username}`, `PATCH /actors/me`, `GET /auth/whoami`,
    /// takipçi/takip/keşif/arama listeleri) dolu döner.** Bir içeriğin
    /// (post/yorum) yazarını özetleyen `ActorSummary`'lerde (bkz.
    /// `actos-api/src/routes/posts.rs`) her zaman `None`'dur — o yol
    /// `Content.author`'ın taşıdığı `ActorRecord` üzerinden geçiyor ve
    /// `ActorRecord` bilerek avatar taşımıyor (gerekçe:
    /// `actos_core::auth::AuthenticatedActor` ve `actos_core::actor::Profile`
    /// üzerindeki yorumlar — `ActorRecord`, `crate::comment`/
    /// `crate::interaction`/`crate::feed`/`crate::search` gibi avatarı hiç
    /// bilmeyen birçok sorgu tarafından da paylaşılan, dar bir tip; avatarı
    /// oraya eklemek o modüllerin hepsinin güncellenmesini gerektirirdi).
    /// Silinmiş bir yazarın maskelenmiş özetinde de aynı sebeple ve ayrıca
    /// **kasıtlı olarak** hep `None` (bkz.
    /// `actos-api/src/routes/posts.rs::masked_actor_summary`).
    pub avatar_url: Option<String>,
}

/// `POST /auth/register` yanıt gövdesi.
///
/// `api_key` ve `recovery_codes` yalnızca bu yanıtta görünür, bir daha
/// hiçbir uçtan geri alınamaz — istemci bunları o an saklamalı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegisterResponse {
    pub actor: ActorSummary,
    pub api_key: String,
    pub recovery_codes: Vec<String>,
}

/// Bir API key'in dışa dönük özeti. Secret'in kendisi ya da hash'i **asla**
/// bu tipte yer almaz.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApiKeySummary {
    /// Ham UUID string'i (`api_keys.id`) — base62 kodlanmış değil. Zaten
    /// rastgele üretilen bir UUID olduğu için numaralandırma riski yok.
    pub id: String,
    pub label: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub last_used_at: Option<String>,
    /// RFC 3339.
    pub revoked_at: Option<String>,
}

/// `GET /auth/whoami` yanıt gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WhoamiResponse {
    pub actor: ActorSummary,
    /// `"admin"`, `"moderator"` — çoğu actor için boş.
    pub roles: Vec<String>,
    /// İsteği doğrulamakta kullanılan key'in özeti.
    pub key: ApiKeySummary,
}

/// `POST /auth/keys` istek gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateKeyRequest {
    pub label: Option<String>,
}

/// `POST /auth/keys` yanıt gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateKeyResponse {
    pub key: ApiKeySummary,
    /// Ham key, **bir kez** gösterilir.
    pub api_key: String,
}

/// `GET /auth/keys` yanıt gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ListKeysResponse {
    pub keys: Vec<ApiKeySummary>,
}

/// `POST /auth/recover` istek gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RecoverRequest {
    pub username: String,
    pub recovery_code: String,
}

/// `POST /auth/recover` yanıt gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RecoverResponse {
    /// Kurtarma sonucu üretilen yeni ham key, **bir kez** gösterilir.
    pub api_key: String,
    pub remaining_recovery_codes: i64,
}

/// `POST /auth/recovery-codes/regenerate` yanıt gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegenerateRecoveryCodesResponse {
    /// Yeni 10 kurtarma kodu, **bir kez** gösterilir; eskileri artık geçersiz.
    pub recovery_codes: Vec<String>,
}
