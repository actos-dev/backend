//! HTTP hata yanıtları — RFC 9457 (`application/problem+json`).

use std::sync::Arc;

use actos_core::Error;
use actos_types::ErrorCode;
use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::telemetry::REQUEST_ID_HEADER;

/// `actos_core::Error`'ı HTTP yanıtına çeviren sarmalayıcı.
///
/// Ayrı bir tip olmasının sebebi yalnızca yönelim kuralı (orphan rule) değil:
/// `actos-core`'un HTTP'den habersiz kalması bilinçli bir katman ayrımı.
///
/// **`inner` neden `Arc<Error>`, düz `Error` değil:** kimlik çözümleme
/// middleware'i (`crate::middleware::identity`) doğrulama hatasını istek
/// başına **bir kez** üretip request extension'ına koyuyor; o extension'ı
/// hem `CurrentActor` hem `OptionalActor` extractor'ı okuyabiliyor (bkz. o
/// modüllerin dokümantasyonu). `actos_core::Error` `Clone` değil (içinde
/// `sqlx::Error` var), bu yüzden extension'da paylaşılabilir tek biçim
/// `Arc<Error>` — `ApiError` de aynı türü taşıyarak ekstra bir kopyalama/
/// dönüştürme katmanına gerek bırakmıyor.
#[derive(Debug)]
pub struct ApiError {
    inner: Arc<Error>,
    request_id: Option<String>,
}

impl ApiError {
    #[must_use]
    pub fn new(inner: Error) -> Self {
        Self {
            inner: Arc::new(inner),
            request_id: None,
        }
    }

    /// [`Self::new`] ile aynı, ama zaten `Arc`'lanmış bir hatadan kurar —
    /// bkz. `inner` alanı üzerindeki yorum.
    #[must_use]
    pub const fn from_arc(inner: Arc<Error>) -> Self {
        Self {
            inner,
            request_id: None,
        }
    }

    /// Yanıta istek kimliğini iliştirir — kullanıcı bu kodu bize verdiğinde
    /// logda tam olarak o isteği bulabiliyoruz.
    #[must_use]
    pub fn with_request_id(mut self, headers: &HeaderMap) -> Self {
        self.request_id = headers
            .get(REQUEST_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        self
    }
}

impl From<Error> for ApiError {
    fn from(inner: Error) -> Self {
        Self::new(inner)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::new(Error::Database(err))
    }
}

/// RFC 9457 "problem details" gövdesi.
///
/// `pub(crate)` (özel değil): Faz 16'nın OpenAPI şeması bu tipi tek bir
/// bileşen (`components.schemas.ProblemDetails`) olarak her hata yanıtında
/// referans veriyor (bkz. `crate::openapi` modülü) — bunun için diğer
/// `routes/*.rs` dosyalarından görünür olması gerekiyor.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ProblemDetails {
    /// Hata tipini tanımlayan URI (dokümantasyona işaret eder).
    #[serde(rename = "type")]
    type_uri: String,
    /// Kısa, insan-okunur özet.
    title: String,
    /// HTTP durum kodu (gövdede de bulunması RFC'nin önerisi).
    status: u16,
    /// Bu spesifik oluşuma dair açıklama. İç hatalarda yok.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// Makine-okunur kod — istemciler `title` metnine değil buna bakmalı.
    code: ErrorCode,
    /// Destek/hata ayıklama için istek kimliği.
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = self.inner.code();
        let status =
            StatusCode::from_u16(code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        // İç hatalar tam detayıyla loglanır; istemci sadece "bir şeyler ters
        // gitti" görür. Bu ayrım güvenlik için, kolaylık için değil.
        if self.inner.is_internal() {
            tracing::error!(
                error = %self.inner,
                request_id = self.request_id.as_deref().unwrap_or("-"),
                "istek iç hatayla sonuçlandı"
            );
        } else {
            tracing::debug!(
                error = %self.inner,
                code = ?code,
                "istek hatayla sonuçlandı"
            );
        }

        let body = ProblemDetails {
            type_uri: format!("https://docs.actos.dev/errors/{}", slug(code)),
            title: title_for(code).to_owned(),
            status: status.as_u16(),
            detail: self.inner.public_detail(),
            code,
            request_id: self.request_id.clone(),
        };

        let mut response = (status, Json(body)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );

        // 429'da istemciye ne zaman tekrar deneyeceğini söylemek zorundayız —
        // özellikle otomatik ajanlar için bu tahmin edilecek bir şey olmamalı.
        // `self.inner` artık `Arc<Error>` olduğu için sahiplik alınamıyor —
        // referans üzerinden eşleniyor.
        if let Error::RateLimited { retry_after_secs } = self.inner.as_ref()
            && let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }

        response
    }
}

const fn title_for(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::ValidationFailed => "Input failed validation",
        ErrorCode::MissingCredentials => "No credentials provided",
        ErrorCode::InvalidKey => "API key is invalid",
        ErrorCode::Forbidden => "Not authorized",
        ErrorCode::Banned => "Account is suspended",
        ErrorCode::NotFound => "Not found",
        ErrorCode::Gone => "Deleted",
        ErrorCode::Conflict => "Conflict",
        ErrorCode::RateLimited => "Rate limit exceeded",
        ErrorCode::UnsupportedMedia => "File not accepted",
        ErrorCode::InvalidCursor => "Invalid pagination cursor",
        ErrorCode::Internal => "Server error",
    }
}

/// `ValidationFailed` -> `validation-failed`
fn slug(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_ascii_lowercase))
        .map(|s| s.replace('_', "-"))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    async fn body_of(err: ApiError) -> (StatusCode, serde_json::Value, HeaderMap) {
        let response = err.into_response();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap_or_default();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json, headers)
    }

    #[tokio::test]
    async fn istemci_hatasi_detay_icerir() {
        let (status, body, headers) = body_of(ApiError::new(Error::NotFound("post"))).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "NOT_FOUND");
        assert_eq!(body["status"], 404);
        assert_eq!(body["detail"], "post not found");
        assert_eq!(body["type"], "https://docs.actos.dev/errors/not-found");
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );
    }

    /// İç hataların detayı gövdeye sızmamalı — bu bir güvenlik sınırı.
    #[tokio::test]
    async fn ic_hata_detay_sizdirmaz() {
        let (status, body, _) = body_of(ApiError::new(Error::Internal(
            "could not connect to postgres://user:password@host/db".to_owned(),
        )))
        .await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["code"], "INTERNAL");
        assert!(body.get("detail").is_none(), "iç hata detayı sızdı: {body}");
        let raw = body.to_string();
        assert!(!raw.contains("password"), "sır sızdı: {raw}");
    }

    #[tokio::test]
    async fn hiz_limiti_retry_after_ekler() {
        let (status, body, headers) = body_of(ApiError::new(Error::RateLimited {
            retry_after_secs: 42,
        }))
        .await;

        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["code"], "RATE_LIMITED");
        assert_eq!(
            headers
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("42")
        );
    }

    #[test]
    fn her_hata_kodu_bir_baslik_ve_slug_uretir() {
        // Yeni bir ErrorCode eklenirse `title_for` derlenmez (non_exhaustive
        // değil); bu test de slug üretiminin bozulmadığını doğrular.
        for code in [
            ErrorCode::ValidationFailed,
            ErrorCode::MissingCredentials,
            ErrorCode::InvalidKey,
            ErrorCode::Forbidden,
            ErrorCode::Banned,
            ErrorCode::NotFound,
            ErrorCode::Gone,
            ErrorCode::Conflict,
            ErrorCode::RateLimited,
            ErrorCode::UnsupportedMedia,
            ErrorCode::InvalidCursor,
            ErrorCode::Internal,
        ] {
            assert!(!title_for(code).is_empty());
            let s = slug(code);
            assert!(!s.is_empty() && !s.contains('_'), "bozuk slug: {s}");
            assert!(StatusCode::from_u16(code.http_status()).is_ok());
        }
    }
}
