//! `actos_core::attachment` entegrasyon testleri.
//!
//! **Faz 18.A odak noktası: [`attachment::cleanup_orphaned`]'in avatar
//! dışlaması.** Bir avatar `attachments.content_id`'yi hiçbir zaman
//! doldurmaz (`actos_core::actor::update_profile` onu yalnızca
//! `actors.avatar_object_key`'e yazar, bkz. `actos_core::attachment::
//! resolve_as_avatar` dokümanı) — yani şema düzeyinde bir avatar,
//! `content_id IS NULL AND created_at < eşik` kuralına göre sıradan bir
//! yetim yüklemeden **ayırt edilemez**. Bu testin konusu tam olarak bu:
//! [`attachment::cleanup_orphaned`]'in `NOT EXISTS (... actors.
//! avatar_object_key ...)` dışlamasının gerçekten çalıştığını, avatar
//! olarak kullanılan bir ekin yaşı ne olursa olsun hayatta kaldığını, ama
//! aynı yaştaki **gerçek** bir yetimin hâlâ silindiğini kanıtlamak.
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

/// Kimlik doğrulama yolundan (Argon2id, 10 kurtarma kodu) geçmeden bir
/// actor oluşturur — bkz. `crates/actos-core/tests/trust_level.rs`'teki
/// aynı gerekçe. `register` burada `crate::actor.rs`'teki gibi ham `INSERT`
/// değil `core_auth::register` kullanıyor çünkü bu dosyada tek satırlık bir
/// yardımcı yeterli, `actor_type` enum'unu elle yazmaya gerek yok.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> i64 {
    core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli")
        .actor
        .id
}

/// `attachments` tablosuna, `content_id IS NULL` ve `created_at` `age_hours`
/// saat geriye alınmış bir satır ekler — `cleanup_orphaned`'in yaş eşiğini
/// gerçek zamanda beklemeden test edebilmek için (`trust_level.rs`'teki
/// `hesabi_yaslandir` ile aynı "geçmişe UPDATE" deseni, burada doğrudan
/// `INSERT ... created_at` ile).
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

/// **⚠️ Bu test, SESSİZ VERİ KAYBI tuzağının bekçisi.** Bu test olmadan (ya
/// da `cleanup_orphaned`'deki `NOT EXISTS` dışlaması geri alınırsa), bir
/// actor avatarını ayarladıktan `ORPHAN_MAX_AGE_HOURS` saat sonra bu iş onu
/// sessizce siler — ne istemciye ne loga bir hata düşer, `avatar_url`
/// yalnızca bir sonraki okumada kırık bir bağlantıya döner.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn avatar_olarak_kullanilan_ek_yetim_temizligine_takilmiyor(pool: PgPool) {
    let storage = test_storage();
    let actor_id = seed_actor(&pool, "avatar_sahibi").await;

    // İkisi de yaşça eşik üstü (`content_id IS NULL`, eski) — aralarındaki
    // TEK fark biri `actors.avatar_object_key`'e yazılmış olması.
    let age_hours = attachment::ORPHAN_MAX_AGE_HOURS + 10;
    let avatar_key = format!("{actor_id}/avatar.webp");
    let avatar_attachment_id =
        seed_orphan_attachment(&pool, actor_id, &avatar_key, age_hours).await;
    let real_orphan_key = format!("{actor_id}/gercekten-yetim.webp");
    let real_orphan_id = seed_orphan_attachment(&pool, actor_id, &real_orphan_key, age_hours).await;

    sqlx::query!(
        r#"UPDATE actors SET avatar_object_key = $1 WHERE id = $2"#,
        avatar_key,
        actor_id,
    )
    .execute(&pool)
    .await
    .expect("avatar_object_key yazılabilmeli");

    let silinen = attachment::cleanup_orphaned(&pool, &storage)
        .await
        .expect("temizlik çalışabilmeli");

    assert_eq!(
        silinen, 1,
        "yalnızca gerçek yetim silinmeli, avatar hariç tutulmalı"
    );
    assert!(
        attachment_var_mi(&pool, avatar_attachment_id).await,
        "avatar olarak kullanılan ek temizliğe takılmamalı"
    );
    assert!(
        !attachment_var_mi(&pool, real_orphan_id).await,
        "avatar OLMAYAN gerçek bir yetim hâlâ silinmeli — dışlama çok geniş olmamalı"
    );
}

/// Eşik altındaki (henüz `ORPHAN_MAX_AGE_HOURS` saati doldurmamış) sıradan
/// bir yükleme — avatar olsun olmasın — hiç dokunulmamalı. Avatar
/// dışlamasının "her avatarı sonsuza dek hariç tut" değil "yalnızca yanlış
/// yere düşmesin" olduğunu göstermek için: bu test avatar OLMAYAN taze bir
/// satırla, yukarıdaki test de avatar OLAN eski bir satırla aynı işi
/// tamamlıyor.
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
