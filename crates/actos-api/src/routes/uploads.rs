//! Dosya yükleme rotaları.
//!
//! `POST /uploads` multipart alıyor; doğrulama, normalize etme ve depolama
//! `actos_core::media` ile `actos_core::attachment`'ta. Bu dosya yalnızca
//! multipart'ı okuyup HTTP'ye çeviriyor.
//!
//! **Gövde sınırı burada, genel `RequestBodyLimit` katmanından ayrı:**
//! `crate::app`'teki katman bütün uçlara `max_body_bytes` (1 MB)
//! uyguluyor; yükleme ucu 8 MB kabul ediyor. Sınır alan okunurken
//! uygulanıyor, yani 8 MB'ı aşan bir gövde belleğe tamamen alınmadan
//! reddediliyor.

use actos_core::{Error, attachment as core_attachment, id::IdCodec};
use actos_types::upload::UploadResponse;
use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::CurrentActor,
    error::ApiError,
    openapi::{Forbidden, NotFound, RateLimited, Unauthorized, UnsupportedMedia, ValidationFailed},
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_upload))
        .routes(routes!(delete_upload))
}

/// Yüklemenin beklendiği multipart alan adı.
const FILE_FIELD: &str = "file";

/// `POST /uploads` istek gövdesinin dokümantasyon amaçlı şeması.
///
/// Gerçek ayrıştırma `axum::extract::Multipart` ile elle yapılıyor (bkz.
/// `create_upload`) — bu struct hiç örneklenmiyor, yalnızca OpenAPI spec'inin
/// `multipart/form-data` gövdesini tarif edebilmesi için var.
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
struct UploadRequestBody {
    /// Yüklenecek görsel dosyası. Kabul edilen biçimler: jpeg, png, gif, webp
    /// (magic byte ile tespit edilir, uzantı/`Content-Type`'a güvenilmez).
    #[schema(content_media_type = "application/octet-stream")]
    file: Vec<u8>,
}

/// `POST /uploads` → `201`, `400` (alan yok / dosya bozuk / çok büyük),
/// `415` (desteklenmeyen biçim), `401`.
///
/// Yanıt, dosyanın **herkese açık URL'sini** de içeriyor: bucket public-read
/// olduğu için istemci ek bir çağrı yapmadan görseli gösterebiliyor.
///
/// Dönen `id` bir sonraki adımda `POST /posts`'un `attachment_ids` alanına
/// veriliyor — yükleme ile bağlama iki ayrı adım (gerekçe
/// `actos_core::attachment` modül dokümantasyonunda).
#[utoipa::path(
    post,
    path = "/uploads",
    tag = "uploads",
    summary = "Bir dosya yükle",
    description = "Multipart gövdede `file` alanı bekler. Yanıttaki `id`, `POST /posts`/`POST \
        /posts/{id}/comments`'ın `attachment_ids` alanına verilir.",
    security(("api_key" = [])),
    request_body(content = inline(UploadRequestBody), content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Yükleme kabul edildi, herkese açık URL ile birlikte", body = UploadResponse),
        ValidationFailed,
        Unauthorized,
        UnsupportedMedia,
        RateLimited,
    )
)]
async fn create_upload(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ApiError> {
    let max_bytes = state.config().server.max_upload_bytes;

    let mut bytes: Option<Vec<u8>> = None;

    while let Some(alan) = multipart.next_field().await.map_err(|e| {
        ApiError::new(Error::Validation(format!("multipart okunamadı: {e}")))
            .with_request_id(&headers)
    })? {
        // Yalnızca beklenen alan okunuyor; diğerleri sessizce atlanıyor
        // (istemci kütüphaneleri sık sık fazladan alan gönderiyor).
        if alan.name() != Some(FILE_FIELD) {
            continue;
        }

        let veri = alan.bytes().await.map_err(|e| {
            ApiError::new(Error::Validation(format!("dosya okunamadı: {e}")))
                .with_request_id(&headers)
        })?;

        if veri.len() > max_bytes {
            return Err(ApiError::new(Error::Validation(format!(
                "dosya çok büyük: {} bayt, sınır {max_bytes} bayt",
                veri.len()
            )))
            .with_request_id(&headers));
        }

        bytes = Some(veri.to_vec());
        break;
    }

    let Some(bytes) = bytes else {
        return Err(ApiError::new(Error::Validation(format!(
            "multipart gövdesinde \"{FILE_FIELD}\" alanı yok"
        )))
        .with_request_id(&headers));
    };

    let ek = core_attachment::create_attachment(
        state.db(),
        state.storage(),
        state.id_codec(),
        current.actor.id,
        &bytes,
        max_bytes,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let yanit = upload_response(&ek, state.id_codec(), state.storage())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok((StatusCode::CREATED, Json(yanit)))
}

/// `DELETE /uploads/{id}` → `204`, `403` (sahibi değil), `404`.
#[utoipa::path(
    delete,
    path = "/uploads/{id}",
    tag = "uploads",
    summary = "Bir yüklemeyi sil",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "Yüklemenin dış id'si"),
    ),
    responses(
        (status = 204, description = "Silindi"),
        Unauthorized,
        Forbidden,
        NotFound,
        RateLimited,
    )
)]
async fn delete_upload(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let ek_id = state
        .id_codec()
        .decode::<actos_core::id::Attachment>(&id)
        .map_err(|_| ApiError::new(Error::NotFound("attachment")).with_request_id(&headers))?;

    core_attachment::delete_attachment(state.db(), state.storage(), ek_id, current.actor.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// Bir [`core_attachment::Attachment`]'i yanıt DTO'suna çevirir.
///
/// `pub(crate)`: `crate::routes::posts` de bir içeriğin eklerini aynı
/// şekilde göstermek için kullanıyor.
pub(crate) fn upload_response(
    ek: &core_attachment::Attachment,
    id_codec: &IdCodec,
    storage: &actos_core::Storage,
) -> Result<UploadResponse, Error> {
    Ok(UploadResponse {
        id: id_codec.encode::<actos_core::id::Attachment>(ek.id)?,
        url: storage.public_url(&ek.object_key),
        thumbnail_url: storage.public_url(&ek.thumbnail_key()),
        mime_type: ek.mime_type.clone(),
        byte_size: ek.byte_size,
        width: ek.width,
        height: ek.height,
        checksum_sha256: ek.checksum_sha256.clone(),
        created_at: ek.created_at.to_rfc3339(),
    })
}
