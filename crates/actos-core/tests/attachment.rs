//! `actos_core::attachment` entegrasyon testleri.
//!
//! **Avatar exclusion coverage removed.** This file used to pin
//! `cleanup_orphaned`'s `NOT EXISTS (... actors.avatar_object_key ...)`
//! guard, which protected an avatar's bookkeeping row (permanently
//! `content_id IS NULL`) from being swept up as an ordinary orphan. Avatars
//! no longer create a row in `attachments` at all — they get their own
//! endpoints, `POST`/`DELETE /actors/me/avatar` (see `actos_core::avatar`),
//! writing `actors.avatar_object_key` directly — so the guard itself was
//! removed from `cleanup_orphaned`'s query, and there is nothing left here
//! to test. See `migrations/0026_drop_avatar_attachments.up.sql` for how
//! the already-existing avatar rows were retired safely alongside that
//! change.
//!
//! What remains is `cleanup_orphaned`'s ordinary age-threshold behavior.
//!
//! **Depolamaya gerçekten bağlanılmıyor:** `crates/actos-api/tests/*`'teki
//! aynı desen — `Storage`, bilerek erişilemez bir adrese (`http://
//! 127.0.0.1:1`) işaret ediyor. `attachment::cleanup_orphaned`'in depolama
//! silme çağrıları başarısız olsa bile yalnızca loglanıp yutuluyor (bkz. o
//! fonksiyonun dokümanı), veritabanı tarafı bundan etkilenmiyor — bu testin
//! ilgilendiği tam olarak veritabanı tarafı.

use actos_core::{
    Storage, attachment,
    auth::{self as core_auth, ActorType},
    config::StorageConfig,
};
use chrono::{Duration, Utc};
use sqlx::PgPool;

#[allow(clippy::expect_used)]
fn test_storage() -> Storage {
    Storage::new(&StorageConfig {
        endpoint: "http://127.0.0.1:1".to_owned(),
        region: "us-east-1".to_owned(),
        bucket: "test-bucket".to_owned(),
        access_key: "test".to_owned(),
        secret_key: "test".to_owned(),
        public_base_url: "http://127.0.0.1:1/test-bucket".to_owned(),
    })
}

/// Creates an actor without going through the authentication path
/// (Argon2id, 10 recovery codes) — see the same rationale in
/// `crates/actos-core/tests/interaction.rs`. `register` uses
/// `core_auth::register` here rather than a raw `INSERT` like
/// `crate::actor.rs` does, because a one-line helper is enough in this
/// file, no need to write out the `actor_type` enum by hand.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> i64 {
    core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli")
        .actor
        .id
}

/// Inserts a row into the `attachments` table with `content_id IS NULL`
/// and `created_at` set back by `age_hours` hours — to test
/// `cleanup_orphaned`'s age threshold without waiting in real time (the
/// "backdate it" pattern, via a direct `INSERT ... created_at`).
#[allow(clippy::expect_used)]
async fn seed_orphan_attachment(
    pool: &PgPool,
    actor_id: i64,
    object_key: &str,
    age_hours: i64,
) -> i64 {
    let created_at = Utc::now() - Duration::hours(age_hours);
    // `ck_attachments_checksum_sha256_format`: 64 karakter küçük harf hex —
    // gerçek bir dosya yok, sabit bir değer format kısıtını karşılıyor.
    let checksum = "0".repeat(64);

    sqlx::query!(
        r#"
        INSERT INTO attachments
            (actor_id, object_key, byte_size, mime_type, width, height, checksum_sha256, created_at)
        VALUES ($1, $2, 1024, 'image/webp', 10, 10, $3, $4)
        RETURNING id
        "#,
        actor_id,
        object_key,
        checksum,
        created_at,
    )
    .fetch_one(pool)
    .await
    .expect("attachment eklenebilmeli")
    .id
}

#[allow(clippy::expect_used)]
async fn attachment_var_mi(pool: &PgPool, id: i64) -> bool {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM attachments WHERE id = $1) AS "exists!""#,
        id,
    )
    .fetch_one(pool)
    .await
    .expect("sorgulanabilmeli")
}

/// An ordinary upload that hasn't crossed [`attachment::ORPHAN_MAX_AGE_HOURS`]
/// yet must be left alone — `cleanup_orphaned` only sweeps rows strictly
/// older than the threshold.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn taze_yukleme_yasi_dolmadan_temizlige_takilmiyor(pool: PgPool) {
    let storage = test_storage();
    let actor_id = seed_actor(&pool, "taze_yukleyici").await;
    let key = format!("{actor_id}/taze.webp");
    let id = seed_orphan_attachment(&pool, actor_id, &key, 1).await;

    let silinen = attachment::cleanup_orphaned(&pool, &storage)
        .await
        .expect("temizlik çalışabilmeli");

    assert_eq!(silinen, 0);
    assert!(attachment_var_mi(&pool, id).await);
}
