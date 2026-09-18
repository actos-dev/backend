//! `actos_core::moderation` entegrasyon testleri — özellikle banla-ve-sil
//! arka plan işçisi (`run_pending_jobs`).
//!
//! HTTP katmanı `crates/actos-api/tests/communities_api.rs`'te; burada
//! domain fonksiyonu doğrudan çağrılıyor. Kurulum yardımcıları
//! `tests/notification.rs` ile aynı desen (her entegrasyon test binary'si
//! bağımsız derlendiği için yardımcılar tekrar tanımlanıyor).

use actos_core::{
    Storage,
    auth::{ActorRecord, ActorType, Grant, Permission, PermissionScope},
    community::{self, CommunityVisibility},
    config::StorageConfig,
    content,
    id::IdCodec,
    moderation,
};
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

#[allow(clippy::expect_used)]
fn test_id_codec() -> IdCodec {
    IdCodec::new("test-id-obfuscation-key-en-az-otuz-iki-karakter").expect("geçerli anahtar")
}

#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> ActorRecord {
    let row = sqlx::query!(
        r#"
        INSERT INTO actors (username, actor_type)
        VALUES ($1, 'human'::actor_type)
        RETURNING id, created_at
        "#,
        username,
    )
    .fetch_one(pool)
    .await
    .expect("actor eklenebilmeli");

    ActorRecord {
        id: row.id,
        username: username.to_owned(),
        actor_type: ActorType::Human,
        display_name: None,
        bio: None,
        created_at: row.created_at,
    }
}

#[allow(clippy::expect_used, clippy::too_many_arguments)]
async fn create_post(
    pool: &PgPool,
    author: &ActorRecord,
    community_name: Option<&str>,
    title: &str,
) -> i64 {
    content::create_post(
        pool,
        &test_storage(),
        &test_id_codec(),
        author,
        community_name,
        title,
        "gövde",
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
    )
    .await
    .expect("post oluşturulabilmeli")
    .id
}

/// Banla-ve-sil işi kuyruğa yazılır ve `run_pending_jobs` onu işleyip
/// yalnızca o topluluktaki canlı içeriği soft-delete eder.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn banla_ve_sil_isi_icerigi_siler(pool: PgPool) {
    let sahip = seed_actor(&pool, "worker_sahip").await;
    let kurban = seed_actor(&pool, "worker_kurban").await;

    let topluluk = community::create_community(
        &pool,
        sahip.id,
        "worker_kulubu",
        "açıklama",
        CommunityVisibility::Public,
    )
    .await
    .expect("topluluk oluşturulabilmeli");

    community::join_community(&pool, kurban.id, "worker_kulubu")
        .await
        .expect("katılınabilmeli");

    let topluluk_post = create_post(&pool, &kurban, Some("worker_kulubu"), "silinecek").await;
    let bagimsiz_post = create_post(&pool, &kurban, None, "kalacak").await;

    let izinler = [Grant {
        permission: Permission::MemberBan,
        scope: PermissionScope::Global,
        community_id: None,
    }];

    moderation::ban_actor(
        &pool,
        sahip.id,
        &izinler,
        "worker_kurban",
        "kural ihlali",
        None,
        Some(topluluk.id),
        true,
    )
    .await
    .expect("banlanabilmeli");

    let bekleyen: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM moderation_jobs WHERE processed_at IS NULL"#,
    )
    .fetch_one(&pool)
    .await
    .expect("iş sayılabilmeli");
    assert_eq!(bekleyen, 1, "banla-ve-sil bir iş kuyruklamalı");

    let islenen = moderation::run_pending_jobs(&pool)
        .await
        .expect("worker çalışabilmeli");
    assert_eq!(islenen, 1);

    let topluluk_silindi = sqlx::query_scalar!(
        r#"SELECT deleted_at IS NOT NULL AS "deleted!" FROM contents WHERE id = $1"#,
        topluluk_post,
    )
    .fetch_one(&pool)
    .await
    .expect("topluluk post'u okunabilmeli");
    assert!(topluluk_silindi, "topluluk post'u soft-delete edilmeli");

    let bagimsiz_silindi = sqlx::query_scalar!(
        r#"SELECT deleted_at IS NOT NULL AS "deleted!" FROM contents WHERE id = $1"#,
        bagimsiz_post,
    )
    .fetch_one(&pool)
    .await
    .expect("bağımsız post okunabilmeli");
    assert!(!bagimsiz_silindi, "bağımsız post silinmemeli");

    let kapanmamis: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM moderation_jobs WHERE processed_at IS NULL"#,
    )
    .fetch_one(&pool)
    .await
    .expect("iş sayılabilmeli");
    assert_eq!(kapanmamis, 0, "iş processed_at ile kapatılmalı");
}
