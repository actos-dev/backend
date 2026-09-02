//! Actor profilleri: public profil, kendi profilini güncelleme/silme,
//! takipçi/takip listeleri, keşif dizini.
//!
//! `crate::routes::auth`'taki gibi burada da iş mantığı yok — her handler
//! `actos_core::actor`'ın bir fonksiyonunu çağırır, sonucu HTTP'ye çevirir.
//! `actor_summary`/`parse_actor_type` gibi ortak dönüşümler burada tekrar
//! yazılmıyor, `crate::routes::auth`'tan (`pub(crate)`) alınıyor.
//!
//! **Cursor'lar burada, uçta encode/decode edilir:** `actos_core::actor`
//! fonksiyonları çözülmüş bir `Cursor` alır/döner, ham `?cursor=` string'i
//! ile `Cursor` arasındaki dönüşüm (`CursorCodec`, `AppState`'ten) yalnızca
//! bu dosyada olur — tıpkı `IdCodec`'in yalnızca HTTP katmanında
//! kullanılması gibi (bkz. `routes/auth.rs`).

use actos_core::{
    Error, actor as core_actor,
    auth::ActorRecord,
    cursor::{Cursor, CursorCodec, SortKind},
};
use actos_types::actor::{
    ActorListResponse, ActorProfileResponse, ActorStats, DeleteAccountRequest,
    UpdateProfileRequest, UpdateProfileResponse,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::CurrentActor,
    error::ApiError,
    openapi::{Gone, NotFound, RateLimited, Unauthorized, ValidationFailed},
    routes::auth::{actor_summary, parse_actor_type},
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_directory))
        .routes(routes!(update_profile, delete_account))
        .routes(routes!(get_profile))
        .routes(routes!(list_followers))
        .routes(routes!(list_following))
}

// --- Query param tipleri -------------------------------------------------
//
// `limit` bilerek `String` olarak alınıyor, `i64` değil: axum'un `Query<T>`
// extractor'ı tip dönüşümü başarısız olduğunda kendi (RFC 9457 OLMAYAN,
// düz metin) reddini üretir. Sayısal ayrıştırmayı burada elle yaparak her
// hatanın aynı `application/problem+json` biçiminden geçmesini garantiliyoruz
// (bkz. [`parse_limit`]).

#[derive(Debug, Deserialize)]
struct PageQuery {
    cursor: Option<String>,
    limit: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DirectoryQuery {
    #[serde(rename = "type")]
    actor_type: Option<String>,
    sort: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

/// Ham `limit` query değerini doğrulayıp [`actos_core::actor::clamp_page_size`]
/// ile makul bir aralığa sıkıştırır.
///
/// `pub(crate)`: `crate::routes::posts::list_actor_posts` (`GET
/// /actors/{username}/posts`) da aynı ayrıştırmaya ihtiyaç duyuyor —
/// `limit`/`cursor` ayrıştırma mantığının iki farklı yerde iki kopyası
/// olmasın diye burada bırakılıp oradan çağrılıyor.
pub(crate) fn parse_limit(raw: Option<String>, headers: &HeaderMap) -> Result<i64, ApiError> {
    let parsed = match raw {
        None => None,
        Some(s) => {
            let value: i64 = s.trim().parse().map_err(|_| {
                ApiError::new(Error::Validation(format!("geçersiz limit: \"{s}\"")))
                    .with_request_id(headers)
            })?;
            Some(value)
        }
    };
    Ok(core_actor::clamp_page_size(parsed))
}

/// Ham `cursor` query değerini çözer. Bu modüldeki her sayfalama `New`
/// sıralaması üzerinden (bkz. `actos_core::actor` modül dokümantasyonu) —
/// başka bir sıralamaya ait bir cursor burada [`actos_core::Error::InvalidCursor`]
/// ile reddedilir.
///
/// `pub(crate)`: bkz. [`parse_limit`] üzerindeki gerekçe — `GET
/// /actors/{username}/posts` de `New` sıralamasıyla sayfalanıyor
/// (`actos_core::content::list_posts_by_actor`), aynı çözümü kullanıyor.
pub(crate) fn decode_cursor(
    codec: &CursorCodec,
    raw: Option<&str>,
    headers: &HeaderMap,
) -> Result<Option<Cursor>, ApiError> {
    decode_cursor_with(codec, raw, SortKind::New, headers)
}

/// [`decode_cursor`]'ın sıralamayı çağırana bırakan hâli.
///
/// `crate::routes::comments`'ın yorum ağacı `?sort=top` ile
/// [`SortKind::Top`] cursor'ı da üretebiliyor; bir cursor'ın hangi
/// sıralamaya ait olduğu imzasının parçası (bkz. `actos_core::cursor`),
/// dolayısıyla çözerken beklenen sıralamayı vermek zorunludur — yanlış
/// sıralamayla çözülen bir cursor sessizce kabul edilseydi istemci
/// sayfaların ortasında sıçrayan bir liste görürdü.
pub(crate) fn decode_cursor_with(
    codec: &CursorCodec,
    raw: Option<&str>,
    kind: SortKind,
    headers: &HeaderMap,
) -> Result<Option<Cursor>, ApiError> {
    match raw {
        None | Some("") => Ok(None),
        Some(s) => codec
            .decode(s, kind)
            .map(Some)
            .map_err(|e| ApiError::new(Error::from(e)).with_request_id(headers)),
    }
}

// --- Handler'lar -----------------------------------------------------------

/// `GET /actors/{username}` → `200` (canlı), `410` (silinmiş), `404` (hiç
/// yok). Ayrım gerekçesi için `actos_core::actor::get_profile` dokümanına
/// bakın.
#[utoipa::path(
    get,
    path = "/actors/{username}",
    tag = "actors",
    summary = "Bir actor'ün public profilini oku",
    params(
        ("username" = String, Path, description = "Actor'ün kullanıcı adı"),
    ),
    responses(
        (status = 200, description = "Profil ve istatistikler", body = ActorProfileResponse),
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn get_profile(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ActorProfileResponse>, ApiError> {
    let profile = core_actor::get_profile(state.db(), &username)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let actor = actor_summary(&profile.actor, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(ActorProfileResponse {
        actor,
        stats: ActorStats {
            post_count: profile.post_count,
            comment_count: profile.comment_count,
            total_score: profile.total_score,
        },
    }))
}

/// `PATCH /actors/me` → `200`. Kısmi güncelleme: `req.display_name`/`req.bio`
/// `Option<Option<String>>` olarak aynen `actos_core::actor::update_profile`'a
/// geçiriliyor (bkz. `actos_types::actor::UpdateProfileRequest` dokümanı).
#[utoipa::path(
    patch,
    path = "/actors/me",
    tag = "actors",
    summary = "Kendi profilini kısmen güncelle",
    description = "Alan JSON'da hiç yoksa dokunulmaz; `null` gönderilirse temizlenir; değer \
        gönderilirse güncellenir (bkz. `actos_types::actor::UpdateProfileRequest`).",
    security(("api_key" = [])),
    request_body = UpdateProfileRequest,
    responses(
        (status = 200, description = "Güncellenmiş profil", body = UpdateProfileResponse),
        Unauthorized,
        ValidationFailed,
        RateLimited,
    )
)]
async fn update_profile(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateProfileRequest>,
) -> Result<Json<UpdateProfileResponse>, ApiError> {
    let updated =
        core_actor::update_profile(state.db(), current.actor.id, req.display_name, req.bio)
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let actor = actor_summary(&updated, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(UpdateProfileResponse { actor }))
}

/// `DELETE /actors/me` → `204`. Gövdede geçerli bir kurtarma kodu şart —
/// bkz. `actos_core::actor::delete_account`.
#[utoipa::path(
    delete,
    path = "/actors/me",
    tag = "actors",
    summary = "Kendi hesabını sil",
    description = "Geri alınamaz. Kanıt olarak gövdede geçerli bir kurtarma kodu gerekir; kod tüketilir.",
    security(("api_key" = [])),
    request_body = DeleteAccountRequest,
    responses(
        (status = 204, description = "Hesap silindi"),
        Unauthorized,
        ValidationFailed,
        RateLimited,
    )
)]
async fn delete_account(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DeleteAccountRequest>,
) -> Result<StatusCode, ApiError> {
    core_actor::delete_account(state.db(), current.actor.id, &req.recovery_code)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /actors/{username}/followers` → `200`. `username`'i takip edenler.
#[utoipa::path(
    get,
    path = "/actors/{username}/followers",
    tag = "actors",
    summary = "Bir actor'ü takip edenleri listele",
    params(
        ("username" = String, Path, description = "Actor'ün kullanıcı adı"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı (varsayılan/azami için `actos_core::actor::clamp_page_size`)"),
    ),
    responses(
        (status = 200, description = "Takipçi listesi, cursor'lu", body = ActorListResponse),
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn list_followers(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Json<ActorListResponse>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_actor::list_followers(state.db(), &username, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    render_page(&state, page, &headers, |entry| &entry.actor)
}

/// `GET /actors/{username}/following` → `200`. `username`'in takip ettikleri.
#[utoipa::path(
    get,
    path = "/actors/{username}/following",
    tag = "actors",
    summary = "Bir actor'ün takip ettiklerini listele",
    params(
        ("username" = String, Path, description = "Actor'ün kullanıcı adı"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı"),
    ),
    responses(
        (status = 200, description = "Takip edilenler listesi, cursor'lu", body = ActorListResponse),
        NotFound,
        Gone,
        RateLimited,
    )
)]
async fn list_following(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Json<ActorListResponse>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_actor::list_following(state.db(), &username, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    render_page(&state, page, &headers, |entry| &entry.actor)
}

/// `GET /actors?type=...&sort=new` → `200`. Keşif dizini.
#[utoipa::path(
    get,
    path = "/actors",
    tag = "actors",
    summary = "Actor keşif dizini",
    description = "Şu an yalnızca `sort=new` (varsayılan) destekleniyor.",
    params(
        ("type" = Option<String>, Query, description = "`human`, `ai_agent`, `system_bot`, `organization`"),
        ("sort" = Option<String>, Query, description = "Yalnızca `new` destekleniyor"),
        ("cursor" = Option<String>, Query, description = "Önceki sayfanın `next_cursor`'ı"),
        ("limit" = Option<String>, Query, description = "Sayfa başına öğe sayısı"),
    ),
    responses(
        (status = 200, description = "Actor listesi, cursor'lu", body = ActorListResponse),
        ValidationFailed,
        RateLimited,
    )
)]
async fn list_directory(
    State(state): State<AppState>,
    Query(query): Query<DirectoryQuery>,
    headers: HeaderMap,
) -> Result<Json<ActorListResponse>, ApiError> {
    let actor_type = match query.actor_type.as_deref() {
        None => None,
        Some(raw) => {
            Some(parse_actor_type(raw).map_err(|e| ApiError::new(e).with_request_id(&headers))?)
        }
    };

    // Şu an yalnızca `new` destekleniyor (bkz.
    // `actos_core::actor::list_directory` dokümanı); `sort` hiç
    // gönderilmemişse de aynı (tek) sıralama zaten uygulanıyor.
    if let Some(sort) = query.sort.as_deref()
        && sort != "new"
    {
        return Err(ApiError::new(Error::Validation(format!(
            "desteklenmeyen sort değeri: \"{sort}\" (yalnızca \"new\" destekleniyor)"
        )))
        .with_request_id(&headers));
    }

    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_actor::list_directory(state.db(), actor_type, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    render_page(&state, page, &headers, |actor| actor)
}

// --- Ortak yanıt kurulumu ----------------------------------------------

/// Bir [`core_actor::Page`]'i (öğe türü `T` — `list_followers`/
/// `list_following` için `core_actor::FollowEntry`, `list_directory` için
/// doğrudan `ActorRecord`) HTTP yanıtına çevirir: her öğeyi
/// [`actor_summary`] ile özetler, cursor'ı [`CursorCodec::encode`] ile
/// string'e kodlar. `actor_of` öğeden `ActorRecord` referansını çıkarır —
/// iki çağıran şeklinin ortak tek noktası bu (bkz. `FollowEntry.actor` vs.
/// doğrudan `ActorRecord`).
fn render_page<T>(
    state: &AppState,
    page: core_actor::Page<T>,
    headers: &HeaderMap,
    actor_of: impl Fn(&T) -> &ActorRecord,
) -> Result<Json<ActorListResponse>, ApiError> {
    let actors = page
        .items
        .iter()
        .map(|item| actor_summary(actor_of(item), state.id_codec()))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(headers))?;

    Ok(Json(ActorListResponse {
        actors,
        next_cursor: page.next_cursor.map(|c| state.cursor_codec().encode(&c)),
    }))
}
