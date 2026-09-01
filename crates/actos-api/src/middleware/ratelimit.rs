//! Hız sınırlama middleware'i.
//!
//! `crate::middleware::identity`'den **sonra** çalışır (bkz. `crate::app`
//! katman sırası): doğru `Subject`'i (kimlikli actor mı, kimliksiz IP mi)
//! seçebilmesi için kimliğin zaten çözülmüş olması gerekiyor.
//!
//! `actos_core::ratelimit::RateLimiter::check` bir karar üretir; bu modülün
//! işi o kararı HTTP'ye çevirmek: `X-RateLimit-*` header'larını **her**
//! yanıta (izin verilen, reddedilen, hatta 401/404 gibi sonradan üretilen
//! hatalara) eklemek, reddedildiyse `429 application/problem+json` döndürmek.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sqlx::PgPool;

use actos_core::ratelimit::{RateLimitConfig, RateLimitDecision, Scope, Subject, config_from_json};

use crate::{
    error::ApiError, middleware::client_ip, middleware::identity::ResolvedIdentity, state::AppState,
};

/// İstenen `Scope`'u yol + metottan çıkarır.
///
/// **Genişletilebilir tasarım:** Faz 8+'ta `POST /posts`, `POST
/// /posts/{id}/comments`, `POST /posts/{id}/votes`, `POST /media` gibi
/// uçlar geldiğinde buraya birer `if` kolu eklemek yeterli olacak — geri
/// kalan mantık (subject seçimi, override, header'lar) değişmeden kalır.
///
/// `None` yalnızca sağlık/versiyon uçları için döner (bkz. aşağıdaki
/// muafiyet notu).
fn classify(method: &Method, path: &str) -> Option<Scope> {
    // `/health`, `/health/ready`, `/version` **muaf**: bir orkestratör
    // (Kubernetes liveness/readiness probe'u, bir yük dengeleyicinin sağlık
    // kontrolü) bu uçlara çok sık ve öngörülebilir aralıklarla istek atar.
    // Bu istekler hız sınırına takılırsa orkestratör sağlıklı bir instance'ı
    // "unhealthy" sanıp öldürür/trafikten düşürür — hız sınırlamanın
    // kendisi bir kesinti sebebi olmamalı.
    if matches!(path, "/health" | "/health/ready" | "/version") {
        return None;
    }

    if *method == Method::POST && path == "/auth/register" {
        return Some(Scope::Register);
    }
    if *method == Method::POST
        && (path == "/auth/recover" || path == "/auth/recovery-codes/regenerate")
    {
        return Some(Scope::Recover);
    }

    // Faz 8+ geldiğinde buraya özel eşlemeler eklenecek, ör.:
    //   if *method == Method::POST && path == "/posts" { return Some(Scope::Post); }
    //   if *method == Method::POST && path.ends_with("/comments") { return Some(Scope::Comment); }
    //   if *method == Method::POST && path.ends_with("/votes") { return Some(Scope::Vote); }
    //   if *method == Method::POST && path == "/media" { return Some(Scope::Upload); }
    // Bu satırların üstünde durmaları gerekiyor çünkü aşağıdaki genel
    // GET/diğer ayrımı her şeyi yakalar.

    Some(if *method == Method::GET || *method == Method::HEAD {
        Scope::Read
    } else {
        Scope::Write
    })
}

/// İstemci IP'sini `ConnectInfo` + (varsa) `X-Forwarded-For`'dan çözer.
///
/// `ConnectInfo<SocketAddr>` normalde her zaman mevcuttur (bkz. `main.rs`,
/// `into_make_service_with_connect_info`); test harness'i `oneshot` ile
/// doğrudan çağırdığında (`axum::extract::connect_info::MockConnectInfo`
/// eklenmediyse) mevcut olmayabilir — bu durumda çökmek yerine loglayıp
/// `0.0.0.0`'a düşülür (yalnızca test/yanlış-kurulum senaryosu; üretimde
/// `main.rs` bunu garanti eder).
fn resolve_client_ip(req: &Request, trusted_proxy_hops: usize) -> IpAddr {
    let socket = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip())
        .unwrap_or_else(|| {
            tracing::warn!(
                "ConnectInfo bulunamadı, istemci IP'si çözülemiyor — 0.0.0.0 kullanılacak"
            );
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        });

    let xff = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok());

    client_ip::resolve(socket, xff, trusted_proxy_hops)
}

/// `actors.rate_limit_config` jsonb'sinden bu `scope`'a özel bir override
/// olup olmadığını okur.
///
/// **Ek bir DB sorgusu — bilinçli bir taviz:** `actos_core::auth::
/// AuthenticatedActor`/`ActorRecord` bu alanı taşımıyor; `authenticate()`
/// sorgusu `actors` satırını zaten okuyor ama `rate_limit_config`'i SELECT
/// etmiyor (bkz. `crates/actos-core/src/auth.rs`). Onu genişletmek
/// `actos-core`'a dokunmak demek olurdu (bu görevde yasak). Ek sorgunun
/// maliyetini sınırlamak için yalnızca `config_from_json`'ın gerçekten
/// tanıdığı scope'larda (`Post`/`Comment`/`Vote`/`Read`/`Upload`) atılıyor
/// — `Register`/`Recover`/`Write` için o fonksiyon zaten koşulsuz `None`
/// döndüğünden sorgu bile gereksiz.
async fn actor_rate_limit_override(
    db: &PgPool,
    actor_id: i64,
    scope: Scope,
) -> Option<RateLimitConfig> {
    if !matches!(
        scope,
        Scope::Post | Scope::Comment | Scope::Vote | Scope::Read | Scope::Upload
    ) {
        return None;
    }

    let value: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT rate_limit_config FROM actors WHERE id = $1")
            .bind(actor_id)
            .fetch_optional(db)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(
                    actor_id,
                    error = %err,
                    "actors.rate_limit_config okunamadı, varsayılan limit kullanılacak"
                );
                None
            });

    value.and_then(|v| config_from_json(&v, scope))
}

/// Saniyeye **yukarı yuvarlar** — `Duration`'ın alt-saniye kısmı varsa
/// istemciye "1 saniye sonra dene" yerine "0 saniye sonra dene" (yani
/// "hemen") sinyali vermemek için.
fn ceil_secs(d: Duration) -> u64 {
    let secs = d.as_secs();
    if d.subsec_nanos() > 0 { secs + 1 } else { secs }
}

/// `X-RateLimit-Limit` / `-Remaining` / `-Reset` header'larını yanıta ekler.
/// `Retry-After` bunun dışında: yalnızca reddedilen isteklerde, `ApiError`
/// (`Error::RateLimited`) tarafından zaten ekleniyor (bkz. `crate::error`).
fn apply_headers(headers: &mut HeaderMap, decision: &RateLimitDecision) {
    let entries = [
        ("x-ratelimit-limit", decision.limit.to_string()),
        ("x-ratelimit-remaining", decision.remaining.to_string()),
        (
            "x-ratelimit-reset",
            ceil_secs(decision.reset_after).to_string(),
        ),
    ];
    for (name, value) in entries {
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.insert(HeaderName::from_static(name), value);
        }
    }
}

/// Tower/axum middleware fonksiyonu — `crate::app::build`'te
/// `crate::middleware::identity::resolve`'dan **sonra** katmana eklenir.
pub async fn enforce(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();

    let Some(scope) = classify(&method, &path) else {
        // Muaf uç (sağlık/versiyon): hız sınırlamaya hiç girmiyor, header
        // da eklenmiyor — bu uçlar hız sınırlamanın parçası değil.
        return next.run(req).await;
    };

    // `identity` middleware'i bu middleware'den önce çalıştığı için
    // extension her zaman dolu olmalı; yine de savunmacı: yoksa
    // kimliksiz (IP başına) davranılır.
    let identity = req.extensions().get::<ResolvedIdentity>().cloned();

    let subject = match &identity {
        Some(ResolvedIdentity::Authenticated(actor)) => Subject::Actor {
            id: actor.actor.id,
            actor_type: actor.actor.actor_type,
        },
        // Anonim VE doğrulaması başarısız olmuş (401 dönecek) istekler
        // aynı şekilde IP başına sınırlanır — bir istemci geçersiz key'ler
        // deneyerek hız sınırını atlatamamalı.
        _ => Subject::Ip(resolve_client_ip(
            &req,
            state.config().server.trusted_proxy_hops,
        )),
    };

    let override_cfg = match &identity {
        Some(ResolvedIdentity::Authenticated(actor)) => {
            actor_rate_limit_override(state.db(), actor.actor.id, scope).await
        }
        _ => None,
    };

    let decision = state
        .rate_limiter()
        .check(scope, &subject, override_cfg.as_ref())
        .await;

    let mut response = if decision.allowed {
        next.run(req).await
    } else {
        let retry_after_secs = ceil_secs(decision.retry_after.unwrap_or(decision.reset_after));
        ApiError::new(actos_core::Error::RateLimited { retry_after_secs }).into_response()
    };

    apply_headers(response.headers_mut(), &decision);
    response
}
