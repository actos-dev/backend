//! Kimlik doğrulama extractor'ları.
//!
//! Gerçek doğrulama artık burada **değil**: `crate::middleware::identity::
//! resolve`, istek başına bir kez çalışıp `Authorization` header'ını
//! çözüyor ve sonucu request extension'ına ([`ResolvedIdentity`]) koyuyor —
//! bunun sebebi, hız sınırlama middleware'inin (`crate::middleware::
//! ratelimit`) doğru `Subject`'i seçebilmek için kimliği handler'a
//! girmeden **önce** bilmesi gerekmesi (bkz. o modüllerin dokümantasyonu).
//! Bu dosya yalnızca o extension'ı okuyup `CurrentActor`/`OptionalActor`'a
//! çeviriyor — istek başına ikinci bir `authenticate` çağrısı yok.

use axum::{extract::FromRequestParts, http::request::Parts};

pub use actos_core::auth::AuthenticatedActor;

use crate::{error::ApiError, middleware::identity::ResolvedIdentity, state::AppState};

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
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match parts.extensions.get::<ResolvedIdentity>() {
            Some(ResolvedIdentity::Authenticated(actor)) => Ok(Self(actor.clone())),
            Some(ResolvedIdentity::Failed(err)) => {
                Err(ApiError::from_arc(err.clone()).with_request_id(&parts.headers))
            }
            // `None` normalde hiç oluşmaz — `identity::resolve` middleware'i
            // her isteği sarmalıyor, extension her zaman dolu olmalı. Yine
            // de savunmacı: middleware bir şekilde atlanırsa (ör. yanlış
            // kurulmuş bir test router'ı) sessizce "kimliksiz" davranmak
            // yerine aynı, doğru hatayı üretmek daha güvenli.
            Some(ResolvedIdentity::Anonymous) | None => {
                Err(ApiError::new(actos_core::Error::MissingCredentials)
                    .with_request_id(&parts.headers))
            }
        }
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
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match parts.extensions.get::<ResolvedIdentity>() {
            Some(ResolvedIdentity::Authenticated(actor)) => Ok(Self(Some(actor.clone()))),
            Some(ResolvedIdentity::Failed(err)) => {
                Err(ApiError::from_arc(err.clone()).with_request_id(&parts.headers))
            }
            Some(ResolvedIdentity::Anonymous) | None => Ok(Self(None)),
        }
    }
}
