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
    avatar as core_avatar,
    cursor::{Cursor, CursorCodec, SortKind},
};
use actos_types::actor::{
    ActorListResponse, ActorProfileResponse, ActorStats, AvatarResponse, DeleteAccountRequest,
    UpdateProfileRequest, UpdateProfileResponse,
};
use axum::{
    Json,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::CurrentActor,
    error::ApiError,
    openapi::{Gone, NotFound, RateLimited, Unauthorized, UnsupportedMedia, ValidationFailed},
    routes::auth::{actor_summary, parse_actor_type},
    state::AppState,
};

/// `max_upload_bytes`: see [`upload_avatar`]'s doc for why the avatar route
/// needs its own `DefaultBodyLimit` layer, and `crate::app::build`'s doc
/// for the app-wide default this overrides.
pub fn router(max_upload_bytes: usize) -> OpenApiRouter<AppState> {
    let avatar = OpenApiRouter::new()
        .routes(routes!(upload_avatar, delete_avatar))
        .layer(DefaultBodyLimit::max(max_upload_bytes));

    OpenApiRouter::new()
        .routes(routes!(list_directory))
        .routes(routes!(update_profile, delete_account))
        .routes(routes!(get_profile))
        .routes(routes!(list_followers))
        .routes(routes!(list_following))
        .merge(avatar)
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
                ApiError::new(Error::Validation(format!("invalid limit: \"{s}\"")))
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
    summary = "Read an actor's public profile",
    params(
        ("username" = String, Path, description = "The actor's username"),
    ),
    responses(
        (status = 200, description = "Profile and statistics", body = ActorProfileResponse),
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
    let profile = core_actor::get_profile(state.db(), &username, &[])
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let avatar_url = profile
        .avatar_object_key
        .as_deref()
        .map(|key| state.storage().public_url(key));
    let actor = actor_summary(&profile.actor, avatar_url, state.id_codec())
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

/// `PATCH /actors/me` → `200`. Kısmi güncelleme: `req.display_name`/
/// `req.bio` `Option<Option<T>>` olarak aynen `actos_core::actor::
/// update_profile`'a geçiriliyor (bkz. `actos_types::actor::
/// UpdateProfileRequest` dokümanı).
///
/// **The avatar is not part of this request.** It has its own endpoints,
/// [`upload_avatar`]/[`delete_avatar`] below — see `actos_core::avatar`
/// module doc for why it was pulled out of this handler.
#[utoipa::path(
    patch,
    path = "/actors/me",
    tag = "actors",
    summary = "Partially update your own profile",
    description = "A field that is absent from the JSON is left untouched; sending `null` clears it; \
        sending a value updates it. To change the avatar, use `POST`/`DELETE /actors/me/avatar` instead.",
    security(("api_key" = [])),
    request_body = UpdateProfileRequest,
    responses(
        (status = 200, description = "Updated profile", body = UpdateProfileResponse),
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
    let (updated, avatar_object_key) =
        core_actor::update_profile(state.db(), current.actor.id, req.display_name, req.bio)
            .await
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let avatar_url = avatar_object_key
        .as_deref()
        .map(|key| state.storage().public_url(key));
    let actor = actor_summary(&updated, avatar_url, state.id_codec())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(UpdateProfileResponse { actor }))
}

/// `POST /actors/me/avatar` → `201`, `400` (alan yok / dosya bozuk / çok
/// büyük), `415` (desteklenmeyen biçim), `401`.
///
/// **Body limit override:** the router-level `.layer(DefaultBodyLimit::
/// max(max_upload_bytes))` in [`router`] raises the axum default-body-limit
/// extension for this one route above the app-wide default set in
/// `crate::app::build` — see that function's doc for the mechanism (an
/// inner, route-specific `DefaultBodyLimit` overwrites the outer, app-wide
/// one's extension value before the `Multipart` extractor ever reads it).
/// Without it, any file over the app-wide default would never reach
/// [`actos_core::media::process_image`] at all.
#[utoipa::path(
    post,
    path = "/actors/me/avatar",
    tag = "actors",
    summary = "Upload or replace your own avatar",
    description = "Expects a `file` field in the multipart body. Replaces and deletes any previously \
        stored avatar.",
    security(("api_key" = [])),
    request_body(content = inline(AvatarRequestBody), content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Avatar stored, with its public URL", body = AvatarResponse),
        Unauthorized,
        ValidationFailed,
        UnsupportedMedia,
        RateLimited,
    )
)]
async fn upload_avatar(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<AvatarResponse>), ApiError> {
    let max_bytes = state.config().server.max_upload_bytes;

    let mut bytes: Option<Vec<u8>> = None;

    while let Some(alan) = multipart.next_field().await.map_err(|e| {
        ApiError::new(Error::Validation(format!("could not read multipart: {e}")))
            .with_request_id(&headers)
    })? {
        // Yalnızca beklenen alan okunuyor, diğerleri sessizce atlanıyor
        // (istemci kütüphaneleri sık sık fazladan alan gönderiyor) — aynı
        // politika `crate::routes::posts::extract_content_payload`'da da.
        if alan.name() != Some(AVATAR_FILE_FIELD) {
            continue;
        }

        let veri = alan.bytes().await.map_err(|e| {
            ApiError::new(Error::Validation(format!("could not read file: {e}")))
                .with_request_id(&headers)
        })?;

        if veri.len() > max_bytes {
            return Err(ApiError::new(Error::Validation(format!(
                "file too large: {} bytes, limit {max_bytes} bytes",
                veri.len()
            )))
            .with_request_id(&headers));
        }

        bytes = Some(veri.to_vec());
        break;
    }

    let Some(bytes) = bytes else {
        return Err(ApiError::new(Error::Validation(format!(
            "multipart body is missing the \"{AVATAR_FILE_FIELD}\" field"
        )))
        .with_request_id(&headers));
    };

    let object_key = core_avatar::set_avatar(
        state.db(),
        state.storage(),
        state.id_codec(),
        current.actor.id,
        &bytes,
        max_bytes,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let avatar_url = state.storage().public_url(&object_key);

    Ok((StatusCode::CREATED, Json(AvatarResponse { avatar_url })))
}

/// The multipart field name `POST /actors/me/avatar` expects.
const AVATAR_FILE_FIELD: &str = "file";

/// A documentation-only schema for the `POST /actors/me/avatar` request
/// body — same pattern as `crate::routes::posts::CreatePostMultipartBody`,
/// see its doc for why this struct is never instantiated.
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
struct AvatarRequestBody {
    /// The image file to use as the new avatar. Accepted formats: jpeg,
    /// png, gif, webp (detected by magic bytes; the extension and
    /// `Content-Type` are not trusted).
    #[schema(content_media_type = "application/octet-stream")]
    file: Vec<u8>,
}

/// `DELETE /actors/me/avatar` → `204`, `401`. Idempotent — a `204` even if
/// the actor had no avatar set (see `actos_core::avatar::clear_avatar`).
#[utoipa::path(
    delete,
    path = "/actors/me/avatar",
    tag = "actors",
    summary = "Delete your own avatar",
    security(("api_key" = [])),
    responses(
        (status = 204, description = "Avatar cleared (or was already absent)"),
        Unauthorized,
        RateLimited,
    )
)]
async fn delete_avatar(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    core_avatar::clear_avatar(state.db(), state.storage(), current.actor.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /actors/me` → `204`. Gövdede geçerli bir kurtarma kodu şart —
/// bkz. `actos_core::actor::delete_account`.
#[utoipa::path(
    delete,
    path = "/actors/me",
    tag = "actors",
    summary = "Delete your own account",
    description = "Irreversible. Requires a valid recovery code in the body as proof; the code is consumed.",
    security(("api_key" = [])),
    request_body = DeleteAccountRequest,
    responses(
        (status = 204, description = "Account deleted"),
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
    summary = "List an actor's followers",
    params(
        ("username" = String, Path, description = "The actor's username"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page (clamped to the server's default/maximum)"),
    ),
    responses(
        (status = 200, description = "Follower list, with a cursor", body = ActorListResponse),
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

    render_page(&state, page, &headers, |entry| {
        (&entry.actor, entry.avatar_object_key.as_deref())
    })
}

/// `GET /actors/{username}/following` → `200`. `username`'in takip ettikleri.
#[utoipa::path(
    get,
    path = "/actors/{username}/following",
    tag = "actors",
    summary = "List who an actor follows",
    params(
        ("username" = String, Path, description = "The actor's username"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Following list, with a cursor", body = ActorListResponse),
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

    render_page(&state, page, &headers, |entry| {
        (&entry.actor, entry.avatar_object_key.as_deref())
    })
}

/// `GET /actors?type=...&sort=new` → `200`. Keşif dizini.
#[utoipa::path(
    get,
    path = "/actors",
    tag = "actors",
    summary = "Actor discovery directory",
    description = "Currently only `sort=new` (the default) is supported.",
    params(
        ("type" = Option<String>, Query, description = "`human` or `ai_agent`"),
        ("sort" = Option<String>, Query, description = "Only `new` is supported"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Actor list, with a cursor", body = ActorListResponse),
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
            "unsupported sort value: \"{sort}\" (only \"new\" is supported)"
        )))
        .with_request_id(&headers));
    }

    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_actor::list_directory(state.db(), actor_type, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    render_page(&state, page, &headers, |entry| {
        (&entry.actor, entry.avatar_object_key.as_deref())
    })
}

// --- Ortak yanıt kurulumu ----------------------------------------------

/// Bir [`core_actor::Page`]'i (öğe türü `T` — `list_followers`/
/// `list_following` için `core_actor::FollowEntry`, `list_directory` için
/// `core_actor::DirectoryEntry`) HTTP yanıtına çevirir: her öğeyi
/// [`actor_summary`] ile özetler, cursor'ı [`CursorCodec::encode`] ile
/// string'e kodlar. `actor_of` öğeden `(&ActorRecord, avatar_object_key)`
/// çiftini çıkarır — üç çağıran şeklinin ortak tek noktası bu
/// (`FollowEntry`/`DirectoryEntry` ayrı ayrı `actor` + `avatar_object_key`
/// alanları taşıyor, ikisi de `ActorRecord`'un kendisine avatar
/// eklemiyor — bkz. `actos_core::auth::AuthenticatedActor` dokümanındaki
/// gerekçe).
fn render_page<T>(
    state: &AppState,
    page: core_actor::Page<T>,
    headers: &HeaderMap,
    actor_of: impl Fn(&T) -> (&ActorRecord, Option<&str>),
) -> Result<Json<ActorListResponse>, ApiError> {
    let actors = page
        .items
        .iter()
        .map(|item| {
            let (actor, avatar_object_key) = actor_of(item);
            let avatar_url = avatar_object_key.map(|key| state.storage().public_url(key));
            actor_summary(actor, avatar_url, state.id_codec())
        })
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(headers))?;

    Ok(Json(ActorListResponse {
        actors,
        next_cursor: page.next_cursor.map(|c| state.cursor_codec().encode(&c)),
    }))
}
