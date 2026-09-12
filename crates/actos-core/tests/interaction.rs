//! `actos_core::interaction` ve `actos_core::feed` entegrasyon testleri.
//!
//! HTTP uçları `crates/actos-api/tests/interactions_api.rs` ve
//! `feed_api.rs`'te; burada domain katmanı doğrudan çağrılıyor. Sayaç
//! tutarlılığı ve `hot_score` hesabı gibi şeyler HTTP'ye hiç ihtiyaç
//! duymuyor — ve eşzamanlılık testi router üzerinden çok daha yavaş
//! olurdu.
//!
//! **Actor'lar neden ham `INSERT` ile kuruluyor:** `auth::register` her
//! actor için 10 kurtarma kodunu Argon2id ile hash'liyor. Eşzamanlılık
//! testi 100 actor istiyor, yani 1000 Argon2 hash'i — testi dakikalar
//! sürecek hâle getirirdi. Oy vermek için gereken tek şey `actors` satırı,
//! kimlik doğrulama bu testlerin konusu değil.

use actos_core::{
    Error, Storage,
    auth::{ActorRecord, ActorType},
    config::StorageConfig,
    content, feed,
    id::IdCodec,
    interaction,
};
use sqlx::PgPool;

/// See the identical helper (and its rationale) in `crates/actos-core/tests/
/// notification.rs` — `seed_post` here never carries a file either.
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

/// Kimlik doğrulama yolundan geçmeden bir actor satırı oluşturur.
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

#[allow(clippy::expect_used)]
async fn seed_post(pool: &PgPool, author: &ActorRecord) -> i64 {
    content::create_post(
        pool,
        &test_storage(),
        &test_id_codec(),
        author,
        "başlık",
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

#[allow(clippy::expect_used)]
async fn sayaclar(pool: &PgPool, content_id: i64) -> (i32, i32, i32) {
    let row = sqlx::query!(
        r#"SELECT score, upvotes, downvotes FROM contents WHERE id = $1"#,
        content_id
    )
    .fetch_one(pool)
    .await
    .expect("sayaçlar okunabilmeli");
    (row.score, row.upvotes, row.downvotes)
}

// --- Oy -------------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn oy_verme_sayaclari_guncelliyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "oy_yazar").await;
    let oylayan = seed_actor(&pool, "oy_veren").await;
    let post = seed_post(&pool, &yazar).await;

    let sonuc = interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect("oy verilebilmeli");

    assert_eq!((sonuc.score, sonuc.upvotes, sonuc.downvotes), (1, 1, 0));
    assert_eq!(sayaclar(&pool, post).await, (1, 1, 0));
}

/// Aynı oyu iki kez göndermek sayaçları kaydırmamalı — buglu bir ajanın
/// isteği tekrar etmesi tipik senaryo.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ayni_oy_tekrar_gonderilince_sayac_degismiyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "idem_yazar").await;
    let oylayan = seed_actor(&pool, "idem_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    for _ in 0..3 {
        interaction::set_vote(&pool, oylayan.id, post, 1)
            .await
            .expect("oy verilebilmeli");
    }

    assert_eq!(sayaclar(&pool, post).await, (1, 1, 0));
}

/// Yukarıdan aşağıya geçiş: upvote düşmeli, downvote artmalı, skor 2 birim
/// azalmalı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn oy_yonu_degistirilince_iki_sayac_da_duzeliyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "yon_yazar").await;
    let oylayan = seed_actor(&pool, "yon_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect("yukarı oy");
    assert_eq!(sayaclar(&pool, post).await, (1, 1, 0));

    interaction::set_vote(&pool, oylayan.id, post, -1)
        .await
        .expect("aşağı oy");
    assert_eq!(sayaclar(&pool, post).await, (-1, 0, 1));
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn oy_geri_cekilince_satir_siliniyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "geri_yazar").await;
    let oylayan = seed_actor(&pool, "geri_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect("oy");
    interaction::set_vote(&pool, oylayan.id, post, 0)
        .await
        .expect("geri çekme");

    assert_eq!(sayaclar(&pool, post).await, (0, 0, 0));

    let kalan: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM votes WHERE content_id = $1"#,
        post
    )
    .fetch_one(&pool)
    .await
    .expect("oy sayısı");
    assert_eq!(kalan, 0, "geri çekilen oyun satırı silinmeli");

    // Zaten oy yokken geri çekmek de hatasız geçmeli (idempotent).
    interaction::set_vote(&pool, oylayan.id, post, 0)
        .await
        .expect("oysuz geri çekme de başarılı olmalı");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kendi_icerigine_oy_vermek_engelleniyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "kendi_oy").await;
    let post = seed_post(&pool, &yazar).await;

    let hata = interaction::set_vote(&pool, yazar.id, post, 1)
        .await
        .expect_err("kendi içeriğine oy engellenmeli");

    assert!(
        matches!(hata, Error::Forbidden),
        "beklenen Forbidden: {hata:?}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn gecersiz_oy_degeri_reddediliyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "gecersiz_yazar").await;
    let oylayan = seed_actor(&pool, "gecersiz_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    let hata = interaction::set_vote(&pool, oylayan.id, post, 5)
        .await
        .expect_err("5 geçerli bir oy değeri değil");

    assert!(
        matches!(hata, Error::Validation(_)),
        "beklenen Validation: {hata:?}"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_icerige_oy_410(pool: PgPool) {
    let yazar = seed_actor(&pool, "silinmis_yazar").await;
    let oylayan = seed_actor(&pool, "silinmis_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    sqlx::query!(
        r#"UPDATE contents SET deleted_at = now() WHERE id = $1"#,
        post
    )
    .execute(&pool)
    .await
    .expect("silinebilmeli");

    let hata = interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect_err("silinmiş içeriğe oy verilememeli");

    assert!(matches!(hata, Error::Gone(_)), "beklenen Gone: {hata:?}");
}

/// **Planın açıkça istediği test:** aynı içeriğe eşzamanlı 100 oy geldiğinde
/// sayaçlar tutarlı kalmalı.
///
/// `set_vote` içerik satırını `FOR UPDATE` ile kilitliyor; kilit olmasaydı
/// "oku, hesapla, yaz" dizileri iç içe geçer ve sayaç 100'ün altında
/// kalırdı (kayıp güncelleme).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn eszamanli_yuz_oy_sayaci_bozmuyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "eszamanli_yazar").await;
    let post = seed_post(&pool, &yazar).await;

    let mut oylayanlar = Vec::with_capacity(100);
    for i in 0..100 {
        oylayanlar.push(seed_actor(&pool, &format!("eszamanli_{i}")).await.id);
    }

    let mut gorevler = Vec::with_capacity(100);
    for actor_id in oylayanlar {
        let pool = pool.clone();
        gorevler.push(tokio::spawn(async move {
            interaction::set_vote(&pool, actor_id, post, 1).await
        }));
    }

    for gorev in gorevler {
        gorev
            .await
            .expect("görev panik atmamalı")
            .expect("oy verilebilmeli");
    }

    assert_eq!(
        sayaclar(&pool, post).await,
        (100, 100, 0),
        "eşzamanlı oylarda sayaç kaymamalı"
    );

    let satir_sayisi: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM votes WHERE content_id = $1"#,
        post
    )
    .fetch_one(&pool)
    .await
    .expect("oy sayısı");
    assert_eq!(satir_sayisi, 100);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn toplu_oy_sorgusu_yalnizca_oy_verilenleri_donuyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "toplu_yazar").await;
    let oylayan = seed_actor(&pool, "toplu_oylayan").await;
    let a = seed_post(&pool, &yazar).await;
    let b = seed_post(&pool, &yazar).await;
    let c = seed_post(&pool, &yazar).await;

    interaction::set_vote(&pool, oylayan.id, a, 1)
        .await
        .expect("oy");
    interaction::set_vote(&pool, oylayan.id, b, -1)
        .await
        .expect("oy");

    let mut oylar = interaction::votes_for(&pool, oylayan.id, &[a, b, c])
        .await
        .expect("toplu sorgu");
    oylar.sort_unstable();

    let mut beklenen = vec![(a, 1_i16), (b, -1_i16)];
    beklenen.sort_unstable();

    assert_eq!(oylar, beklenen, "oy verilmemiş içerik yanıtta olmamalı");
}

// --- Withdrawing/changing a vote must not corrupt the score ---------------
//
// Vote weight (`votes.weight`) was removed (see REFACTOR.md §3): the score
// is now a flat `sum(value)`. The tests in this section used to check that
// the weight snapshot (the `weight` stored on the row, not the voter's
// CURRENT tier) was used correctly — that mechanism is gone, but the
// actual invariant that must be preserved is the same: changing or
// withdrawing a vote must never take back or add more than that vote's own
// previous contribution.

/// When a vote first changes direction and then is withdrawn entirely, the
/// score must drop to exactly the expected value at each step — this
/// verifies that the `score_delta = value - onceki_value` computation
/// doesn't drift across successive changes.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn oy_degistirip_sonra_geri_cekmek_skoru_tam_sifirliyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "reversal_yazar").await;
    let oylayan = seed_actor(&pool, "reversal_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    let yukari = interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect("yukarı oy verilebilmeli");
    assert_eq!(yukari.score, 1);

    let asagi = interaction::set_vote(&pool, oylayan.id, post, -1)
        .await
        .expect("yön değiştirilebilmeli");
    assert_eq!(
        asagi.score, -1,
        "yön değişimi skoru tam iki birim kaydırmalı"
    );

    let geri_cekilen = interaction::set_vote(&pool, oylayan.id, post, 0)
        .await
        .expect("geri çekilebilmeli");
    assert_eq!(
        geri_cekilen.score, 0,
        "geri çekme, oyun kendi son katkısını (-1) tam olarak tersine çevirmeli"
    );
    assert_eq!(
        sayaclar(&pool, post).await,
        (0, 0, 0),
        "geri çekme upvote/downvote sayaçlarını da sıfırlamalı"
    );
}

// --- Takip -----------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn takip_idempotent(pool: PgPool) {
    let a = seed_actor(&pool, "takip_eden").await;
    seed_actor(&pool, "takip_edilen").await;

    for _ in 0..3 {
        interaction::follow(&pool, a.id, "takip_edilen")
            .await
            .expect("takip edilebilmeli");
    }

    let n: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM follows WHERE follower_actor_id = $1"#,
        a.id
    )
    .fetch_one(&pool)
    .await
    .expect("sayım");
    assert_eq!(n, 1);

    // Takibi bırakmak da idempotent.
    for _ in 0..2 {
        interaction::unfollow(&pool, a.id, "takip_edilen")
            .await
            .expect("takip bırakılabilmeli");
    }

    let n: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM follows WHERE follower_actor_id = $1"#,
        a.id
    )
    .fetch_one(&pool)
    .await
    .expect("sayım");
    assert_eq!(n, 0);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kendini_takip_reddediliyor(pool: PgPool) {
    let a = seed_actor(&pool, "kendini_takip").await;

    let hata = interaction::follow(&pool, a.id, "kendini_takip")
        .await
        .expect_err("kendini takip engellenmeli");

    assert!(
        matches!(hata, Error::Validation(_)),
        "beklenen Validation: {hata:?}"
    );
}

/// Silinmiş bir hesap **takip edilemez** ama **takipten çıkarılabilir** —
/// aksi hâlde takip listesinde kaldırılamayan bir satır sıkışırdı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_hesap_takip_edilemez_ama_birakilabilir(pool: PgPool) {
    let a = seed_actor(&pool, "birakan").await;
    let b = seed_actor(&pool, "silinecek_hedef").await;

    interaction::follow(&pool, a.id, "silinecek_hedef")
        .await
        .expect("önce takip edilebilmeli");

    sqlx::query!(
        r#"UPDATE actors SET deleted_at = now() WHERE id = $1"#,
        b.id
    )
    .execute(&pool)
    .await
    .expect("silinebilmeli");

    let hata = interaction::follow(&pool, a.id, "silinecek_hedef")
        .await
        .expect_err("silinmiş hesap takip edilememeli");
    assert!(matches!(hata, Error::Gone(_)), "beklenen Gone: {hata:?}");

    interaction::unfollow(&pool, a.id, "silinecek_hedef")
        .await
        .expect("silinmiş hesap takipten çıkarılabilmeli");
}

// --- Kaydetme ---------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kaydetme_idempotent_ve_kendi_icerigi_serbest(pool: PgPool) {
    let a = seed_actor(&pool, "kaydeden").await;
    let post = seed_post(&pool, &a).await;

    // Kendi içeriğini kaydetmek serbest (oy vermenin aksine).
    for _ in 0..3 {
        interaction::save(&pool, a.id, post)
            .await
            .expect("kaydedilebilmeli");
    }

    let n: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM saves WHERE actor_id = $1"#,
        a.id
    )
    .fetch_one(&pool)
    .await
    .expect("sayım");
    assert_eq!(n, 1);

    interaction::unsave(&pool, a.id, post)
        .await
        .expect("kayıt kaldırılabilmeli");
    interaction::unsave(&pool, a.id, post)
        .await
        .expect("ikinci kez de hatasız olmalı");
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kayitlar_en_son_kaydedilen_once_donuyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "kayit_yazar").await;
    let kaydeden = seed_actor(&pool, "kayit_kaydeden").await;

    let eski = seed_post(&pool, &yazar).await;
    let yeni = seed_post(&pool, &yazar).await;

    // Sıralama kaydın zamanına göre: önce YENİ post kaydediliyor, sonra
    // ESKİ post. Beklenen sıra "eski post, yeni post" — yani içeriğin
    // yaşına göre değil kaydın yaşına göre.
    interaction::save(&pool, kaydeden.id, yeni)
        .await
        .expect("kayıt");
    sqlx::query!(
        r#"UPDATE saves SET created_at = now() - interval '1 hour' WHERE content_id = $1"#,
        yeni
    )
    .execute(&pool)
    .await
    .expect("kayıt zamanı geriye alınabilmeli");

    interaction::save(&pool, kaydeden.id, eski)
        .await
        .expect("kayıt");

    let sayfa = interaction::list_saves(&pool, kaydeden.id, None, 10)
        .await
        .expect("kayıtlar listelenebilmeli");

    let idler: Vec<i64> = sayfa.items.iter().map(|c| c.id).collect();
    assert_eq!(
        idler,
        vec![eski, yeni],
        "en son kaydedilen önce gelmeli (içeriğin yaşına göre değil)"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_icerik_kayit_listesinde_gorunmuyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "kayit_silme_yazar").await;
    let kaydeden = seed_actor(&pool, "kayit_silme_kaydeden").await;

    let kalan = seed_post(&pool, &yazar).await;
    let silinen = seed_post(&pool, &yazar).await;

    interaction::save(&pool, kaydeden.id, kalan)
        .await
        .expect("kayıt");
    interaction::save(&pool, kaydeden.id, silinen)
        .await
        .expect("kayıt");

    sqlx::query!(
        r#"UPDATE contents SET deleted_at = now() WHERE id = $1"#,
        silinen
    )
    .execute(&pool)
    .await
    .expect("silinebilmeli");

    let sayfa = interaction::list_saves(&pool, kaydeden.id, None, 10)
        .await
        .expect("liste");

    let idler: Vec<i64> = sayfa.items.iter().map(|c| c.id).collect();
    assert_eq!(idler, vec![kalan]);

    // Kaydın kendisi duruyor: kullanıcı isterse kaldırabilmeli.
    interaction::unsave(&pool, kaydeden.id, silinen)
        .await
        .expect("silinmiş içeriğin kaydı kaldırılabilmeli");
}

// --- Hot score --------------------------------------------------------------

/// Oy verildiğinde `hot_score` **aynı transaction'da** güncellenmeli.
///
/// Ayrıca formülün düzeltilmiş hâlini doğruluyor: oy almamış bir post'un
/// `hot_score`'u 0 DEĞİL, zaman terimi kadar olmalı. Planın ilk
/// formülünde `sign(0) = 0` zaman terimini siliyordu (bkz. PLAN.md Faz 12).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn oy_hot_scoreu_aninda_guncelliyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "hot_yazar").await;
    let oylayan = seed_actor(&pool, "hot_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    // Yeni post: şema varsayılanı 0. Tazeleme henüz koşmadı.
    let ilk: f64 = sqlx::query_scalar!(
        r#"SELECT hot_score AS "h!" FROM contents WHERE id = $1"#,
        post
    )
    .fetch_one(&pool)
    .await
    .expect("hot_score");
    assert!(
        (ilk - 0.0).abs() < f64::EPSILON,
        "yeni post şema varsayılanıyla başlar: {ilk}"
    );

    interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect("oy");

    let sonra: f64 = sqlx::query_scalar!(
        r#"SELECT hot_score AS "h!" FROM contents WHERE id = $1"#,
        post
    )
    .fetch_one(&pool)
    .await
    .expect("hot_score");

    // Zaman terimi tek başına ~39 700 (epoch/45000). Skor 1 iken log
    // terimi 0, yani değer zaman teriminden ibaret ama kesinlikle 0 değil.
    assert!(
        sonra > 39_000.0,
        "hot_score zaman terimini içermeli, 0 kalmamalı: {sonra}"
    );
}

/// Periyodik tazeleme oy almamış postları da düzeltiyor: şema varsayılanı
/// 0 olan satır, zaman terimini kazanıyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tazeleme_oysuz_postlari_da_hesapliyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "tazeleme_yazar").await;
    let post = seed_post(&pool, &yazar).await;

    let guncellenen = feed::recompute_hot_scores(&pool)
        .await
        .expect("tazeleme çalışabilmeli");
    assert!(guncellenen >= 1, "en az bir post güncellenmeli");

    let hot: f64 = sqlx::query_scalar!(
        r#"SELECT hot_score AS "h!" FROM contents WHERE id = $1"#,
        post
    )
    .fetch_one(&pool)
    .await
    .expect("hot_score");

    assert!(
        hot > 39_000.0,
        "oy almamış post da zaman terimini almalı (düzeltilmiş formül): {hot}"
    );
}

/// Tazeleme penceresi dışında kalan eski postlara dokunulmuyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tazeleme_pencere_disini_atliyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "pencere_yazar").await;
    let eski = seed_post(&pool, &yazar).await;

    sqlx::query!(
        r#"UPDATE contents SET created_at = now() - interval '30 days' WHERE id = $1"#,
        eski
    )
    .execute(&pool)
    .await
    .expect("tarih geriye alınabilmeli");

    feed::recompute_hot_scores(&pool)
        .await
        .expect("tazeleme çalışabilmeli");

    let hot: f64 = sqlx::query_scalar!(
        r#"SELECT hot_score AS "h!" FROM contents WHERE id = $1"#,
        eski
    )
    .fetch_one(&pool)
    .await
    .expect("hot_score");

    assert!(
        (hot - 0.0).abs() < f64::EPSILON,
        "7 günden eski post tazelenmemeli: {hot}"
    );
}
