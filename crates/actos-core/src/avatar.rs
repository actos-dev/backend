//! An actor's avatar: `POST`/`DELETE /actors/me/avatar`.
//!
//! This is deliberately its own module, separate from [`crate::attachment`].
//! An avatar used to be created by `POST /uploads` (an `attachments` row
//! with `content_id` left `NULL` forever) and then attached to
//! `actors.avatar_object_key` by `crate::actor::update_profile` via
//! `attachment::resolve_as_avatar`. That made an avatar the ONE case where
//! `attachments.content_id` was permanently `NULL` by design rather than
//! "not yet attached" — which stood in the way of ever making that column
//! `NOT NULL`. This module removes the whole detour: an avatar is not an
//! attachment. It goes straight into and out of `actors.avatar_object_key`,
//! and this module is the only code that writes that column.
//!
//! **No storage quota accounting here, on purpose.** `crate::attachment::
//! create_attachment`'s quota (`crate::config::StorageQuotaConfig`) defends
//! against an actor accumulating an unbounded number of uploads over time.
//! An avatar can't accumulate: there is exactly one object key live per
//! actor at any moment (this module always deletes the previous one when
//! setting a new one, see [`set_avatar`]), so its total contribution to an
//! actor's storage footprint is capped at one file, forever. Running the
//! same `SUM(byte_size)` accounting for a single bounded file would be
//! bookkeeping with no failure mode it actually prevents.
//!
//! **No thumbnail here, unlike [`crate::attachment`].** The attachment
//! pipeline generates a small preview because a content item's attachments
//! are shown at gallery/grid scale, where a full-size fetch would be
//! wasteful. An avatar is displayed at one small, fairly consistent size
//! everywhere it appears (profile header, actor summaries in lists) — the
//! [`crate::media`] pipeline already caps it at
//! [`crate::media::MAX_DIMENSION`], and a second, even smaller derivative
//! isn't worth the extra `put_object` call and the extra object to keep in
//! sync on every replace/delete.

use sqlx::PgPool;

use crate::{
    attachment,
    error::{Error, Result},
    id::IdCodec,
    media::{self, ProcessedImage},
    storage::Storage,
};

/// `POST /actors/me/avatar`: validate + normalize the uploaded bytes (see
/// [`crate::media::process_image`]), store the result under a fresh object
/// key, point `actors.avatar_object_key` at it, and delete whatever object
/// key it pointed at before, if any. Returns the new object key.
///
/// **Ordering, and why:** the new object is uploaded to storage BEFORE any
/// database write, for the same reason as `attachment::create_attachment` —
/// if the database update then failed, the worst case is one unreferenced
/// object in storage (invisible, harmless, cleaned up by nothing today
/// since avatars aren't tracked by `attachment::cleanup_orphaned`, but also
/// never linked to from anywhere a client could reach). The reverse order
/// (write the database row, then upload) would risk the opposite: a live
/// `avatar_object_key` pointing at an object that was never actually
/// written, which every reader of the profile would see as a broken image.
///
/// The **previous** object is deleted only AFTER the database transaction
/// that replaces it has committed, and the deletion failure is logged and
/// swallowed rather than propagated — deleting it before commit would risk
/// losing the old avatar if the commit then failed for an unrelated reason
/// (e.g. the actor was deleted concurrently), and a failed deletion after a
/// successful commit just leaves one harmless orphaned object behind, the
/// same trade-off `attachment::delete_attachment` already makes.
///
/// **`SELECT ... FOR UPDATE` before the `UPDATE`:** the transaction needs
/// the row's CURRENT `avatar_object_key` (to know what to delete
/// afterwards) at the same instant it is about to overwrite it. Without the
/// lock, two concurrent calls for the same actor (e.g. a double-submit)
/// could both read the same "previous" key, both overwrite the column, and
/// then both correctly delete that one old object — but if a third
/// concurrent read landed between the two `UPDATE`s, only one of the two
/// uploaded objects would end up referenced, and the OTHER would silently
/// leak. Locking serializes the two calls: the second one's `SELECT`
/// blocks until the first commits, so it correctly observes the first
/// call's new key as ITS "previous" — a leak is avoided, not just a data
/// race.
///
/// # Errors
/// If the bytes don't pass validation, [`Error::UnsupportedMedia`] /
/// [`Error::Validation`] (see [`crate::media::process_image`]); if storage
/// is unreachable, [`Error::Internal`]; if the actor row is unexpectedly
/// missing (should not happen after `crate::auth::authenticate`),
/// [`Error::Internal`]; database error, [`Error::Database`].
pub async fn set_avatar(
    pool: &PgPool,
    storage: &Storage,
    id_codec: &IdCodec,
    actor_id: i64,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<String> {
    let islenmis: ProcessedImage = media::process_image(bytes, max_bytes)?;

    // Same key shape and same rationale as `attachment::create_attachment`
    // (server-generated, never derived from client input) — see that
    // function's helper, reused here rather than duplicated.
    let object_key = attachment::object_key_uret(id_codec, actor_id)?;

    storage
        .put_object(&object_key, islenmis.data, ProcessedImage::mime_type())
        .await?;

    let mut tx = pool.begin().await?;

    let previous_object_key = sqlx::query_scalar!(
        r#"SELECT avatar_object_key FROM actors WHERE id = $1 AND deleted_at IS NULL FOR UPDATE"#,
        actor_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        Error::Internal(format!(
            "could not set avatar: actor {actor_id} not found (should not happen after authenticate())"
        ))
    })?;

    sqlx::query!(
        r#"UPDATE actors SET avatar_object_key = $2 WHERE id = $1"#,
        actor_id,
        object_key,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    if let Some(old_key) = previous_object_key {
        delete_object_logged(storage, &old_key).await;
    }

    Ok(object_key)
}

/// `DELETE /actors/me/avatar`: clear `actors.avatar_object_key` and delete
/// the object it pointed at. A no-op (still `Ok(())`) if the actor had no
/// avatar set — `DELETE` on an already-absent resource is idempotent by
/// convention across this API.
///
/// Same commit-then-delete ordering as [`set_avatar`], same reasoning.
///
/// # Errors
/// If the actor row is unexpectedly missing (should not happen after
/// `crate::auth::authenticate`), [`Error::Internal`]; database error,
/// [`Error::Database`].
pub async fn clear_avatar(pool: &PgPool, storage: &Storage, actor_id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;

    let previous_object_key = sqlx::query_scalar!(
        r#"SELECT avatar_object_key FROM actors WHERE id = $1 AND deleted_at IS NULL FOR UPDATE"#,
        actor_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        Error::Internal(format!(
            "could not clear avatar: actor {actor_id} not found (should not happen after authenticate())"
        ))
    })?;

    sqlx::query!(
        r#"UPDATE actors SET avatar_object_key = NULL WHERE id = $1"#,
        actor_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    if let Some(old_key) = previous_object_key {
        delete_object_logged(storage, &old_key).await;
    }

    Ok(())
}

/// Deletes a single object, logging and swallowing failure — the same
/// "database side is already final, a leftover object is invisible garbage"
/// trade-off as `attachment`'s own best-effort deletion helper. Unlike
/// `attachment::Attachment`, an avatar has no thumbnail counterpart to also
/// delete (see the module doc's "No thumbnail here" section).
async fn delete_object_logged(storage: &Storage, object_key: &str) {
    if let Err(err) = storage.delete_object(object_key).await {
        tracing::warn!(object_key = %object_key, error = %err, "could not delete avatar object");
    }
}
