//! Kimlik doğrulama extractor'ları.
//!
//! `Authorization: Bearer <key>` header'ını `actos_core::auth::authenticate`'e
//! bağlar. İş mantığının kendisi burada yok — bu modül yalnızca HTTP'ye özgü
//! kısmı (header ayrıştırma, `FromRequestParts`, fire-and-forget `touch_key`)
//! üstleniyor.

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
};

pub use actos_core::auth::AuthenticatedActor;

use crate::{error::ApiError, state::AppState};

/// Kimliği doğrulanmış bir istek sahibi.
///
/// Auth zorunlu uçlarda handler imzasına parametre olarak eklenir; extractor
/// başarısız olursa handler hiç çalışmaz, `ApiError` doğrudan döner.
#[derive(Debug, Clone)]
pub struct CurrentActor(pub AuthenticatedActor);

impl std::ops::Deref for CurrentActor {
    type Target = AuthenticatedActor;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequestParts<AppState> for CurrentActor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let raw_key = extract_bearer(parts).ok_or_else(|| {
            ApiError::new(actos_core::Error::MissingCredentials).with_request_id(&parts.headers)
        })?;

        let authenticated = actos_core::auth::authenticate(state.db(), &raw_key)
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&parts.headers))?;

        // `touch_key` isteği geciktirmemeli: ayrı bir görev olarak fırlatılır,
        // hatası (bkz. `touch_key` üzerindeki yorum) yalnızca loglanır, isteği
        // düşürmez.
        spawn_touch(state, authenticated.key_id);

        Ok(Self(authenticated))
    }
}

/// Auth'un opsiyonel olduğu uçlar için: kimlik bilgisi varsa doğrular,
/// yoksa `None` taşır (ör. "bu içeriği ben oyladım mı" gibi, kimliksiz
/// isteklerde de çalışması gereken public uçlarda kullanılacak).
///
/// **Yalnızca header'ın yokluğu `None` üretir.** `Authorization` header'ı
/// gönderilmiş ama bozuksa/geçersizse (kötü biçimli, iptal edilmiş, yanlış
/// key, banlı hesap) yine hata döner — istemci açıkça bir kimlik bilgisi
/// sundu, bunu sessizce yok saymak istemciyi kendi hatasından habersiz
/// bırakırdı.
#[derive(Debug, Clone)]
pub struct OptionalActor(pub Option<AuthenticatedActor>);

impl FromRequestParts<AppState> for OptionalActor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if parts.headers.get(header::AUTHORIZATION).is_none() {
            return Ok(Self(None));
        }

        let CurrentActor(actor) = CurrentActor::from_request_parts(parts, state).await?;
        Ok(Self(Some(actor)))
    }
}

/// `Authorization` header'ından ham API key'i çıkarır.
///
/// `Bearer` öneki büyük/küçük harf duyarsız kabul edilir (istemciler
/// `bearer` da yazabiliyor) — RFC 7235 şema adlarının case-insensitive
/// olduğunu söylüyor, biz de buna uyuyoruz.
fn extract_bearer(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let key = rest.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_owned())
    }
}

fn spawn_touch(state: &AppState, key_id: uuid::Uuid) {
    let db = state.db().clone();
    tokio::spawn(async move {
        actos_core::auth::touch_key(&db, key_id).await;
    });
}
