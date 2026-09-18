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

use std::marker::PhantomData;

use axum::{extract::FromRequestParts, http::request::Parts};

pub use actos_core::auth::AuthenticatedActor;
use actos_core::{auth::Permission, authz};

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
            Some(ResolvedIdentity::Authenticated(actor)) => {
                // **Banlı actor yazamaz ama okuyabilir** (PLAN.md Faz 14).
                // Kural burada, tek yerde uygulanıyor: her yazma
                // handler'ına elle `if banned` yazmak, yeni bir uç
                // eklendiğinde unutulacak bir adım olurdu. Güvenli metotlar
                // (GET/HEAD/OPTIONS) geçiyor.
                if actor.banned && !parts.method.is_safe() {
                    return Err(
                        ApiError::new(actos_core::Error::Banned).with_request_id(&parts.headers)
                    );
                }
                Ok(Self(actor.clone()))
            }
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

/// Bir uçun gerektirdiği **global** izni taşıyan marker tipler.
///
/// Her marker [`Permission`] sabitini bildirir; [`Require`] extractor'ı bu
/// sabiti kontrol eder. Yeni bir izin gerektiren uç eklemek, burada bir
/// marker tanımlayıp handler imzasına `Require<Marker>` yazmak demektir —
/// yetki kontrolü yine **tip imzasının parçası**, handler gövdesinde
/// unutulabilecek bir `if` değil (eski `ModeratorActor`/`AdminActor`
/// garantisinin kapsamlı izin modelindeki karşılığı).
pub trait GlobalPermission: Send + Sync + 'static {
    /// Bu ucun gerektirdiği izin.
    const PERMISSION: Permission;
}

macro_rules! global_permission_marker {
    ($(#[$meta:meta])* $name:ident => $permission:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name;

        impl GlobalPermission for $name {
            const PERMISSION: Permission = $permission;
        }
    };
}

global_permission_marker!(
    /// `GET /admin/reports` — moderasyon kuyruğunu görme.
    CanViewReports => Permission::ReportView
);
global_permission_marker!(
    /// `PATCH /admin/reports/{id}` — şikayeti sonuçlandırma.
    CanResolveReports => Permission::ReportResolve
);
global_permission_marker!(
    /// `DELETE /admin/contents/{id}` — başkasının içeriğini silme.
    CanDeleteContent => Permission::ContentDelete
);
global_permission_marker!(
    /// `POST`/`DELETE /admin/bans` — platform geneli ban.
    CanBan => Permission::MemberBan
);
global_permission_marker!(
    /// `PUT`/`DELETE /admin/permissions` — izin verme/alma.
    CanGrantPermission => Permission::RoleGrant
);
global_permission_marker!(
    /// `GET /admin/actions` — denetim izini görme.
    CanViewAudit => Permission::AuditView
);

/// Verilen **global** izni gerektiren extractor.
///
/// `CurrentActor`'ı (dolayısıyla ban-yazma kontrolünü) temel alır; izin
/// yoksa handler'ın hiçbir satırı çalışmadan `403` döner.
#[derive(Debug, Clone)]
pub struct Require<P: GlobalPermission>(pub AuthenticatedActor, PhantomData<P>);

impl<P: GlobalPermission> std::ops::Deref for Require<P> {
    type Target = AuthenticatedActor;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<P: GlobalPermission> FromRequestParts<AppState> for Require<P> {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let current = CurrentActor::from_request_parts(parts, state).await?;
        if authz::actor_has_global(&current.0, P::PERMISSION) {
            Ok(Self(current.0, PhantomData))
        } else {
            Err(ApiError::new(actos_core::Error::Forbidden).with_request_id(&parts.headers))
        }
    }
}
