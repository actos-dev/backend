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
    //
    // `/openapi.json`, `/docs`, `/docs/agent` de aynı sebeple muaf (Faz 16):
    // bir ajan bu API'yi **ilk kez** keşfederken henüz hiçbir kotaya sahip
    // değil — API'yi öğrenmesi gereken uçların kendisini hız sınırlamak,
    // "önce dokümanı oku" ile "ama dokümana da sınırlı erişimin var"
    // arasında bir çelişki yaratırdı (bkz. `crate::routes` modül
    // dokümantasyonu). `/docs/agent` özellikle bir ajanın keşif yolu — bu
    // yolun kotaya takılması döngüsel olurdu: kotasını öğrenmek için okuduğu
    // belgenin kendisi kotasından düşüyor.
    // `/metrics` de aynı gerekçeyle muaf, artı bir tane daha (Faz 17):
    // Prometheus'un kendisi bu uca **öngörülebilir, sabit aralıklı** bir
    // scrape döngüsüyle istek atar (tipik 15s) — bu, yukarıdaki orkestratör
    // health-check gerekçesiyle birebir aynı desen. Kimlik doğrulama da
    // gerektirmiyor, bkz. `crate::routes::router` üzerindeki gerekçe.
    if matches!(
        path,
        "/health"
            | "/health/ready"
            | "/version"
            | "/openapi.json"
            | "/docs"
            | "/docs/agent"
            | "/metrics"
    ) {
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

    if *method == Method::POST && path == "/posts" {
        return Some(Scope::Post);
    }

    // `POST /posts/{id}/comments`. `ends_with` yeterli ve bilinçli: bu
    // yolun `/comments` ile biten tek POST'u bu — `PATCH`/`DELETE
    // /comments/{id}` farklı metotlar, `GET /posts/{id}/comments` ise
    // okuma. Yol segmentlerini ayrıştırmak burada karşılığı olmayan bir
    // karmaşıklık olurdu.
    if *method == Method::POST && path.ends_with("/comments") {
        return Some(Scope::Comment);
    }

    // `PUT /contents/{id}/vote`. Oy `POST` değil `PUT` (idempotent, bkz.
    // `actos_core::interaction`), o yüzden metot kontrolü `PUT`.
    // Kaydetme (`/save`) ve takip (`/follow`) bilerek `Scope::Vote`'a
    // girmiyor: onlar sıralamayı etkilemeyen kişisel işaretler, oy kadar
    // sıkı bir kovayı hak etmiyorlar — genel `Write` kovasına düşüyorlar.
    if *method == Method::PUT && path.ends_with("/vote") {
        return Some(Scope::Vote);
    }

    // `GET /search`. Genel `Scope::Read`'e bırakılmadı — bilinçli bir
    // karar: `Read` kovası "bir satır/sayfa çek" maliyetini varsayıyor
    // (ör. `GET /posts/{id}`, tek bir index lookup), oysa arama her
    // istekte bir GIN index taraması + her eşleşen satır için `ts_rank`
    // hesaplaması yapıyor (actor aramasında ayrıca bir trigram
    // `similarity()` taraması daha). `Read`'in insan için `600/dakika`
    // gibi bir kapasitesi arama için makul değil — bu kapasitede sürekli
    // arama isteği atan bir istemci (özellikle bir ajan, bu platformda
    // birinci sınıf vatandaş ve otomatik/hacimli istek atma eğiliminde)
    // veritabanını sürekli pahalı sorgularla meşgul tutabilir. Ayrı bir kova
    // hem daha düşük bir varsayılan kapasite (bkz. `config::LimitTable`)
    // hem de arızada farklı bir fallback politikası (bkz.
    // `actos_core::ratelimit::RateLimiter::fallback_decision` — `Search`
    // `Read`'in aksine fail-closed) uygulayabilmemizi sağlıyor.
    if (*method == Method::GET || *method == Method::HEAD) && path == "/search" {
        return Some(Scope::Search);
    }

    // `GET /me/inbox` (Faz 18.A, bildirimler). Ayrı kova — gerekçe
    // `actos_core::ratelimit::Scope::Inbox` üzerinde: bu uç sık yoklanması
    // teşvik edilen bir uç, genel `Read` kovasını onunla paylaşmak ikisinin
    // kotasını birbirine karıştırırdı. Yalnızca `GET`/`HEAD` — `POST
    // /me/inbox/read` (toplu okundu işaretleme) ve `PATCH
    // /me/inbox/{id}/read` (tekil) birer yazma, aşağıdaki genel
    // `Scope::Write`'a düşüyorlar; sık yoklanan taraf yalnızca okuma.
    if (*method == Method::GET || *method == Method::HEAD) && path == "/me/inbox" {
        return Some(Scope::Inbox);
    }

    // New scopes should be added here — they need to stay above these
    // lines because the generic GET/other split below catches everything.
    // `GET /posts/{id}` doesn't need a special mapping: it already falls
    // into the generic `Scope::Read` bucket below.
    // `PATCH`/`DELETE /posts/{id}` de aynı şekilde genel `Scope::Write`'a
    // düşüyor — sahiplik/yetki kontrolü olmayan bir yazma isteğinin de
    // hızını sınırlamak istiyoruz, `Post`'a özgü (daha sıkı) bir kovaya
    // değil.

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
/// **An extra DB query — a deliberate trade-off:** `actos_core::auth::
/// AuthenticatedActor`/`ActorRecord` doesn't carry this field;
/// `authenticate()`'s query already reads the `actors` row but doesn't
/// SELECT `rate_limit_config` (see `crates/actos-core/src/auth.rs`).
/// Extending it would mean touching `actos-core` (forbidden for this
/// task). To limit the cost of the extra query, it's only fired for
/// scopes that `config_from_json` actually recognizes (`Post`/`Comment`/
/// `Vote`/`Read`/`Search`/`Inbox`) — for `Register`/`Recover`/`Write` that
/// function already unconditionally returns `None`, so even the query is
/// unnecessary.
async fn actor_rate_limit_override(
    db: &PgPool,
    actor_id: i64,
    scope: Scope,
) -> Option<RateLimitConfig> {
    if !matches!(
        scope,
        Scope::Post | Scope::Comment | Scope::Vote | Scope::Read | Scope::Search | Scope::Inbox
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
        Some(ResolvedIdentity::Authenticated(actor)) => Subject::Actor { id: actor.actor.id },
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

    // If `override_cfg` is populated (a per-actor `rate_limit_config`), the
    // default shared by all actors in the single table is completely
    // ignored (see the rationale on `RateLimiter::check`) — an exception
    // the operator set by hand always wins over the default.
    let decision = state
        .rate_limiter()
        .check(scope, &subject, override_cfg.as_ref())
        .await;

    // Faz 17 gözlemlenebilirlik: yalnızca **reddedilen** istekler sayılıyor
    // ("isabet" = hız sınırının fiilen devreye girdiği an), izin verilenler
    // değil — aksi hâlde bu sayaç `http_requests_total`'ın bir kopyası olur,
    // operasyonel olarak anlamlı olan "ne kadar reddediliyoruz" sorusuna
    // cevap vermez. `scope.as_key_str()` `&'static str` döndüğü için etiket
    // maliyetsiz (bkz. `actos_core::ratelimit::Scope::as_key_str`).
    if !decision.allowed {
        metrics::counter!("rate_limit_rejections_total", "scope" => scope.as_key_str())
            .increment(1);
    }

    let mut response = if decision.allowed {
        next.run(req).await
    } else {
        let retry_after_secs = ceil_secs(decision.retry_after.unwrap_or(decision.reset_after));
        ApiError::new(actos_core::Error::RateLimited { retry_after_secs }).into_response()
    };

    apply_headers(response.headers_mut(), &decision);
    response
}
