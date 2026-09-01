//! Kimlik çözümleme middleware'i.
//!
//! **Sorun:** `CurrentActor` eskiden bir extractor'dı ve handler'a girerken
//! kendi başına `actos_core::auth::authenticate`'i çağırıyordu. Hız sınırlama
//! ise doğru `Subject`'i (kimlikli actor mı, kimliksiz IP mi — ki limit
//! bundan değişiyor) handler'dan **önce** bilmek zorunda, ve
//! `X-RateLimit-*` header'ları 401 dahil **her** yanıtta olmalı. İki ayrı
//! yerde `authenticate` çağırmak hem gereksiz bir veritabanı sorgusu hem de
//! (401 durumunda) hangi hatanın "asıl" olduğu konusunda tutarsızlık riski
//! demek.
//!
//! **Çözüm:** bu middleware her istekte **tam olarak bir kez** çalışır,
//! `Authorization` header'ı varsa doğrular, sonucu (başarı ya da hata fark
//! etmeksizin) [`ResolvedIdentity`] olarak request extension'ına koyar.
//! `crate::auth::CurrentActor`/`OptionalActor` extractor'ları artık kendileri
//! doğrulama yapmıyor, yalnızca buradan okuyor. `crate::middleware::ratelimit`
//! (bu middleware'den **sonra**, bkz. `crate::app`'teki katman sırası) aynı
//! extension'ı kullanarak `Subject::Actor` mı `Subject::Ip` mi olduğuna
//! karar veriyor.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::Response,
};

use actos_core::auth::AuthenticatedActor;

use crate::state::AppState;

/// [`resolve`] tarafından request extension'ına konan, istek başına bir kez
/// hesaplanan kimlik durumu.
///
/// Üç hâl birbirinden **kasıtlı olarak** ayrı: `CurrentActor` ve
/// `OptionalActor` bunları farklı yorumluyor (bkz. `crate::auth`).
#[derive(Clone)]
pub enum ResolvedIdentity {
    /// `Authorization` header'ı hiç yoktu.
    Anonymous,
    /// Header vardı, doğrulama başarılı oldu.
    Authenticated(AuthenticatedActor),
    /// Header vardı ama doğrulama başarısız oldu (bozuk anahtar, iptal
    /// edilmiş, banlı hesap, veritabanı hatası...).
    ///
    /// `Arc` sarmalı: `actos_core::Error` `Clone` değil (içinde
    /// `sqlx::Error` var), ama bu extension birden fazla extractor
    /// tarafından okunabiliyor (ör. `OptionalActor`, `CurrentActor`'ın
    /// mantığını çağırır) — `Arc::clone` ile ucuzca paylaşılıyor.
    Failed(Arc<actos_core::Error>),
}

/// `Authorization` header'ından ham API key'i çıkarır.
///
/// `Bearer` öneki büyük/küçük harf duyarsız kabul edilir (RFC 7235 şema
/// adları case-insensitive) — eskiden `crate::auth::extract_bearer`
/// içindeydi, doğrulama buraya taşındığı için o da buraya taşındı.
fn extract_bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
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

/// Tower/axum middleware fonksiyonu — `crate::app::build`'te
/// `axum::middleware::from_fn_with_state` ile katmana eklenir.
///
/// `next.run(req)` çağrısından **önce** çalışır (kimlik, handler'a
/// ulaşmadan biliniyor olmalı) ama yanıtı kendisi değiştirmez — yalnızca
/// isteğe [`ResolvedIdentity`] ekleyip akışı devam ettirir. Yanıta
/// header eklemek `crate::middleware::ratelimit`'in işi.
pub async fn resolve(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let identity = match extract_bearer(req.headers()) {
        None => ResolvedIdentity::Anonymous,
        Some(raw_key) => match actos_core::auth::authenticate(state.db(), &raw_key).await {
            Ok(actor) => {
                // Faz 5'teki `touch_key`'in Redis tamponlu karşılığı
                // (bkz. `RateLimiter::record_key_use` üzerindeki yorum).
                // İsteği geciktirmemesi için ayrı bir görev olarak
                // fırlatılıyor — tıpkı eski `crate::auth::spawn_touch`
                // deseni gibi, hatası yalnızca loglanır.
                let limiter = state.rate_limiter_handle();
                let key_id = actor.key_id;
                tokio::spawn(async move { limiter.record_key_use(key_id).await });
                ResolvedIdentity::Authenticated(actor)
            }
            Err(err) => ResolvedIdentity::Failed(Arc::new(err)),
        },
    };

    req.extensions_mut().insert(identity);
    next.run(req).await
}
