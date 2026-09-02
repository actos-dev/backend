//! `actos_core::actor::recompute_trust_levels` entegrasyon testleri.
//!
//! **Bu dosya yalnızca güven kademesinin TEMELİni test ediyor** (Faz 18.A'nın
//! "temel" kısmı) — oy ağırlığı, hot filtresi, rate limit ve depolama kotası
//! gibi kademenin ETKİLERİ ayrı bir görevde uygulanacak, burada test edilmiyor.
//!
//! **Actor'lar neden ham `INSERT` ile kuruluyor:** `crates/actos-core/tests/
//! interaction.rs`'teki aynı gerekçe — `auth::register` her actor için 10
//! kurtarma kodunu Argon2id ile hash'liyor, bu testlerin konusu değil.
//!
//! **Hesap yaşı neden `UPDATE actors SET created_at = ...` ile geriye
//! alınıyor:** `#[sqlx::test]` her testi taze bir veritabanında, "şimdi"
//! oluşturulmuş satırlarla başlatıyor; 24 saatlik/7 günlük eşikleri gerçek
//! zamanda beklemek yerine `created_at`'i geriye almak
//! `crates/actos-core/tests/interaction.rs` ve `tests/search.rs`'te de
//! kullanılan aynı desen.

use actos_core::{
    auth::{ActorRecord, ActorType},
    content, moderation,
};
use sqlx::PgPool;

/// Kimlik doğrulama yolundan geçmeden bir actor satırı oluşturur.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> ActorRecord {
    let row = sqlx::query!(
        r#"
        INSERT INTO actors (username, actor_type)
        VALUES ($1, 'human'::actor_type)
        RETURNING id, created_at, trust_level
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
        trust_level: row.trust_level,
    }
}

#[allow(clippy::expect_used)]
async fn seed_post(pool: &PgPool, author: &ActorRecord) -> i64 {
    content::create_post(pool, author, "başlık", "gövde", &[], None, &[])
        .await
        .expect("post oluşturulabilmeli")
        .id
}

/// `actors.created_at`'i `gun_once` gün geriye alır — hesap yaşı eşiklerini
/// test edebilmek için (bkz. modül dokümantasyonundaki gerekçe).
#[allow(clippy::expect_used)]
async fn hesabi_yaslandir(pool: &PgPool, actor_id: i64, gun_once: f64) {
    sqlx::query!(
        r#"UPDATE actors SET created_at = now() - make_interval(secs => $2) WHERE id = $1"#,
        actor_id,
        gun_once * 86_400.0,
    )
    .execute(pool)
    .await
    .expect("hesap yaşı geriye alınabilmeli");
}

/// Bir içeriğin `score`'unu doğrudan yazar — 25 gerçek actor'le oy vermek
/// yerine (bkz. `crates/actos-api/tests/tags_api.rs`/`comments_api.rs`'teki
/// aynı desen: `UPDATE contents SET score = ...`). `recompute_trust_levels`
/// zaten `contents.score`'u okuyor, `votes` tablosuna inmiyor — bkz. o
/// fonksiyonun dokümantasyonu.
#[allow(clippy::expect_used)]
async fn skor_ayarla(pool: &PgPool, content_id: i64, score: i32) {
    sqlx::query!(
        r#"UPDATE contents SET score = $2 WHERE id = $1"#,
        content_id,
        score,
    )
    .execute(pool)
    .await
    .expect("skor ayarlanabilmeli");
}

#[allow(clippy::expect_used)]
async fn trust_level_oku(pool: &PgPool, actor_id: i64) -> i16 {
    sqlx::query_scalar!(r#"SELECT trust_level FROM actors WHERE id = $1"#, actor_id)
        .fetch_one(pool)
        .await
        .expect("trust_level okunabilmeli")
}

// --- Seviye 0 -----------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yeni_hesap_seviye_0(pool: PgPool) {
    let actor = seed_actor(&pool, "yeni_hesap").await;
    assert_eq!(
        actor.trust_level, 0,
        "şema varsayılanı: yeni bir actor satırı seviye 0 ile doğmalı"
    );

    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");

    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        0,
        "hiçbir kriteri karşılamayan taze bir hesap recompute'tan sonra da seviye 0'da kalmalı"
    );
}

// --- Seviye 1 -------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yirmi_dort_saatten_eski_ve_icerigi_olan_hesap_seviye_1_e_cikiyor(pool: PgPool) {
    let actor = seed_actor(&pool, "terfi_aday").await;
    seed_post(&pool, &actor).await;
    hesabi_yaslandir(&pool, actor.id, 2.0).await;

    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");

    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        1,
        "yaşı >= 24 saat VE en az bir silinmemiş içeriği olan hesap seviye 1'e çıkmalı"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yeterince_yasli_ama_icerigi_olmayan_hesap_seviye_0_da_kaliyor(pool: PgPool) {
    let actor = seed_actor(&pool, "icerigi_yok").await;
    hesabi_yaslandir(&pool, actor.id, 2.0).await;

    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");

    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        0,
        "yaş şartı tek başına yetmiyor, en az bir silinmemiş içerik de şart"
    );
}

/// **SOĞUK BAŞLANGIÇ (cold start) TUZAĞININ BEKÇİSİ.**
///
/// Bu test kasıtlı olarak veritabanında BAŞKA HİÇBİR actor yaratmıyor —
/// yani test edilen actor'e teorik olarak bile oy verebilecek kimse yok
/// (`net_votes` yapısal olarak `0`). Buna rağmen seviye 1'e çıkması
/// bekleniyor: `crate::actor::recompute_trust_levels` dokümantasyonundaki
/// uyarı burada — seviye 1'e "karma" (oy) şartı EKLENİRSE bu test KIRILIR,
/// ki bu tam olarak istenen: yeni bir platformda ilk kullanıcıları
/// sonsuza dek seviye 0'da kilitleyecek bir regresyonu yakalamak bu
/// testin tek işi. "Tutarlılık olsun" diye buraya bir oy/karma şartı
/// eklemeyin — NOTES.md §9.3 ve migration 0020'nin `COMMENT ON COLUMN`'u
/// bunun neden bilinçli bir tasarım kararı olduğunu anlatıyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn karma_olmadan_seviye_1_e_cikilabiliyor_soguk_baslangic_bekcisi(pool: PgPool) {
    let ilk_kullanici = seed_actor(&pool, "ilk_kullanici").await;
    seed_post(&pool, &ilk_kullanici).await;
    hesabi_yaslandir(&pool, ilk_kullanici.id, 2.0).await;

    // Platformda bu tek actor'den başka kimse yok — kimse ona oy veremez.
    let actor_sayisi: i64 = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM actors"#)
        .fetch_one(&pool)
        .await
        .expect("actor sayısı okunabilmeli");
    assert_eq!(actor_sayisi, 1, "test varsayımı: platformda tek actor var");

    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");

    assert_eq!(
        trust_level_oku(&pool, ilk_kullanici.id).await,
        1,
        "seviye 1'in karma şartı YOK: kimsenin oy veremediği bir platformda bile \
         yaş + içerik şartını karşılayan ilk kullanıcı seviye 1'e çıkabilmeli"
    );
}

// --- Seviye 2 ---------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn seviye_2_sartlari(pool: PgPool) {
    let actor = seed_actor(&pool, "kurulmus_uye").await;
    let post = seed_post(&pool, &actor).await;
    hesabi_yaslandir(&pool, actor.id, 8.0).await;

    // Sınır: 24 net oy henüz seviye 2 için yetmiyor.
    skor_ayarla(&pool, post, 24).await;
    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");
    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        1,
        "24 net oy seviye 2 eşiğinin (>= 25) altında, hesap seviye 1'de kalmalı"
    );

    // Eşik: net oy 25'e çıkınca seviye 2.
    skor_ayarla(&pool, post, 25).await;
    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");
    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        2,
        "yaş >= 7 gün VE net oy >= 25 VE onaylanmış rapor yok ise hesap seviye 2'ye çıkmalı"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yedi_gunden_taze_ama_oyu_yeterli_hesap_seviye_2_ye_cikamiyor(pool: PgPool) {
    let actor = seed_actor(&pool, "taze_ama_oylu").await;
    let post = seed_post(&pool, &actor).await;
    // Seviye 1 şartını karşılasın (>= 24 saat) ama seviye 2'nin >= 7 gün
    // şartını karşılamasın.
    hesabi_yaslandir(&pool, actor.id, 2.0).await;
    skor_ayarla(&pool, post, 100).await;

    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");

    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        1,
        "yaş şartı (>= 7 gün) karşılanmadan yüksek net oy tek başına seviye 2'ye yetmemeli"
    );
}

// --- Düşürme ----------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn onaylanmis_rapor_bir_seviye_dusuruyor(pool: PgPool) {
    let admin = seed_actor(&pool, "trust_moderator").await;
    let reporter = seed_actor(&pool, "sikayetci").await;
    let actor = seed_actor(&pool, "raporlanan_uye").await;
    let post = seed_post(&pool, &actor).await;

    // Seviye 2'yi hak eden bir hesap kur.
    hesabi_yaslandir(&pool, actor.id, 8.0).await;
    skor_ayarla(&pool, post, 30).await;

    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");
    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        2,
        "test varsayımı: rapor öncesi hesap seviye 2'yi hak ediyor olmalı"
    );

    let rapor = moderation::create_report(
        &pool,
        reporter.id,
        moderation::ReportTargetType::Post,
        post,
        "kural ihlali",
    )
    .await
    .expect("şikayet oluşturulabilmeli");

    moderation::update_report(
        &pool,
        admin.id,
        rapor.id,
        moderation::ReportStatus::Resolved,
        Some("ihlal doğrulandı"),
    )
    .await
    .expect("şikayet çözülebilmeli");

    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");

    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        1,
        "onaylanmış (resolved) bir rapor, aksi hâlde hak edilen kademeyi bir azaltmalı (2 -> 1)"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn reddedilen_rapor_kademeyi_dusurmuyor(pool: PgPool) {
    let admin = seed_actor(&pool, "trust_moderator2").await;
    let reporter = seed_actor(&pool, "sikayetci2").await;
    let actor = seed_actor(&pool, "haksiz_raporlanan").await;
    let post = seed_post(&pool, &actor).await;

    hesabi_yaslandir(&pool, actor.id, 2.0).await;

    let rapor = moderation::create_report(
        &pool,
        reporter.id,
        moderation::ReportTargetType::Post,
        post,
        "asılsız şikayet",
    )
    .await
    .expect("şikayet oluşturulabilmeli");

    // `dismissed`: onaylanmamış, yalnızca `resolved` düşürmeli.
    moderation::update_report(
        &pool,
        admin.id,
        rapor.id,
        moderation::ReportStatus::Dismissed,
        Some("asılsız"),
    )
    .await
    .expect("şikayet reddedilebilmeli");

    actos_core::actor::recompute_trust_levels(&pool)
        .await
        .expect("recompute başarılı olmalı");

    assert_eq!(
        trust_level_oku(&pool, actor.id).await,
        1,
        "reddedilen (dismissed) bir rapor kademeyi düşürmemeli, yalnızca resolved düşürür"
    );
}
