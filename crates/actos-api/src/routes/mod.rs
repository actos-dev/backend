//! HTTP rotaları.
//!
//! ## OpenAPI (Faz 16) — `OpenApiRouter`, elle path listesi değil
//!
//! Her alt modülün `router()` fonksiyonu `axum::Router<AppState>` değil
//! `utoipa_axum::router::OpenApiRouter<AppState>` döner; `.route("/x",
//! get(h))` yerine `.routes(routes!(h))` kullanılır. Bu iki değişikliğin
//! dışında rota mantığı hiç değişmedi.
//!
//! **Neden bu, `#[derive(OpenApi)] #[openapi(paths(...))]` içinde elle bir
//! fonksiyon listesi tutmaktan daha invaziv bir yaklaşım değil mi?** Evet,
//! ama bilinçli bir tercih: elle tutulan bir liste, yeni bir uç eklenip o
//! listeye eklenmesi unutulduğunda **sessizce** eksik bir spec üretir —
//! derleme de, test de bunu yakalamaz. `OpenApiRouter` + `routes!()` ise
//! rotanın axum'a kaydını *ve* OpenAPI şemasına kaydını **aynı çağrıda**
//! yapar (bkz. `routes!` makrosunun ürettiği `(schemas, paths,
//! method_router)` üçlüsü) — bir uç axum'da yaşıyorsa spec'te de yaşar,
//! ikisinin ayrı düşmesi derleme zamanında imkânsız. PLAN.md'nin "spec'in
//! kodla senkron kaldığını doğrulayan CI kontrolü" maddesi bu yüzden ayrı
//! bir CI adımı gerektirmiyor: garanti zaten burada, derleme zamanında.
//!
//! `routes!(a, b)` **yalnızca aynı URL yoluna** (farklı HTTP metotlarıyla)
//! sahip handler'ları birleştirmek için kullanılır (bkz. `utoipa_axum::routes!`
//! makro dokümantasyonu) — ör. `/auth/keys` için `routes!(create_key,
//! list_keys)`. Farklı yollara `.routes()` her zaman ayrı ayrı çağrılıyor.
//!
//! ## `GET /openapi.json` ve `GET /docs`
//!
//! İkisi de **hız sınırından ve kimlik doğrulamadan muaf** (bkz.
//! `crate::middleware::ratelimit::classify`): bir ajan API'yi öğrenmeden bir
//! API key alamaz — `GET /docs`/`GET /openapi.json`'ın kendisi kimlik
//! gerektirseydi bu döngüsel olurdu. Hız sınırından muaf olmaları da aynı
//! gerekçeyle: `/health`/`/version` gibi bunlar da bir ajanın *ilk* isteği
//! olabilir, henüz hiçbir kotaya sahip değilken.
//!
//! `GET /docs` (Scalar UI) spec'i **HTML'in içine gömerek** sunuyor
//! (`utoipa_scalar::Scalar::to_html`, `$spec` yer tutucusu) — yani sayfa
//! açıldığında `/openapi.json`'a ayrı bir istek atmıyor, ikisi birbirinden
//! bağımsız.

pub mod actors;
pub mod admin;
pub mod auth;
pub mod comments;
pub mod feed;
pub mod health;
pub mod interactions;
pub mod meta;
pub mod posts;
pub mod search;
pub mod tags;
pub mod uploads;

use std::sync::Arc;

use axum::{Json, Router, http::HeaderMap, routing::get};
use utoipa::OpenApi as _;
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_scalar::{Scalar, Servable as _};

use crate::{error::ApiError, openapi::ApiDoc, state::AppState};

/// Uygulamanın rota ağacı. Katmanlar burada değil, `app` içinde eklenir.
pub fn router() -> Router<AppState> {
    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(health::live))
        .routes(routes!(health::ready))
        .routes(routes!(meta::version))
        .merge(auth::router())
        .merge(actors::router())
        .merge(posts::router())
        .merge(comments::router())
        .merge(tags::router())
        .merge(search::router())
        .merge(interactions::router())
        .merge(feed::router())
        .merge(uploads::router())
        .merge(admin::router())
        .split_for_parts();

    // `/openapi.json` bu üretilmiş `openapi` değerinden serveden ham bir
    // handler — `Arc` ile sarmalanıyor ki her istek tüm spec'i yeniden
    // klonlamak yerine yalnızca referans sayacını artırsın (spec küçük
    // olmasa da bu uç sık çağrılan bir "hot path" değil, yine de bedelsiz
    // bir optimizasyon).
    let spec = Arc::new(openapi.clone());

    router
        .route(
            "/openapi.json",
            get(move || {
                let spec = Arc::clone(&spec);
                async move { Json((*spec).clone()) }
            }),
        )
        .merge(Scalar::with_url("/docs", openapi))
        .fallback(not_found)
}

/// Eşleşmeyen rotalar için de aynı hata biçimi.
///
/// Varsayılan davranış boş gövdeli bir 404 döndürmek olurdu; istemcilerin
/// (özellikle ajanların) her hatayı tek bir şemayla ayrıştırabilmesi için
/// burada da `application/problem+json` üretiyoruz.
async fn not_found(headers: HeaderMap) -> ApiError {
    ApiError::new(actos_core::Error::NotFound("rota")).with_request_id(&headers)
}
