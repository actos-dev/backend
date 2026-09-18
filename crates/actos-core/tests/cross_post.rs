//! `actos_core::content`'in çapraz-gönderi yolları (COMMUNITY_PLAN.md §8,
//! Faz 5): `create_post`'un kaynak kuralları ve [`content::resolve_cross_posts`]'un
//! sayfa başına **tek sorguluk** toplu çözümlemesi.
//!
//! HTTP uçları `crates/actos-api/tests/cross_post_api.rs`'te; burada domain
//! katmanı doğrudan çağrılıyor.

use actos_core::{
    Error, Storage,
    auth::{ActorRecord, ActorType},
    community::{self as core_community, CommunityVisibility},
    config::StorageConfig,
    content::{self, Content},
    id::IdCodec,
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

/// Argon2'yi atlayan ham actor — diğer core testlerdeki aynı gerekçe.
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

/// Başlık/gövde dolu normal bir post; iç kimliğini döner.
#[allow(clippy::expect_used)]
async fn seed_post(
    pool: &PgPool,
    author: &ActorRecord,
    community: Option<&str>,
    title: &str,
) -> i64 {
    content::create_post(
        pool,
        &test_storage(),
        &test_id_codec(),
        author,
        community,
        title,
        "gövde",
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
        None,
    )
    .await
    .expect("post oluşturulabilmeli")
    .id
}

#[allow(clippy::expect_used)]
async fn cross_post(pool: &PgPool, author: &ActorRecord, source_id: i64) -> Content {
    content::create_post(
        pool,
        &test_storage(),
        &test_id_codec(),
        author,
        None,
        "yok sayılır",
        "yok sayılır",
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
        Some(source_id),
    )
    .await
    .expect("çapraz-gönderi oluşturulabilmeli")
}

#[allow(clippy::expect_used)]
async fn make_community(
    pool: &PgPool,
    owner: &ActorRecord,
    name: &str,
    visibility: CommunityVisibility,
) -> i64 {
    core_community::create_community(pool, owner.id, name, "açıklama", visibility)
        .await
        .expect("topluluk oluşturulabilmeli")
        .id
}

// --- create_post kaynak kuralları ----------------------------------------

/// Çapraz-gönderi başlıksız/gövdesiz doğar; kaynağın önizlemesi `201`
/// yanıtında hemen çözülmüş olur (bkz. `create_post` dokümanı).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn capraz_gonderi_basliksiz_dogar_ve_onizleme_cozulur(pool: PgPool) {
    let yazar = seed_actor(&pool, "cp_dogum").await;
    let kaynak = seed_post(&pool, &yazar, None, "kaynak başlık").await;

    let cp = cross_post(&pool, &yazar, kaynak).await;

    assert_eq!(cp.cross_post_source_id, Some(kaynak));
    assert!(cp.title.is_none(), "çapraz-gönderinin kendi başlığı yok");
    assert!(cp.body.is_empty(), "çapraz-gönderinin gövdesi boş");
    let preview = cp.cross_post.expect("kaynak görünür olmalı");
    assert_eq!(preview.source_id, kaynak);
    assert_eq!(preview.title.as_deref(), Some("kaynak başlık"));
    assert_eq!(preview.author.id, yazar.id);
}

/// Derinlik sınırı **tek seviye**: bir çapraz-gönderi kaynak olamaz.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn capraz_gonderinin_capraz_gonderisi_reddediliyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "cp_zincir").await;
    let kaynak = seed_post(&pool, &yazar, None, "kaynak").await;
    let ilk = cross_post(&pool, &yazar, kaynak).await;

    let sonuc = content::create_post(
        &pool,
        &test_storage(),
        &test_id_codec(),
        &yazar,
        None,
        "x",
        "x",
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
        Some(ilk.id),
    )
    .await;

    assert!(
        matches!(sonuc, Err(Error::Validation(_))),
        "zincir 400 (Validation) olmalı, gelen: {sonuc:?}"
    );
}

/// Özel topluluktaki kaynak, yaratıcı o topluluğun üyesi olsa bile
/// çapraz-gönderilemez — "özel topluluktan hiçbir şey çıkmaz".
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ozel_topluluk_kaynagi_uyeye_bile_yasak(pool: PgPool) {
    let sahip = seed_actor(&pool, "cp_ozel_sahip").await;
    make_community(&pool, &sahip, "cp_ozel", CommunityVisibility::Private).await;
    let kaynak = seed_post(&pool, &sahip, Some("cp_ozel"), "gizli").await;

    let sonuc = content::create_post(
        &pool,
        &test_storage(),
        &test_id_codec(),
        &sahip,
        None,
        "x",
        "x",
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
        Some(kaynak),
    )
    .await;

    assert!(
        matches!(sonuc, Err(Error::Forbidden)),
        "özel kaynak 403 olmalı, gelen: {sonuc:?}"
    );
}

/// Yaratıcının göremediği (üyesi olmadığı) özel bir kaynağa `404` —
/// kaynağın **varlığı** sızdırılmaz.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gormedigi_ozel_kaynak_404(pool: PgPool) {
    let sahip = seed_actor(&pool, "cp_kapali_sahip").await;
    let yabanci = seed_actor(&pool, "cp_yabanci").await;
    make_community(&pool, &sahip, "cp_kapali", CommunityVisibility::Private).await;
    let kaynak = seed_post(&pool, &sahip, Some("cp_kapali"), "gizli").await;

    let sonuc = content::create_post(
        &pool,
        &test_storage(),
        &test_id_codec(),
        &yabanci,
        None,
        "x",
        "x",
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
        Some(kaynak),
    )
    .await;

    assert!(
        matches!(sonuc, Err(Error::NotFound(_))),
        "görünmeyen kaynak 404 olmalı, gelen: {sonuc:?}"
    );
}

/// Olmayan bir kaynak `404`; silinmiş bir kaynak `410`.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn olmayan_kaynak_404_silinmis_kaynak_410(pool: PgPool) {
    let yazar = seed_actor(&pool, "cp_silinmis").await;
    let kaynak = seed_post(&pool, &yazar, None, "silinecek").await;

    let yok = content::create_post(
        &pool,
        &test_storage(),
        &test_id_codec(),
        &yazar,
        None,
        "x",
        "x",
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
        Some(999_999),
    )
    .await;
    assert!(matches!(yok, Err(Error::NotFound(_))), "gelen: {yok:?}");

    content::delete_post(&pool, kaynak, yazar.id, &[])
        .await
        .expect("soft-delete başarılı olmalı");

    let silinmis = content::create_post(
        &pool,
        &test_storage(),
        &test_id_codec(),
        &yazar,
        None,
        "x",
        "x",
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
        Some(kaynak),
    )
    .await;
    assert!(
        matches!(silinmis, Err(Error::Gone(_))),
        "gelen: {silinmis:?}"
    );
}

// --- resolve_cross_posts: toplu çözümleme --------------------------------

/// `resolve_cross_posts` bir sayfadaki bütün çapraz-gönderileri **tek
/// sorguda** çözer; aynı kaynağa iki kez bakan satırlar tek yükle paylaşılır
/// (kaynak `HashMap`'ten `get` ile okunuyor, `remove` ile tüketilmiyor).
///
/// Görünürlük: public kaynak herkese, sonradan özel olmuş kaynak yalnızca
/// üyeye. Public yüzeyin geçirdiği boş küme ile üyenin kümesi farklı sonuç
/// verir.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn resolve_cross_posts_toplu_cozer(pool: PgPool) {
    let yazar = seed_actor(&pool, "cp_resolve").await;

    let bagimsiz_id = seed_post(&pool, &yazar, None, "bağımsız kaynak").await;

    let topluluk_id =
        make_community(&pool, &yazar, "cp_resolve_pub", CommunityVisibility::Public).await;
    let topluluk_kaynagi =
        seed_post(&pool, &yazar, Some("cp_resolve_pub"), "topluluk kaynağı").await;

    // İki çapraz-gönderi aynı bağımsız kaynağa, biri topluluk kaynağına.
    let cp_a = cross_post(&pool, &yazar, bagimsiz_id).await;
    let cp_b = cross_post(&pool, &yazar, bagimsiz_id).await;
    let cp_c = cross_post(&pool, &yazar, topluluk_kaynagi).await;

    // Topluluk sonradan özel olur (public → private tek yönlü, §2): kaynak
    // artık yalnızca üyelere görünür.
    sqlx::query!(
        r#"UPDATE communities SET visibility = 'private'::community_visibility WHERE id = $1"#,
        topluluk_id,
    )
    .execute(&pool)
    .await
    .expect("görünürlük güncellenebilmeli");

    let mut icerikler: Vec<Content> = vec![cp_a, cp_b, cp_c];
    // Önizlemeleri temizle ki çözümlemeyi testin kendisi tetiklesin.
    for content in &mut icerikler {
        content.cross_post = None;
    }

    // Public yüzey: boş küme. Özel topluluk kaynağı mezar taşı olur.
    content::resolve_cross_posts(&pool, &[], &mut icerikler)
        .await
        .expect("çözümleme başarılı olmalı");

    assert!(
        icerikler[0].cross_post.is_some(),
        "bağımsız kaynak herkese görünür"
    );
    assert!(
        icerikler[1].cross_post.is_some(),
        "aynı kaynağa bakan ikinci satır da çözülmeli (paylaşılan yükleme)"
    );
    assert!(
        icerikler[2].cross_post.is_none(),
        "özel topluluk kaynağı public yüzeyde mezar taşı"
    );

    // Üye kümesi: özel topluluk kaynağı da görünür.
    for content in &mut icerikler {
        content.cross_post = None;
    }
    content::resolve_cross_posts(&pool, &[topluluk_id], &mut icerikler)
        .await
        .expect("çözümleme başarılı olmalı");

    let preview = icerikler[2]
        .cross_post
        .as_ref()
        .expect("üye özel kaynağı görmeli");
    assert_eq!(preview.community.as_ref().map(|c| c.id), Some(topluluk_id));
    assert_eq!(preview.title.as_deref(), Some("topluluk kaynağı"));
}

/// Silinmiş kaynak **herkese** mezar taşıdır (üye olsa bile).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn resolve_cross_posts_silinmis_kaynak_mezartasi(pool: PgPool) {
    let yazar = seed_actor(&pool, "cp_sil_mezartasi").await;
    let kaynak = seed_post(&pool, &yazar, None, "silinecek").await;
    let cp = cross_post(&pool, &yazar, kaynak).await;

    content::delete_post(&pool, kaynak, yazar.id, &[])
        .await
        .expect("soft-delete başarılı olmalı");

    let mut icerikler = vec![cp];
    icerikler[0].cross_post = None;
    content::resolve_cross_posts(&pool, &[], &mut icerikler)
        .await
        .expect("çözümleme başarılı olmalı");

    assert!(
        icerikler[0].cross_post.is_none(),
        "silinmiş kaynak mezar taşı olmalı"
    );
}
