//! `actos_core::visibility` testleri (COMMUNITY_PLAN.md Faz 4A).
//!
//! [`visible_community_ids`]'in sözleşmesi: bir aktörün **üye olduğu** ve
//! **topluluk kapsamlı izin tuttuğu** toplulukların birleşimi; anonim için
//! boş. Bu küme, okuma yollarının `content_visible_to`'ya geçirdiği
//! `viewer_communities` değerinin kaynağı olduğu için doğruluğu doğrudan bir
//! sızıntı riski taşıyor.

use actos_core::{
    auth::{self as core_auth, Permission, PermissionScope},
    community::{self as core_community, CommunityVisibility},
    visibility,
};
use sqlx::PgPool;

/// `auth::register`'ın 10 Argon2 hash'ini atlayan ham actor ekleme — diğer
/// core testlerindeki aynı gerekçe (kimlik doğrulama bu testin konusu değil).
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> i64 {
    sqlx::query_scalar!(
        r#"INSERT INTO actors (username, actor_type)
           VALUES ($1, 'human'::actor_type)
           RETURNING id"#,
        username,
    )
    .fetch_one(pool)
    .await
    .expect("actor eklenebilmeli")
}

#[allow(clippy::expect_used)]
async fn create_community(pool: &PgPool, owner_id: i64, name: &str) -> i64 {
    core_community::create_community(
        pool,
        owner_id,
        name,
        "açıklama",
        CommunityVisibility::Public,
    )
    .await
    .expect("topluluk oluşturulabilmeli")
    .id
}

#[allow(clippy::expect_used)]
async fn add_member(pool: &PgPool, community_id: i64, actor_id: i64) {
    sqlx::query!(
        r#"INSERT INTO community_members (community_id, actor_id)
           VALUES ($1, $2)
           ON CONFLICT DO NOTHING"#,
        community_id,
        actor_id,
    )
    .execute(pool)
    .await
    .expect("üyelik eklenebilmeli");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn anonim_icin_kume_bos(pool: PgPool) {
    let ids = visibility::visible_community_ids(&pool, None)
        .await
        .expect("sorgu çalışabilmeli");
    assert!(ids.is_empty(), "anonim hiçbir topluluğu göremez");
}

/// Üyelikler ve topluluk kapsamlı izinler tek kümede birleşiyor: biri
/// görebildiği, diğeri yönetebildiği topluluk olabilir ve ikisi de kapıya
/// girmeli.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn uyelik_ve_topluluk_kapsamli_izin_birlesiyor(pool: PgPool) {
    let owner = seed_actor(&pool, "vis_owner").await;
    let viewer = seed_actor(&pool, "vis_viewer").await;

    let uye_oldugu = create_community(&pool, owner, "vis_uyelik").await;
    let izni_oldugu = create_community(&pool, owner, "vis_izin").await;

    add_member(&pool, uye_oldugu, viewer).await;

    core_auth::grant_permission(
        &pool,
        viewer,
        Permission::ContentDelete,
        PermissionScope::Community,
        Some(izni_oldugu),
        None,
    )
    .await
    .expect("topluluk kapsamlı izin verilebilmeli");

    let ids = visibility::visible_community_ids(&pool, Some(viewer))
        .await
        .expect("sorgu çalışabilmeli");

    assert_eq!(
        ids,
        vec![uye_oldugu, izni_oldugu],
        "üyelik ve topluluk kapsamlı izin birlikte dönmeli"
    );
}

/// Aynı toplulukta hem üye hem izinli olmak kümeyi **iki kez** büyütmez —
/// `UNION` (yalnızca `UNION ALL` değil) bunun için seçildi.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ayni_topluluk_tek_kez_donuyor(pool: PgPool) {
    let owner = seed_actor(&pool, "vis_dup_owner").await;
    let viewer = seed_actor(&pool, "vis_dup_viewer").await;

    let ortak = create_community(&pool, owner, "vis_ortak").await;

    add_member(&pool, ortak, viewer).await;
    core_auth::grant_permission(
        &pool,
        viewer,
        Permission::ContentDelete,
        PermissionScope::Community,
        Some(ortak),
        None,
    )
    .await
    .expect("izin verilebilmeli");

    let ids = visibility::visible_community_ids(&pool, Some(viewer))
        .await
        .expect("sorgu çalışabilmeli");

    assert_eq!(ids, vec![ortak], "aynı topluluk tek satır olmalı");
}

/// Sahip, topluluk oluşturulurken otomatik üye yazıldığı için kendi
/// topluluğunu görünürlük kümesinde görür — ayrı bir "sahip özel durumu"
/// yok, üyelik tablosu tek doğruluk kaynağı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn sahip_kendi_toplulugunu_gorur(pool: PgPool) {
    let owner = seed_actor(&pool, "vis_sahip").await;
    let sahipligi = create_community(&pool, owner, "vis_sahiplik").await;

    let ids = visibility::visible_community_ids(&pool, Some(owner))
        .await
        .expect("sorgu çalışabilmeli");

    assert_eq!(ids, vec![sahipligi]);
}
