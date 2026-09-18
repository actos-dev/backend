//! Topluluk rotaları: oluşturma, dizin, tekil okuma, açıklama güncelleme,
//! üyelik ve topluluk akışı.
//!
//! `crate::routes::actors`/`crate::routes::posts`'taki gibi burada da iş
//! mantığı yok — her handler `actos_core::community`'nin bir fonksiyonunu
//! çağırır, sonucu HTTP'ye çevirir. `actor_summary` gibi ortak dönüşümler
//! `crate::routes::auth`'tan alınıyor; silinmiş sahip maskesi
//! `crate::routes::posts`'tan (`pub(crate)`).
//!
//! Rotalar isimle çalışıyor (`/communities/{name}`), iç id ile değil: topluluk
//! adı zaten insan-okunur ve paylaşılabilir bir adres (COMMUNITY_PLAN.md §2).
//! İç `bigint` yalnızca `ContentSummary.community.id` içinde, kodlanmış
//! olarak dışarı çıkıyor.

use actos_core::{
    Error,
    community::{
        self as core_community, Application, ApplicationStatus, Community, CommunityView,
        Invitation, MemberEntry,
    },
    content::PostSort,
    id::{
        Application as ApplicationIdKind, Community as CommunityIdKind, IdCodec,
        Invitation as InvitationIdKind,
    },
};
use actos_types::{
    community::{
        ApplicationListResponse, ApplicationSummary, CommunityListResponse,
        CommunityMemberListResponse, CommunityMemberSummary, CommunitySummary,
        CreateApplicationRequest, CreateCommunityRequest, CreateInvitationRequest,
        InvitationListResponse, InvitationSummary, SuccessorRequest, UpdateCommunityRequest,
    },
    content::{CommunityRefSummary, PostListResponse},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::Value;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::{CurrentActor, OptionalActor},
    error::ApiError,
    fields,
    openapi::{Conflict, Forbidden, NotFound, RateLimited, Unauthorized, ValidationFailed},
    routes::actors::{decode_cursor, decode_cursor_with, parse_limit},
    routes::auth::actor_summary,
    routes::posts::{content_summary_with_optional_body_html, masked_actor_summary},
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_community, list_communities))
        .routes(routes!(get_community, update_community))
        .routes(routes!(join_community, leave_community))
        .routes(routes!(list_members))
        .routes(routes!(kick_member))
        .routes(routes!(list_community_posts))
        .routes(routes!(close_community))
        .routes(routes!(set_successor))
        .routes(routes!(create_invitation))
        .routes(routes!(list_my_invitations))
        .routes(routes!(accept_invitation))
        .routes(routes!(decline_invitation))
        .routes(routes!(create_application))
        .routes(routes!(list_applications))
        .routes(routes!(resolve_application_accept))
        .routes(routes!(resolve_application_reject))
}

// --- Query param tipleri -------------------------------------------------

/// `GET /communities?cursor=&limit=` ve `GET /communities/{name}/members?...`.
#[derive(Debug, Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<String>,
}

/// `GET /communities/{name}/posts?sort=&cursor=&limit=&fields=`.
#[derive(Debug, Deserialize)]
struct CommunityPostsQuery {
    sort: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
    fields: Option<String>,
}

/// `GET /communities/{name}/applications?status=&cursor=&limit=`.
///
/// `cursor`/`limit` için [`ListQuery`] ile aynı alanlar, artı `status`
/// filtresi. `status` verilmezse her durum listelenir.
#[derive(Debug, Deserialize)]
struct ApplicationListQuery {
    status: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

// --- Ortak dönüşümler --------------------------------------------------

/// Bir [`Community`]'yi yanıt DTO'suna çevirir.
///
/// Silinmiş sahip, silinmiş içerik yazarıyla aynı şekilde maskelenir
/// (`masked_actor_summary`): sahip hesabını silmiş olabilir, ama topluluk
/// yaşamaya devam eder.
fn community_summary(
    community: &Community,
    id_codec: &IdCodec,
    is_member: bool,
) -> Result<CommunitySummary, Error> {
    let owner = if community.owner_deleted {
        masked_actor_summary(&community.owner, id_codec)?
    } else {
        actor_summary(&community.owner, None, id_codec)?
    };

    Ok(CommunitySummary {
        id: id_codec
            .encode::<actos_core::id::Community>(community.id)
            .map_err(|e| Error::Internal(format!("could not encode community id: {e}")))?,
        name: community.name.clone(),
        description: community.description.clone(),
        visibility: community.visibility.as_str().to_owned(),
        owner,
        member_count: community.member_count,
        post_count: community.post_count,
        is_member,
        created_at: community.created_at.to_rfc3339(),
        updated_at: community.updated_at.to_rfc3339(),
    })
}

// --- Handler'lar ---------------------------------------------------------

/// `POST /communities` → `201` + `Location: /communities/{name}`.
///
/// `visibility` opsiyoneldir; verilmezse `public`. Sahiplik sınırı (3) core
/// katmanında uygulanır.
#[utoipa::path(
    post,
    path = "/communities",
    tag = "communities",
    summary = "Create a community",
    description = "The creator becomes the owner and the first member. An actor may own at most \
        3 communities. `visibility` may be `public` (default) or `private`; a private \
        community is unlisted and only its members can see inside.",
    security(("api_key" = [])),
    request_body = CreateCommunityRequest,
    responses(
        (status = 201, description = "Community created", body = CommunitySummary,
            headers(("location" = String, description = "Path of the new community: /communities/{name}"))),
        ValidationFailed,
        Unauthorized,
        Conflict,
        RateLimited,
    )
)]
async fn create_community(
    current: CurrentActor,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateCommunityRequest>,
) -> Result<Response, ApiError> {
    let visibility = match req.visibility.as_deref() {
        Some(raw) => core_community::CommunityVisibility::parse(raw)
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?,
        None => core_community::CommunityVisibility::Public,
    };

    let community = core_community::create_community(
        state.db(),
        current.actor.id,
        &req.name,
        &req.description,
        visibility,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    // Oluşturan kişi sahibidir, sahip de her zaman üyedir.
    let summary = community_summary(&community, state.id_codec(), true)
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let location = format!("/communities/{}", summary.name);
    let mut response = (StatusCode::CREATED, Json(summary)).into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

/// `GET /communities?cursor=&limit=` → `200`. Yalnızca public topluluklar,
/// en yeni önce.
#[utoipa::path(
    get,
    path = "/communities",
    tag = "communities",
    summary = "List communities (the directory)",
    description = "Public communities, newest first. Private communities are never listed.",
    params(
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Community list, with a cursor", body = CommunityListResponse),
        RateLimited,
    )
)]
async fn list_communities(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Result<Json<CommunityListResponse>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_community::list_directory(state.db(), cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let communities = page
        .items
        .iter()
        .map(|community| community_summary(community, state.id_codec(), false))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(CommunityListResponse {
        communities,
        next_cursor: page.next_cursor.map(|c| state.cursor_codec().encode(&c)),
    }))
}

/// `GET /communities/{name}` → `200`, `404`. `is_member` anonim istekte
/// `false`.
///
/// Private topluluğu göremeyen okuyucuya **kapak** döner: ad ve açıklama
/// aynı, sayaçlar sıfır, `is_member = false` (§2). Görebilen (üye, topluluk
/// kapsamlı izin sahibi, global moderatör) ve public topluluklarda tam
/// özet döner.
#[utoipa::path(
    get,
    path = "/communities/{name}",
    tag = "communities",
    summary = "Read a community",
    description = "If an `Authorization` header is present, `is_member` reflects the requesting \
        actor; anonymous requests get `false`. A private community a viewer may not see inside \
        returns a cover: the same name and description, with `member_count = 0`, `post_count = 0` \
        and `is_member = false`.",
    params(
        ("name" = String, Path, description = "Community name"),
    ),
    responses(
        (status = 200, description = "Community summary (or cover)", body = CommunitySummary),
        NotFound,
        RateLimited,
    )
)]
async fn get_community(
    current: OptionalActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CommunitySummary>, ApiError> {
    let viewer_communities = state
        .viewer_communities(current.0.as_ref().map(|actor| actor.actor.id))
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let view = core_community::get_community_for_viewer(state.db(), &name, &viewer_communities)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let summary = match view {
        CommunityView::Full(community) => {
            let is_member = match &current.0 {
                Some(actor) => core_community::is_member(state.db(), community.id, actor.actor.id)
                    .await
                    .map_err(|e| ApiError::new(e).with_request_id(&headers))?,
                None => false,
            };
            community_summary(&community, state.id_codec(), is_member)
        }
        // Kapak: göremeyen okuyucuya sayaçlar sıfırlanmış özet; üyelik
        // sorusunun cevabı zaten "hayır".
        CommunityView::Cover(community) => community_summary(&community, state.id_codec(), false),
    }
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(summary))
}

/// `PATCH /communities/{name}` → `200`. Yalnızca sahibi veya bu toplulukta
/// `community.edit` sahibi; değilse `403`. `visibility` verilirse tek yönlü
/// kural uygulanır: public → private serbest, private → public `400`.
#[utoipa::path(
    patch,
    path = "/communities/{name}",
    tag = "communities",
    summary = "Edit a community",
    description = "Callable by the owner or a holder of `community.edit` for this community. \
        `visibility` is optional and one-way: a public community may become private, never the \
        reverse.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
    ),
    request_body = UpdateCommunityRequest,
    responses(
        (status = 200, description = "Updated community", body = CommunitySummary),
        Unauthorized,
        Forbidden,
        NotFound,
        ValidationFailed,
        RateLimited,
    )
)]
async fn update_community(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateCommunityRequest>,
) -> Result<Json<CommunitySummary>, ApiError> {
    if req.description.is_none() && req.visibility.is_none() {
        return Err(ApiError::new(Error::Validation(
            "provide at least one of description or visibility".to_owned(),
        ))
        .with_request_id(&headers));
    }

    let visibility = req
        .visibility
        .as_deref()
        .map(core_community::CommunityVisibility::parse)
        .transpose()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let community = core_community::update_community(
        state.db(),
        current.actor.id,
        &current.permissions,
        &name,
        req.description.as_deref(),
        visibility,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let is_member = core_community::is_member(state.db(), community.id, current.actor.id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let summary = community_summary(&community, state.id_codec(), is_member)
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(summary))
}

/// `POST /communities/{name}/join` → `204`. İdempotent.
#[utoipa::path(
    post,
    path = "/communities/{name}/join",
    tag = "communities",
    summary = "Join a community",
    description = "Instant and idempotent for public communities. Being a member already is not an error.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
    ),
    responses(
        (status = 204, description = "Now a member (or already was)"),
        Unauthorized,
        NotFound,
        ValidationFailed,
        RateLimited,
    )
)]
async fn join_community(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    core_community::join_community(state.db(), current.actor.id, &name)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /communities/{name}/join` → `204`. İdempotent; sahip ayrılırsa
/// devralma (ya da kapsama kapanış) tetiklenir.
#[utoipa::path(
    delete,
    path = "/communities/{name}/join",
    tag = "communities",
    summary = "Leave a community",
    description = "Idempotent. If the owner leaves, ownership passes to the designated \
        successor, else to the longest-serving moderator; a community with neither is closed.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
    ),
    responses(
        (status = 204, description = "No longer a member (or never was)"),
        Unauthorized,
        NotFound,
        ValidationFailed,
        RateLimited,
    )
)]
async fn leave_community(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    core_community::leave_community(state.db(), current.actor.id, &name)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /communities/{name}/members?cursor=&limit=` → `200`. En uzun süredir
/// üye olan önce.
#[utoipa::path(
    get,
    path = "/communities/{name}/members",
    tag = "communities",
    summary = "List a community's members",
    description = "Longest-serving member first (`joined_at` ascending).",
    params(
        ("name" = String, Path, description = "Community name"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Member list, with a cursor", body = CommunityMemberListResponse),
        NotFound,
        RateLimited,
    )
)]
async fn list_members(
    current: OptionalActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Result<Json<CommunityMemberListResponse>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    // Özel topluluğun üye listesi public bir yüzey değil; göremeyen
    // okuyucuya core `403` döner (bkz. `list_members` dokümanı).
    let viewer_communities = state
        .viewer_communities(current.0.as_ref().map(|actor| actor.actor.id))
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let page = core_community::list_members(state.db(), &name, cursor, limit, &viewer_communities)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let members = page
        .items
        .iter()
        .map(|member| member_summary(member, state.id_codec()))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(CommunityMemberListResponse {
        members,
        next_cursor: page.next_cursor.map(|c| state.cursor_codec().encode(&c)),
    }))
}

/// `DELETE /communities/{name}/members/{username}` → `204`. Bir üyeyi
/// topluluktan atar; `member.kick` yetkisi (o topluluk kapsamında) gerekir.
#[utoipa::path(
    delete,
    path = "/communities/{name}/members/{username}",
    tag = "communities",
    summary = "Kick a member from a community",
    description = "Requires `member.kick` scoped to this community. The owner cannot be kicked. A non-member returns 404.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
        ("username" = String, Path, description = "Username of the member to kick"),
    ),
    responses(
        (status = 204, description = "Member kicked"),
        Unauthorized,
        Forbidden,
        NotFound,
        ValidationFailed,
        RateLimited,
    )
)]
async fn kick_member(
    current: CurrentActor,
    State(state): State<AppState>,
    Path((name, username)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    core_community::kick_member(
        state.db(),
        current.actor.id,
        &current.permissions,
        &name,
        &username,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// Bir [`MemberEntry`]'yi yanıt DTO'suna çevirir.
fn member_summary(
    entry: &MemberEntry,
    id_codec: &IdCodec,
) -> Result<CommunityMemberSummary, Error> {
    // Üye listesindeki actor'ler avatarsız (bkz. `ActorRecord`: avatar
    // taşımıyor) — içerik yazarlarındaki aynı kapsam kararı.
    Ok(CommunityMemberSummary {
        actor: actor_summary(&entry.actor, None, id_codec)?,
        joined_at: entry.joined_at.to_rfc3339(),
    })
}

/// Bir topluluk `(id, ad)` çiftini [`CommunityRefSummary`]'e çevirir
/// (davet/başvuru DTO'ları için).
fn community_ref_summary(
    id: i64,
    name: &str,
    id_codec: &IdCodec,
) -> Result<CommunityRefSummary, Error> {
    Ok(CommunityRefSummary {
        id: id_codec
            .encode::<CommunityIdKind>(id)
            .map_err(|e| Error::Internal(format!("could not encode community id: {e}")))?,
        name: name.to_owned(),
    })
}

/// Bir [`Invitation`]'ı yanıt DTO'suna çevirir.
fn invitation_summary(
    invitation: &Invitation,
    id_codec: &IdCodec,
) -> Result<InvitationSummary, Error> {
    Ok(InvitationSummary {
        id: id_codec
            .encode::<InvitationIdKind>(invitation.id)
            .map_err(|e| Error::Internal(format!("could not encode invitation id: {e}")))?,
        community: community_ref_summary(
            invitation.community.id,
            &invitation.community.name,
            id_codec,
        )?,
        invited_by: actor_summary(&invitation.invited_by, None, id_codec)?,
        created_at: invitation.created_at.to_rfc3339(),
    })
}

/// Bir [`Application`]'ı yanıt DTO'suna çevirir.
fn application_summary(
    application: &Application,
    id_codec: &IdCodec,
) -> Result<ApplicationSummary, Error> {
    Ok(ApplicationSummary {
        id: id_codec
            .encode::<ApplicationIdKind>(application.id)
            .map_err(|e| Error::Internal(format!("could not encode application id: {e}")))?,
        community: community_ref_summary(
            application.community.id,
            &application.community.name,
            id_codec,
        )?,
        applicant: actor_summary(&application.applicant, None, id_codec)?,
        reason: application.reason.clone(),
        status: application.status.as_str().to_owned(),
        created_at: application.created_at.to_rfc3339(),
        resolved_at: application.resolved_at.map(|t| t.to_rfc3339()),
    })
}

/// `GET /communities/{name}/posts?sort=` → `200`, `404`. Etiket ucundaki
/// (`GET /tags/{name}/posts`) gövde şeklinin ve `?fields=` davranışının
/// birebir aynısı.
#[utoipa::path(
    get,
    path = "/communities/{name}/posts",
    tag = "communities",
    summary = "List a community's posts",
    description = "Supports the three sorts (`new`, `top`, `hot`). A community that exists but \
        has no live posts returns an empty list, not `404`.",
    params(
        ("name" = String, Path, description = "Community name"),
        ("sort" = Option<String>, Query, description = "`new`, `top`, or `hot`"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
        ("fields" = Option<String>, Query,
            description = "Comma-separated field names; applied to each post item"),
    ),
    responses(
        (status = 200, description = "Post list, with a cursor", body = PostListResponse),
        ValidationFailed,
        NotFound,
        RateLimited,
    )
)]
async fn list_community_posts(
    current: OptionalActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<CommunityPostsQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let sort = PostSort::parse(query.sort.as_deref())
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor_with(
        state.cursor_codec(),
        query.cursor.as_deref(),
        sort.sort_kind(),
        &headers,
    )?;

    // Topluluk akışı public bir yüzey DEĞİL (Faz 4A): özel bir topluluğu
    // göremeyen okuyucuya core `403` döner; görebilen üye içeriği okur.
    let viewer_communities = state
        .viewer_communities(current.0.as_ref().map(|actor| actor.actor.id))
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let page = core_community::list_posts_in_community(
        state.db(),
        &name,
        sort,
        cursor,
        limit,
        &viewer_communities,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let selected_fields = fields::parse_fields(query.fields.as_deref());
    let include_body_html = fields::wants_body_html(selected_fields.as_deref());

    let posts = page
        .items
        .iter()
        .map(|content| {
            let summary = content_summary_with_optional_body_html(
                content,
                state.id_codec(),
                include_body_html,
            )
            .map_err(|e| ApiError::new(e).with_request_id(&headers))?;
            fields::apply_fields(&summary, selected_fields.as_deref(), &headers)
        })
        .collect::<Result<Vec<Value>, ApiError>>()?;

    let next_cursor = page.next_cursor.map(|c| state.cursor_codec().encode(&c));

    Ok(Json(serde_json::json!({
        "posts": posts,
        "next_cursor": next_cursor,
    })))
}

// --- Kapanış ve devralma (Faz 4B-1) ------------------------------------

/// `POST /communities/{name}/close` → `204`. `community.close` (o topluluk
/// kapsamında ya da global) gerekir; zaten kapalıysa `404`.
#[utoipa::path(
    post,
    path = "/communities/{name}/close",
    tag = "communities",
    summary = "Close a community",
    description = "Requires `community.close` scoped to this community. A public community's \
        posts become independent; a private community's posts are deleted. Closing an already \
        closed community returns `404`.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
    ),
    responses(
        (status = 204, description = "Community closed"),
        Unauthorized,
        Forbidden,
        NotFound,
        RateLimited,
    )
)]
async fn close_community(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    core_community::close_community(
        state.db(),
        current.actor.id,
        &current.permissions,
        &name,
        None,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /communities/{name}/successor` → `204`. Yalnızca sahip; hedef actor
/// var olmalı ve silinmemiş olmalı.
#[utoipa::path(
    put,
    path = "/communities/{name}/successor",
    tag = "communities",
    summary = "Designate a community successor",
    description = "Owner only. The named actor inherits the community when the owner leaves or \
        deletes their account; without one, ownership falls to the longest-serving moderator. The \
        target must exist and not be deleted.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
    ),
    request_body = SuccessorRequest,
    responses(
        (status = 204, description = "Successor designated"),
        Unauthorized,
        Forbidden,
        NotFound,
        RateLimited,
    )
)]
async fn set_successor(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SuccessorRequest>,
) -> Result<StatusCode, ApiError> {
    core_community::set_successor(state.db(), current.actor.id, &name, &req.username)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

// --- Faz 4B-2: davetler ve başvurular ----------------------------------

/// `POST /communities/{name}/invitations` → `201`. `member.invite` (o
/// topluluk kapsamında) gerekir; public topluluk `400`.
#[utoipa::path(
    post,
    path = "/communities/{name}/invitations",
    tag = "communities",
    summary = "Invite an actor to a private community",
    description = "Requires `member.invite` scoped to this community. Private communities only; a \
        public community is joined instantly and returns `400`. The invitee is not a member until \
        they accept. A second pending invitation for the same actor returns `409`.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
    ),
    request_body = CreateInvitationRequest,
    responses(
        (status = 201, description = "Invitation created"),
        Unauthorized,
        Forbidden,
        NotFound,
        Conflict,
        ValidationFailed,
        RateLimited,
    )
)]
async fn create_invitation(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CreateInvitationRequest>,
) -> Result<StatusCode, ApiError> {
    core_community::invite_member(
        state.db(),
        current.actor.id,
        &current.permissions,
        &name,
        &req.username,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::CREATED)
}

/// `GET /me/invitations?cursor=&limit=` → `200`. Çağırana adreslenmiş
/// bekleyen davetler, en yeni önce.
#[utoipa::path(
    get,
    path = "/me/invitations",
    tag = "communities",
    summary = "List your pending invitations",
    description = "Pending invitations addressed to the requesting actor, newest first. An \
        invitation to a community that has since closed is not listed.",
    security(("api_key" = [])),
    params(
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Invitation list, with a cursor", body = InvitationListResponse),
        Unauthorized,
        RateLimited,
    )
)]
async fn list_my_invitations(
    current: CurrentActor,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Result<Json<InvitationListResponse>, ApiError> {
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_community::list_my_invitations(state.db(), current.actor.id, cursor, limit)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let invitations = page
        .items
        .iter()
        .map(|invitation| invitation_summary(invitation, state.id_codec()))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(InvitationListResponse {
        invitations,
        next_cursor: page.next_cursor.map(|c| state.cursor_codec().encode(&c)),
    }))
}

/// `POST /me/invitations/{id}/accept` → `204`.
#[utoipa::path(
    post,
    path = "/me/invitations/{id}/accept",
    tag = "communities",
    summary = "Accept an invitation",
    description = "The invitation must be addressed to the requesting actor and still pending. On \
        success the actor becomes a member of the community. A resolved invitation returns `409`; \
        another actor's invitation returns `404`.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The invitation's external id (`i_...`)"),
    ),
    responses(
        (status = 204, description = "Invitation accepted; now a member"),
        Unauthorized,
        NotFound,
        Conflict,
        RateLimited,
    )
)]
async fn accept_invitation(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let invitation_id = state
        .id_codec()
        .decode::<InvitationIdKind>(&id)
        .map_err(|_| ApiError::new(Error::NotFound("invitation")).with_request_id(&headers))?;

    core_community::accept_invitation(state.db(), current.actor.id, invitation_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /me/invitations/{id}/decline` → `204`.
#[utoipa::path(
    post,
    path = "/me/invitations/{id}/decline",
    tag = "communities",
    summary = "Decline an invitation",
    description = "The invitation must be addressed to the requesting actor and still pending. The \
        actor is not made a member. A resolved invitation returns `409`; another actor's \
        invitation returns `404`.",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The invitation's external id (`i_...`)"),
    ),
    responses(
        (status = 204, description = "Invitation declined"),
        Unauthorized,
        NotFound,
        Conflict,
        RateLimited,
    )
)]
async fn decline_invitation(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let invitation_id = state
        .id_codec()
        .decode::<InvitationIdKind>(&id)
        .map_err(|_| ApiError::new(Error::NotFound("invitation")).with_request_id(&headers))?;

    core_community::decline_invitation(state.db(), current.actor.id, invitation_id)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /communities/{name}/applications` → `201`. Private topluluğa
/// başvuru; `member.approve` sahiplerine bildirim gider.
#[utoipa::path(
    post,
    path = "/communities/{name}/applications",
    tag = "communities",
    summary = "Apply to a private community",
    description = "Private communities only; a public community is joined instantly and returns \
        `400`. The actor must not already be a member and must not be banned. The reason is \
        1-2000 characters. Every holder of `member.approve` scoped to the community is notified. \
        A second pending application returns `409`.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
    ),
    request_body = CreateApplicationRequest,
    responses(
        (status = 201, description = "Application submitted"),
        Unauthorized,
        NotFound,
        Conflict,
        ValidationFailed,
        RateLimited,
    )
)]
async fn create_application(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CreateApplicationRequest>,
) -> Result<StatusCode, ApiError> {
    core_community::apply_to_community(state.db(), current.actor.id, &name, &req.reason)
        .await
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(StatusCode::CREATED)
}

/// `GET /communities/{name}/applications?status=&cursor=&limit=` → `200`.
/// `member.approve` (o topluluk kapsamında) gerekir; en eski önce (iş
/// kuyruğu).
#[utoipa::path(
    get,
    path = "/communities/{name}/applications",
    tag = "communities",
    summary = "List a community's applications (the moderation queue)",
    description = "Requires `member.approve` scoped to this community. Oldest first, like a work \
        queue. `?status=pending|accepted|rejected` filters; without it every status is listed.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
        ("status" = Option<String>, Query, description = "`pending`, `accepted`, or `rejected`"),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`"),
        ("limit" = Option<String>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Application list, with a cursor", body = ApplicationListResponse),
        Unauthorized,
        Forbidden,
        NotFound,
        ValidationFailed,
        RateLimited,
    )
)]
async fn list_applications(
    current: CurrentActor,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<ApplicationListQuery>,
    headers: HeaderMap,
) -> Result<Json<ApplicationListResponse>, ApiError> {
    let status = query
        .status
        .as_deref()
        .map(ApplicationStatus::parse)
        .transpose()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;
    let limit = parse_limit(query.limit, &headers)?;
    let cursor = decode_cursor(state.cursor_codec(), query.cursor.as_deref(), &headers)?;

    let page = core_community::list_applications(
        state.db(),
        current.actor.id,
        &current.permissions,
        &name,
        status,
        cursor,
        limit,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    let applications = page
        .items
        .iter()
        .map(|application| application_summary(application, state.id_codec()))
        .collect::<Result<Vec<_>, Error>>()
        .map_err(|e| ApiError::new(e).with_request_id(&headers))?;

    Ok(Json(ApplicationListResponse {
        applications,
        next_cursor: page.next_cursor.map(|c| state.cursor_codec().encode(&c)),
    }))
}

/// `POST /communities/{name}/applications/{id}/accept` → `204`.
#[utoipa::path(
    post,
    path = "/communities/{name}/applications/{id}/accept",
    tag = "communities",
    summary = "Accept an application",
    description = "Requires `member.approve` scoped to this community. The application must belong \
        to this community and still be pending. On success the applicant becomes a member and is \
        notified.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
        ("id" = String, Path, description = "The application's external id (`p_...`)"),
    ),
    responses(
        (status = 204, description = "Application accepted; applicant is now a member"),
        Unauthorized,
        Forbidden,
        NotFound,
        Conflict,
        RateLimited,
    )
)]
async fn resolve_application_accept(
    current: CurrentActor,
    State(state): State<AppState>,
    Path((name, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    resolve_application(current, state, name, id, true, &headers).await
}

/// `POST /communities/{name}/applications/{id}/reject` → `204`.
#[utoipa::path(
    post,
    path = "/communities/{name}/applications/{id}/reject",
    tag = "communities",
    summary = "Reject an application",
    description = "Requires `member.approve` scoped to this community. The application must belong \
        to this community and still be pending. The applicant is not made a member; they are \
        notified of the result.",
    security(("api_key" = [])),
    params(
        ("name" = String, Path, description = "Community name"),
        ("id" = String, Path, description = "The application's external id (`p_...`)"),
    ),
    responses(
        (status = 204, description = "Application rejected"),
        Unauthorized,
        Forbidden,
        NotFound,
        Conflict,
        RateLimited,
    )
)]
async fn resolve_application_reject(
    current: CurrentActor,
    State(state): State<AppState>,
    Path((name, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    resolve_application(current, state, name, id, false, &headers).await
}

/// [`resolve_application_accept`] ve [`resolve_application_reject`]'in ortak
/// gövdesi: id'yi çözer, core'a devreder. İkisi ayrı handler çünkü accept ve
/// reject farklı URL yolları (aynı yolun iki metodu değil), `routes!(a, b)`
/// ile birleştirilemezler.
async fn resolve_application(
    current: CurrentActor,
    state: AppState,
    name: String,
    id: String,
    accept: bool,
    headers: &HeaderMap,
) -> Result<StatusCode, ApiError> {
    let application_id = state
        .id_codec()
        .decode::<ApplicationIdKind>(&id)
        .map_err(|_| ApiError::new(Error::NotFound("application")).with_request_id(headers))?;

    core_community::resolve_application(
        state.db(),
        current.actor.id,
        &current.permissions,
        &name,
        application_id,
        accept,
    )
    .await
    .map_err(|e| ApiError::new(e).with_request_id(headers))?;

    Ok(StatusCode::NO_CONTENT)
}
