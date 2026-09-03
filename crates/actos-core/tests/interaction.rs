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
    Error,
    auth::{ActorRecord, ActorType},
    content, feed, interaction,
};
use sqlx::PgPool;

/// Kimlik doğrulama yolundan geçmeden bir actor satırı oluşturur.
///
/// **`trust_level = 1` ile açılıyor** (şema varsayılanı `0` değil) — Faz
/// 18.B'den (oy ağırlığı, bkz. `crate::interaction::set_vote`) önce
/// yazılmış bu dosyadaki testlerin BÜYÜK ÇOĞUNLUĞU sayaç/eşzamanlılık
/// davranışını sınıyor, güven kademesini DEĞİL — "bir oy skoru 1
/// artırır" gibi varsayımlar taşıyorlar. Eğer bu fixture şema
/// varsayılanı `0`'da kalsaydı, oy ağırlığı seviye 0'ı `0` ağırlıklandırdığı
/// için o testlerin TAMAMI (ilgisiz oldukları bir mekanizma yüzünden)
/// kırılırdı. Seviye 0'a özgü davranış (ağırlık `0`, terfi sonrası geri
/// çekme) [`seed_actor_level0`] ile AYRI ve AÇIKÇA test ediliyor — bkz.
/// aşağıdaki "Oy ağırlığı" bölümü.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> ActorRecord {
    let row = sqlx::query!(
        r#"
        INSERT INTO actors (username, actor_type, trust_level)
        VALUES ($1, 'human'::actor_type, 1)
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

/// [`seed_actor`] gibi ama **seviye 0** (şema varsayılanı) ile açılır —
/// oy ağırlığı testlerinin "taze/doğrulanmamış hesap" ucu bunu kullanıyor.
#[allow(clippy::expect_used)]
async fn seed_actor_level0(pool: &PgPool, username: &str) -> ActorRecord {
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

/// Bir actor'ün `trust_level`'ını doğrudan yazar — "terfi" senaryosunu
/// gerçek zamanda (`recompute_trust_levels`'ın koşullarını sağlamayı
/// beklemeden) simüle etmek için.
#[allow(clippy::expect_used)]
async fn trust_level_yukselt(pool: &PgPool, actor_id: i64, yeni_seviye: i16) {
    sqlx::query!(
        r#"UPDATE actors SET trust_level = $2 WHERE id = $1"#,
        actor_id,
        yeni_seviye,
    )
    .execute(pool)
    .await
    .expect("trust_level güncellenebilmeli");
}

#[allow(clippy::expect_used)]
async fn seed_post(pool: &PgPool, author: &ActorRecord) -> i64 {
    content::create_post(pool, author, "başlık", "gövde", &[], None, &[])
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

// --- Oy ağırlığı (Faz 18.B, NOTES.md §9.3, migrations/0022_vote_weight) ----

/// Seviye 0'ın oyu `votes` satırı olarak kaydedilir ve `upvotes` ham
/// sayacına işler — kullanıcı oyunun "tuttuğunu" görmeli — ama `weight = 0`
/// olduğu için `score`'a hiç katkı yapmaz. Bkz.
/// `crate::interaction::set_vote`'un "Oy ağırlığı" bölümü.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn seviye_0in_oyu_sayaci_etkiler_skoru_etkilemez(pool: PgPool) {
    let yazar = seed_actor(&pool, "agirlik0_yazar").await;
    let oylayan = seed_actor_level0(&pool, "agirlik0_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    let sonuc = interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect("seviye 0 da oy verebilmeli — oy engellenmiyor, sadece ağırlıksız");

    assert_eq!(sonuc.score, 0, "seviye 0'ın oyu skora katkı yapmamalı");
    assert_eq!(sonuc.upvotes, 1, "ham upvote sayacı yine de artmalı");
    assert_eq!(sonuc.downvotes, 0);

    assert_eq!(
        sayaclar(&pool, post).await,
        (0, 1, 0),
        "veritabanındaki sayaçlar da aynı: score 0, upvotes 1"
    );

    let satir = sqlx::query!(
        r#"SELECT value, weight FROM votes WHERE actor_id = $1 AND content_id = $2"#,
        oylayan.id,
        post,
    )
    .fetch_one(&pool)
    .await
    .expect("oy satırı kaydedilmiş olmalı — silinmedi, sadece ağırlıksız");
    assert_eq!((satir.value, satir.weight), (1, 0));
}

/// Seviye 1'in (ve dolayısıyla seviye 2'nin) oyu tam ağırlıklı: skoru
/// doğrudan etkiler. `seed_actor` zaten seviye 1 açıyor (bkz. onun
/// dokümantasyonu) — bu test o varsayımı açıkça sabitliyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn seviye_1in_oyu_skoru_degistirir(pool: PgPool) {
    let yazar = seed_actor(&pool, "agirlik1_yazar").await;
    let oylayan = seed_actor(&pool, "agirlik1_oylayan").await;
    assert_eq!(
        oylayan.trust_level, 1,
        "fixture varsayımı: seed_actor seviye 1"
    );
    let post = seed_post(&pool, &yazar).await;

    let sonuc = interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect("oy verilebilmeli");

    assert_eq!(sonuc.score, 1, "seviye 1'in oyu tam ağırlıklı olmalı");
    assert_eq!(sayaclar(&pool, post).await, (1, 1, 0));
}

/// **Madde 3'teki incelik:** seviye 0'ken verilen bir oy, sahibi SONRADAN
/// terfi ettikten sonra geri çekilirse `score`'u BOZMAMALI. Ağırlık oy
/// ANINDA sabitlendiği için (`votes.weight`), geri çekmenin delta'sı satırda
/// saklı `weight = 0` ile hesaplanmalı — oy verenin BUGÜNKÜ (terfi sonrası)
/// kademesiyle değil. Yanlış bir uygulama (delta'yı bugünkü kademeden
/// türetirse) geri çekmeyi `-1` sayar ve `score`'u gereksiz yere eksiye
/// kaydırırdı; bu test tam olarak o hatayı yakalamak için var.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn seviye_0iken_verilip_terfi_sonrasi_geri_cekilen_oy_skoru_bozmuyor(pool: PgPool) {
    let yazar = seed_actor(&pool, "terfi_yazar").await;
    let oylayan = seed_actor_level0(&pool, "terfi_oylayan").await;
    let post = seed_post(&pool, &yazar).await;

    // Seviye 0'ken oy ver: ağırlıksız, score 0'da kalır (satırda weight=0
    // sabitlenir).
    interaction::set_vote(&pool, oylayan.id, post, 1)
        .await
        .expect("seviye 0 oy verebilmeli");
    assert_eq!(sayaclar(&pool, post).await, (0, 1, 0));

    // Terfi: recompute_trust_levels'ın koşullarını sağlamayı beklemek
    // yerine kademe doğrudan yazılıyor (bkz. trust_level_yukselt).
    trust_level_yukselt(&pool, oylayan.id, 1).await;

    // Oyu geri çek. Eğer delta yanlışlıkla oy verenin BUGÜNKÜ (seviye 1)
    // kademesinden hesaplansaydı, "eski katkı" 1 sayılır ve score -1'e
    // düşerdi. Doğru davranış: satırda saklı weight=0 kullanıldığı için
    // eski katkı zaten 0, geri çekme score'u DEĞİŞTİRMEMELİ.
    let sonuc = interaction::set_vote(&pool, oylayan.id, post, 0)
        .await
        .expect("geri çekilebilmeli");

    assert_eq!(
        sonuc.score, 0,
        "terfi sonrası geri çekme skoru eksiye kaydırmamalı"
    );
    assert_eq!(
        sayaclar(&pool, post).await,
        (0, 0, 0),
        "geri çekme upvote sayacını da düşürmeli (ham sayaç, ağırlıktan bağımsız)"
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
