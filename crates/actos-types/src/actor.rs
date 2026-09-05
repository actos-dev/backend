//! Request/response types for actor profiles and the directory/discovery
//! endpoints.
//!
//! The rule from the `actos-types` crate root applies here too: this module
//! carries no server dependency, only `serde`.

use serde::{Deserialize, Deserializer, Serialize};

use crate::auth::ActorSummary;

/// The statistics block in the `GET /actors/{username}` response.
///
/// Computed with a single aggregate query over the contents table (live rows
/// only), not with one query per actor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActorStats {
    pub post_count: i64,
    pub comment_count: i64,
    pub total_score: i64,
}

/// Response body of `GET /actors/{username}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActorProfileResponse {
    pub actor: ActorSummary,
    pub stats: ActorStats,
}

/// Request body of `PATCH /actors/me`.
///
/// **The `Option<Option<T>>` pattern — partial update:** if the field is
/// absent from the JSON the outer `Option` stays `None` ("leave it alone");
/// if it is sent explicitly as `null` the outer `Option` becomes `Some(None)`
/// ("clear it"); if a value is sent it becomes `Some(Some(v))` ("update it").
/// A plain `#[serde(default)]` + `Option<T>` cannot tell these three apart —
/// `null` and "field not sent at all" would collapse into the same `None`,
/// and a client could never clear a field.
///
/// `double_option` achieves this as follows: thanks to `#[serde(default)]`,
/// when the field is absent from the JSON the `deserialize_with` function is
/// **never called** and the field stays `Default::default()` (that is,
/// `None`). When the field is present — even with a `null` value — the
/// function is called, and the inner `Option<T>::deserialize` already draws
/// the right distinction (`null` → `None`, a value → `Some(value)`); we wrap
/// that in a `Some(...)` to add the outer layer.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateProfileRequest {
    #[serde(default, deserialize_with = "double_option")]
    pub display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub bio: Option<Option<String>>,
    /// The **external** id of the upload to use as the new avatar (`f_...`
    /// — the `id` returned by `POST /uploads`). Same `Option<Option<T>>`
    /// pattern as `display_name`/`bio`: if the field is absent the avatar is
    /// left alone, if `null` is sent the avatar is removed, and if an id is
    /// sent that upload becomes the avatar.
    ///
    /// Before accepting the id the server checks three things: that the
    /// upload exists (`404`), that it **belongs to the calling actor**
    /// (`403`), and that it is **not yet attached** to any content (`409` — a
    /// file already attached to a post or comment cannot be reused as an
    /// avatar, since two different lifecycles would collide on one row).
    #[serde(default, deserialize_with = "double_option")]
    pub avatar: Option<Option<String>>,
}

fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// Response body of `PATCH /actors/me` — the updated profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateProfileResponse {
    pub actor: ActorSummary,
}

/// Request body of `DELETE /actors/me`.
///
/// Because deleting an account cannot be undone, confirmation requires a
/// second proof beyond the credential (the API key): a valid recovery code.
/// The code is consumed in the process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeleteAccountRequest {
    pub recovery_code: String,
}

/// The shared response shape of the actor-listing endpoints (`followers`,
/// `following`, the discovery directory): one page of actors plus the cursor
/// for the next page, if any.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActorListResponse {
    pub actors: Vec<ActorSummary>,
    /// `None` means this is the last page.
    pub next_cursor: Option<String>,
}
