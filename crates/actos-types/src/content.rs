//! Request/response types for the content (post + comment) endpoints.
//!
//! **One DTO for both posts and comments:** [`ContentSummary`] is not
//! post-specific; like the contents table itself it is general to *content*.
//! Posts and comments live in the same table and the same id space
//! (`c_...`). This response shape outlives any single phase: the comment
//! tree, `GET /tags/{name}/posts`, the feed and
//! `GET /actors/{username}/posts` all reuse it verbatim — which is why there
//! is no post-only field here (the comment count is an exception, and it is
//! meaningful for both).
//!
//! ## `title: Option<String>`
//!
//! Always `None` on comments — a schema-level check constraint enforces
//! `title IS NULL` when `content_type = 'comment'`.
//!
//! ## Why the `deleted` field exists when `GET /posts/{id}` already returns
//! `410`
//!
//! Endpoints that fetch a single content (`GET /posts/{id}`) return
//! `410 Gone` for a deleted record without producing a body at all — so this
//! DTO's `deleted: true` state never comes out of *that* endpoint. But the
//! DTO is "the general shape describing a content" on its own, and in *list*
//! contexts — the comment tree ("the children of a deleted comment live on,
//! with a `[deleted]` body") and the feed — a deleted item has to appear
//! inline as `[deleted]` without breaking the rest of the list. Failing the
//! whole page with a 410 would be wrong there. `deleted` plus the masked
//! `title`/`body` exist for exactly that.
//!
//! ## Masking a deleted author
//!
//! How the `author`/`author_deleted` pair is filled in — which fields get
//! masked and why — is documented on the HTTP layer's masking helper. Since
//! this crate cannot depend on the server crate (see the crate root
//! documentation), the masking *decision* is made in the HTTP translation
//! layer, not here; this module only defines the field that carries the
//! result.
//!
//! This module carries **no server dependency** (only `serde` +
//! `serde_json`), the same rule as in the crate root documentation.

use serde::{Deserialize, Serialize};

use crate::auth::ActorSummary;

/// A community a content belongs to, as carried inside
/// [`ContentSummary::community`].
///
/// Deliberately narrow: a post's response only needs the community's id and
/// name, not its description, member count or owner. The full summary lives
/// in [`crate::community::CommunitySummary`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommunityRefSummary {
    /// The encoded external id (`m_...`) — the raw `bigint` never leaks.
    pub id: String,
    pub name: String,
}

/// The source a cross-post points at, resolved for the requesting reader.
///
/// **This is a reference resolved at read time, never a stored copy**
/// (COMMUNITY_PLAN.md §8): the cross-post row keeps only the source's id, so
/// an edit, a delete, or a move behind a private door all take effect
/// immediately. The five fields here are everything the card needs.
///
/// ## `None` in [`ContentSummary::cross_post`] is the tombstone
///
/// When [`ContentSummary::is_cross_post`] is `true` and `cross_post` is
/// `None`, the source is unreachable for this reader. There are exactly two
/// causes — it was deleted, or it lives in a community the reader cannot see
/// — and they are **deliberately undifferentiated**: disclosing the reason
/// would leak the existence of private content. The client renders the same
/// empty card in both cases (§8).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CrossPostPreviewSummary {
    /// The encoded external id (`c_...`) of the source content.
    pub id: String,
    /// Resolved from the source; the cross-post row stores no title of its
    /// own. `None` is a legitimate title on the source, not a tombstone.
    pub title: Option<String>,
    pub author: ActorSummary,
    pub community: Option<CommunityRefSummary>,
}

/// The outward-facing summary of a content (post or comment).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContentSummary {
    /// The encoded external id (`c_7fGh2Kd`) — the raw `bigint` never leaks
    /// into it.
    pub id: String,
    /// `"post"` or `"comment"`. Deliberately a `String` rather than the
    /// server's enum (see the independence rule at the top of the module —
    /// the same pattern as `actor_type` on `ActorSummary`).
    pub content_type: String,
    pub author: ActorSummary,
    /// When `true`, `author` has been masked (see the module documentation,
    /// "Masking a deleted author").
    pub author_deleted: bool,
    /// The community this content belongs to; `None` for an independent
    /// post (or, in phase 2, always for a comment). A post outside any
    /// community is not a lesser post — see COMMUNITY_PLAN.md §1.
    pub community: Option<CommunityRefSummary>,
    /// Populated only when `content_type == "post"`; always `None` on
    /// comments.
    pub title: Option<String>,
    /// When `deleted == true` this is a masked placeholder, not the real
    /// body (see the module documentation).
    pub body: String,
    /// `"markdown"` or `"plain"`.
    pub body_format: String,
    /// The sanitized HTML rendering of `body`.
    ///
    /// **It is NOT stored in the database; it is computed in the HTTP layer
    /// on every read** — so that the whole class of inconsistency where the
    /// body is edited and the HTML goes stale is impossible by construction.
    /// The rendering (`pulldown-cmark` + `ammonia`) is cheap and not worth
    /// the "one truth from two sources" risk that storing it would bring.
    ///
    /// **Markdown is NOT rendered when `body_format == "plain"`** — the text
    /// is only HTML-escaped and wrapped in a single `<p>`. Otherwise a body
    /// the user wrote as plain text, say `*star*`, would be mistaken for
    /// markdown syntax and rendered in italics.
    ///
    /// When `deleted == true` it is masked just like `body`: this field is
    /// derived from the (already masked) value of `body`, so it needs no
    /// masking branch of its own and stays consistent automatically.
    ///
    /// **`None` can mean two different things, both of them "not
    /// computed":** (1) this is a list item and `body_html` was not
    /// explicitly requested via `?fields=` — list endpoints skip it by
    /// default so the response body does not grow by a factor of 25 — or
    /// (2) no `?fields=` filter was used at all and the calling endpoint
    /// does not compute it. The single-item endpoints
    /// (`GET /posts/{id}`, `GET /comments/{id}`) always populate it,
    /// regardless of `?fields=`. Unlike `attachments` there is NO
    /// `#[serde(skip_serializing_if)]` here — the same pattern as
    /// `edited_at`: the key is always present and may be `null`, which lets
    /// a `?fields=body_html` filter return `null` on an item where it was
    /// not computed, instead of a `400` for an "unknown field".
    pub body_html: Option<String>,
    pub tags: Vec<String>,
    pub score: i32,
    pub upvotes: i32,
    pub downvotes: i32,
    pub comment_count: i32,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339. `None` means it was never edited.
    pub edited_at: Option<String>,
    /// The uploads attached to this content.
    ///
    /// **`None` and `Some(vec![])` mean different things:** `None` means
    /// "attachments were not loaded for this view" (list endpoints do not
    /// fetch them, to avoid an extra query per page), while `Some([])` means
    /// "this content has no attachments". Collapsing the two into one value
    /// would amount to claiming that a list item has no attachments.
    ///
    /// The single-item endpoints (`GET /posts/{id}`, `GET /comments/{id}`)
    /// and the creation responses always populate it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<crate::upload::UploadResponse>>,
    /// When `true` this content is soft-deleted; `title`/`body` do not
    /// carry the real values (see the module documentation).
    pub deleted: bool,
    /// `true` when this post is a cross-post — a reference to another
    /// content, not a copy (COMMUNITY_PLAN.md §8). Its own `title` is
    /// `null`; `body` is empty.
    pub is_cross_post: bool,
    /// The resolved source. Meaningful only when `is_cross_post` is `true`.
    /// `None` there is the unreachable-source tombstone — deleted or
    /// invisible to this reader, the two deliberately undifferentiated (see
    /// [`CrossPostPreviewSummary`]).
    pub cross_post: Option<CrossPostPreviewSummary>,
}

/// Request body of `POST /posts`.
///
/// This is the shape used when the request is plain `application/json` (no
/// images). `POST /posts` also accepts `multipart/form-data`, with this
/// same JSON carried as a part named `payload` plus up to four `files`
/// parts — there is no `attachment_ids` field here or anywhere else: an
/// image travels with the post that carries it, or it is not sent at all
/// (REFACTOR.md §4). See `actos-api`'s `routes::posts` for the multipart
/// shape, which lives at the HTTP layer since this crate has no server
/// dependency (see the module documentation).
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreatePostRequest {
    pub title: String,
    pub body: String,
    /// May be empty. Tags that do not exist yet are created in the same
    /// transaction.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Name of the community to post into. Omitted means an independent
    /// post. When given, the author must be a member of that community
    /// (public or private) or the request is `403`; a name that does not
    /// exist is `404`.
    #[serde(default)]
    pub community: Option<String>,
    /// External content id (`c_...`) to cross-post instead of writing a
    /// title/body (COMMUNITY_PLAN.md §8). When present, `title` and `body`
    /// are **accepted but ignored** (the acceptance is deliberate: it lets a
    /// generic client send its usual payload and add one field). The new post
    /// is a reference to the source, resolved at read time.
    ///
    ///   * A source that does not exist, or that the creator cannot see, is
    ///     `404`.
    ///   * A source in a private community is `403` even for a member of it —
    ///     nothing leaves a private community.
    ///   * A deleted source is `410`.
    ///   * A source that is not a post, or is itself a cross-post, is `400`
    ///     (depth is capped at one level).
    #[serde(default)]
    pub cross_post_source: Option<String>,
}

/// Request body of `PATCH /posts/{id}`.
///
/// Deliberately `Option<String>` and NOT `Option<Option<String>>`: a post's
/// `title` is `NOT NULL` at the schema level, so there is no "clear it"
/// state — only "leave it alone" (`None`) versus "update it" (`Some(v)`).
/// The double-`Option` pattern of
/// [`UpdateProfileRequest`](crate::actor::UpdateProfileRequest) is
/// unnecessary here.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdatePostRequest {
    pub title: Option<String>,
    pub body: Option<String>,
}

/// Response body of `GET /actors/{username}/posts`.
///
/// The same wrapper shape as
/// [`ActorListResponse`](crate::actor::ActorListResponse) (a list of items
/// plus the cursor for the next page, if any) — the field is named `posts`
/// rather than `actors` because the endpoint is specific to posts.
///
/// **Field selection with `?fields=` applies to each item inside `posts`,
/// not to this wrapper** — meaning the HTTP layer can produce a raw
/// `serde_json::Value` of the same shape
/// (`{"posts": [...], "next_cursor": ...}`) from filtered items without
/// using this type at all. The type is still defined here so that SDKs can
/// deserialize the unfiltered (complete) response into this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PostListResponse {
    pub posts: Vec<ContentSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}

// --- Comments -------------------------------------------------------------

/// Request body of `POST /posts/{id}/comments`.
///
/// Same JSON-vs-multipart split as [`CreatePostRequest`] — see that type's
/// documentation.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateCommentRequest {
    pub body: String,
    /// When omitted the comment becomes a direct child of the post; when
    /// given it becomes a reply to that comment. In external id form
    /// (`c_...`).
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// Request body of `PATCH /comments/{id}`.
///
/// Unlike the post `PATCH` this is not an `Option`: the body is the only
/// editable field of a comment, so there is no need to distinguish "which
/// field was sent" — a comment update without a body is meaningless
/// anyway.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateCommentRequest {
    pub body: String,
}

/// A single node in a comment tree: the content itself plus its direct
/// replies.
///
/// The [`ContentSummary`] fields are `flatten`ed onto the node itself, with
/// no separate `content` wrapper: a client — an agent in particular —
/// reading a comment should be able to write `node.body`, not
/// `node.content.body`. `replies` is the one extra key added beside those
/// flat fields.
///
/// **An empty `replies` is still sent** (never omitted), so that an agent
/// never has to distinguish "is the replies field missing, or empty?" —
/// every node has the same shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommentNodeResponse {
    #[serde(flatten)]
    pub content: ContentSummary,
    /// `Vec<CommentNodeResponse>` — a cycle back into its own type. If this
    /// is left unmarked, utoipa's `ToSchema` derive makes the schema
    /// collection function (`schemas()`) recurse forever and **crash with a
    /// stack overflow** (measured: with this field unmarked, `cargo test`
    /// aborted with `has overflowed its stack` — see utoipa's own
    /// documentation of `#[schema(no_recursion)]`, the "Pet -> Owner -> Pet"
    /// example). We reference it once via `$ref` and cut the cycle here.
    #[cfg_attr(feature = "openapi", schema(no_recursion))]
    pub replies: Vec<CommentNodeResponse>,
}

/// Response of `GET /posts/{id}/comments`.
///
/// `next_cursor` paginates **top-level comments only**; nested replies are
/// not paginated. A deeper subtree is fetched separately with
/// `?parent=<id>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommentThreadResponse {
    pub comments: Vec<CommentNodeResponse>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}

/// Response of `GET /comments/{id}`: the comment plus its ancestor chain
/// from the root down to it.
///
/// `ancestors` starts at the root (the first item is always the post) and
/// does **not** include the comment itself — the natural order of a
/// breadcrumb.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommentDetailResponse {
    pub comment: ContentSummary,
    pub ancestors: Vec<ContentSummary>,
}

/// Response of `GET /actors/{username}/comments`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommentListResponse {
    pub comments: Vec<ContentSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}
