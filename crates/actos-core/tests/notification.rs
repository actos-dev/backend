//! `actos_core::notification` entegrasyon testleri.
//!
//! HTTP katmanı `crates/actos-api/tests/notifications_api.rs`'te; burada
//! domain katmanı doğrudan çağrılıyor (comment/interaction/moderation
//! yazma yollarındaki fan-out dahil).

use actos_core::{
    Storage,
    auth::{ActorRecord, ActorType, Grant, Permission, PermissionScope},
    comment,
    config::StorageConfig,
    content,
    cursor::{Cursor, SortKey},
    id::IdCodec,
    interaction, moderation, notification,
};
use sqlx::PgPool;

/// Tek bir global izin taşıyan sahte yetki kümesi — moderasyon
/// fonksiyonlarının yetki parametresi için.
#[allow(clippy::expect_used)]
fn global(permission: Permission) -> Vec<Grant> {
    vec![Grant {
        permission,
        scope: PermissionScope::Global,
        community_id: None,
    }]
}

/// A `Storage`/`IdCodec` pair for tests that never carry attachments (every
/// `seed_post`/`seed_comment` call in this file passes an empty file list) —
/// `create_post`/`create_comment` need them structurally, but
/// `attachment::create_for_content` returns before either is touched when
/// there is nothing to upload, so pointing `Storage` at an unreachable
/// address costs nothing (same pattern as `crates/actos-core/tests/
/// attachment.rs`'s `test_storage`).
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

/// Kimlik doğrulama yolundan geçmeden bir actor satırı oluşturur (bkz.
/// `crates/actos-core/tests/interaction.rs`'teki aynı isimli yardımcı
/// üzerindeki gerekçe: `auth::register`in Argon2 maliyetinden kaçınmak).
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
        None,
        "başlık",
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

/// `comment::create_comment` çağrısını sarmalar — bu dosyadaki her yorum
/// hiçbir dosya taşımıyor (bkz. [`test_storage`] üzerindeki gerekçe).
/// Döndürdüğü iç `bigint` id, `Some(...)` içinde `parent_id` olarak
/// yeniden kullanılabiliyor.
#[allow(clippy::expect_used)]
async fn seed_comment(
    pool: &PgPool,
    author: &ActorRecord,
    post_id: i64,
    parent_id: Option<i64>,
    body: &str,
) -> i64 {
    comment::create_comment(
        pool,
        &test_storage(),
        &test_id_codec(),
        author,
        post_id,
        parent_id,
        body,
        &[],
        8 * 1024 * 1024,
        i64::MAX,
        &[],
    )
    .await
    .expect("yorum oluşturulabilmeli")
    .id
}

/// Bir actor'ün gelen kutusundaki bildirim (kind, tetikleyen actor_id)
/// çiftlerini, en yeniden eskiye sırayla döner — testlerin karşılaştırma
/// yapması için ham `Notification`'dan daha kolay bir şekil.
#[allow(clippy::expect_used)]
async fn inbox_kinds(pool: &PgPool, actor_id: i64) -> Vec<(&'static str, Option<i64>)> {
    let page = notification::list_inbox(pool, actor_id, false, None, 100, &[])
        .await
        .expect("inbox okunabilmeli");
    page.items
        .into_iter()
        .map(|n| (n.kind.as_str(), n.actor.map(|a| a.id)))
        .collect()
}

// --- Fan-out sınırı: kök yazarı + doğrudan ebeveyn, BAŞKASI DEĞİL --------

/// Post'a doğrudan yazılan bir yorum yalnızca kök yazarına TEK bir
/// `comment_on_post` bildirimi üretmeli — ayrı bir `reply_to_comment`
/// ÜRETİLMEMELİ (ebeveyn zaten post'un kendisi).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn posta_dogrudan_yorum_yalniz_kok_yazarina_bildirim_uretir(pool: PgPool) {
    let post_yazari = seed_actor(&pool, "fanout_post_yazari").await;
    let yorumcu = seed_actor(&pool, "fanout_yorumcu").await;
    let post = seed_post(&pool, &post_yazari).await;

    seed_comment(&pool, &yorumcu, post, None, "ilk yorum").await;

    let post_yazari_kutu = inbox_kinds(&pool, post_yazari.id).await;
    assert_eq!(
        post_yazari_kutu,
        vec![("comment_on_post", Some(yorumcu.id))],
        "post yazarı tam olarak bir comment_on_post bildirimi almalı"
    );

    // Yorumcunun kendi kutusu boş kalmalı — kimse ona bildirim üretmedi.
    assert!(inbox_kinds(&pool, yorumcu.id).await.is_empty());
}

/// 32 seviyeye kadar uzanabilen bir zincirde, zincirin altına eklenen bir
/// yanıt yalnızca (a) kök post yazarına VE (b) doğrudan ebeveyninin
/// yazarına bildirim üretmeli — aradaki atalar (ör. torunun yanıtladığı
/// yorumun ebeveyni değil, büyük-ebeveyni) HİÇBİR BİLDİRİM ALMAMALI. Bu,
/// PLAN.md'nin açıkça istediği "fan-out sınırı" testi.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn derin_zincirde_yalniz_kok_ve_dogrudan_ebeveyn_bildirim_alir(pool: PgPool) {
    let post_yazari = seed_actor(&pool, "zincir_post_yazari").await;
    let a = seed_actor(&pool, "zincir_a").await;
    let b = seed_actor(&pool, "zincir_b").await;
    let c = seed_actor(&pool, "zincir_c").await;
    let post = seed_post(&pool, &post_yazari).await;

    // A, post'a doğrudan yorum yapar.
    let yorum_a = seed_comment(&pool, &a, post, None, "a yorumu").await;

    // B, A'nın yorumuna yanıt verir.
    let yorum_b = seed_comment(&pool, &b, post, Some(yorum_a), "b yaniti").await;

    // C, B'nin yorumuna yanıt verir — bu, testin asıl odağı.
    seed_comment(&pool, &c, post, Some(yorum_b), "c yaniti").await;

    // Post yazarı: HER üç yorumdan da comment_on_post bildirimi almalı
    // (kök yazarı her zaman bilgilendirilir).
    let post_yazari_kutu = inbox_kinds(&pool, post_yazari.id).await;
    assert_eq!(post_yazari_kutu.len(), 3, "{post_yazari_kutu:?}");
    assert!(
        post_yazari_kutu
            .iter()
            .all(|(kind, _)| *kind == "comment_on_post"),
        "{post_yazari_kutu:?}"
    );

    // A: yalnızca B'nin yanıtından bir `reply_to_comment` bildirimi almalı.
    // C'nin B'ye yanıtı A'yı İLGİLENDİRMEMELİ — bu tam olarak fan-out
    // sınırının test ettiği şey.
    let a_kutu = inbox_kinds(&pool, a.id).await;
    assert_eq!(
        a_kutu,
        vec![("reply_to_comment", Some(b.id))],
        "A yalnızca doğrudan kendi yorumuna gelen yanıttan bildirim almalı, torun yanıttan değil"
    );

    // B: yalnızca C'nin yanıtından bir `reply_to_comment` bildirimi almalı.
    let b_kutu = inbox_kinds(&pool, b.id).await;
    assert_eq!(b_kutu, vec![("reply_to_comment", Some(c.id))]);

    // C: kimseye yanıt vermedi kimse ona yanıt vermedi, kutusu boş.
    assert!(inbox_kinds(&pool, c.id).await.is_empty());
}

/// Kök yazarı ile doğrudan ebeveynin yazarı AYNI actor olduğunda
/// (post sahibinin kendi postuna açtığı bir yorum zincirinde birine yanıt
/// verilmesi), o actor'e AYNI yorum için iki ayrı bildirim (comment_on_post
/// + reply_to_comment) DEĞİL, tek bir bildirim gitmeli.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kok_ve_ebeveyn_ayni_actor_ise_tek_bildirim_gider(pool: PgPool) {
    let post_yazari = seed_actor(&pool, "dedup_post_yazari").await;
    let yanitlayan = seed_actor(&pool, "dedup_yanitlayan").await;
    let post = seed_post(&pool, &post_yazari).await;

    // Post yazarının KENDİSİ post'a bir yorum açıyor.
    let kendi_yorumu = seed_comment(&pool, &post_yazari, post, None, "kendi yorumum").await;

    // Kendi yorumuna karşı kendine bildirim gitmemeli (aşağıdaki testte
    // ayrıca doğrulanıyor), burada önemli olan bir sonraki adım.
    // Başka bir actor o yoruma yanıt veriyor: kök yazarı == ebeveyn yazarı
    // (ikisi de post_yazari).
    seed_comment(&pool, &yanitlayan, post, Some(kendi_yorumu), "yanit").await;

    let kutu = inbox_kinds(&pool, post_yazari.id).await;
    assert_eq!(
        kutu.len(),
        1,
        "kök yazarı ile ebeveyn yazarı aynı actor olduğunda tek bildirim beklenir: {kutu:?}"
    );
}

// --- Kendi eylemin sana bildirim üretmez ---------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kendi_postuna_kendi_yorumun_bildirim_uretmez(pool: PgPool) {
    let actor = seed_actor(&pool, "kendine_yorum").await;
    let post = seed_post(&pool, &actor).await;

    seed_comment(&pool, &actor, post, None, "kendi yorumum").await;

    assert!(
        inbox_kinds(&pool, actor.id).await.is_empty(),
        "kendi postuna kendi yorumu bildirim üretmemeli"
    );
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn moderatorun_kendi_icerigini_silmesi_bildirim_uretmez(pool: PgPool) {
    let moderator = seed_actor(&pool, "kendine_mod").await;
    let post = seed_post(&pool, &moderator).await;

    moderation::moderate_delete_content(
        &pool,
        moderator.id,
        &global(Permission::ContentDelete),
        post,
        "kendi içeriğimi siliyorum",
    )
    .await
    .expect("silinebilmeli");

    assert!(
        inbox_kinds(&pool, moderator.id).await.is_empty(),
        "kendi içeriğini silen moderatöre bildirim gitmemeli"
    );
}

// --- Takip ----------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yeni_takipte_bildirim_uretilir(pool: PgPool) {
    let takip_edilen = seed_actor(&pool, "takip_edilen").await;
    let takipci = seed_actor(&pool, "takipci").await;

    interaction::follow(&pool, takipci.id, &takip_edilen.username)
        .await
        .expect("takip edilebilmeli");

    let kutu = inbox_kinds(&pool, takip_edilen.id).await;
    assert_eq!(kutu, vec![("new_follower", Some(takipci.id))]);
}

/// Aynı takibi tekrar tekrar göndermek (idempotent `PUT`) ikinci bir
/// bildirim üretmemeli — `ON CONFLICT DO NOTHING` yüzünden gerçek bir
/// state değişikliği olmuyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tekrarlanan_takip_ikinci_bildirim_uretmez(pool: PgPool) {
    let takip_edilen = seed_actor(&pool, "tekrar_takip_edilen").await;
    let takipci = seed_actor(&pool, "tekrar_takipci").await;

    for _ in 0..3 {
        interaction::follow(&pool, takipci.id, &takip_edilen.username)
            .await
            .expect("takip edilebilmeli");
    }

    let kutu = inbox_kinds(&pool, takip_edilen.id).await;
    assert_eq!(kutu.len(), 1, "{kutu:?}");
}

// --- Moderasyon -------------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn icerik_silme_yazarina_bildirim_uretir(pool: PgPool) {
    let admin = seed_actor(&pool, "mod_admin").await;
    let yazar = seed_actor(&pool, "mod_yazar").await;
    let post = seed_post(&pool, &yazar).await;

    moderation::moderate_delete_content(
        &pool,
        admin.id,
        &global(Permission::ContentDelete),
        post,
        "kural ihlali",
    )
    .await
    .expect("silinebilmeli");

    let kutu = inbox_kinds(&pool, yazar.id).await;
    assert_eq!(kutu, vec![("moderation_action", Some(admin.id))]);
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ban_banli_actore_bildirim_uretir(pool: PgPool) {
    let admin = seed_actor(&pool, "ban_admin").await;
    let hedef = seed_actor(&pool, "ban_hedef").await;

    moderation::ban_actor(
        &pool,
        admin.id,
        &global(Permission::MemberBan),
        &hedef.username,
        "kural ihlali",
        None,
        None,
        false,
    )
    .await
    .expect("banlanabilmeli");

    let kutu = inbox_kinds(&pool, hedef.id).await;
    assert_eq!(kutu, vec![("moderation_action", Some(admin.id))]);
}

// --- Okundu işaretleme: idempotent ----------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tekil_okundu_isaretleme_idempotent(pool: PgPool) {
    let alici = seed_actor(&pool, "tekil_okundu_alici").await;
    let gonderen = seed_actor(&pool, "tekil_okundu_gonderen").await;
    interaction::follow(&pool, gonderen.id, &alici.username)
        .await
        .expect("takip edilebilmeli");

    let page = notification::list_inbox(&pool, alici.id, false, None, 10, &[])
        .await
        .expect("inbox okunabilmeli");
    let notif_id = page.items[0].id;

    // İlk çağrı gerçekten okundu işaretlemeli.
    notification::mark_read(&pool, alici.id, notif_id)
        .await
        .expect("okundu işaretlenebilmeli");
    let once_read_at = notification::list_inbox(&pool, alici.id, false, None, 10, &[])
        .await
        .expect("inbox okunabilmeli")
        .items[0]
        .read_at
        .expect("read_at dolu olmalı");

    // İkinci çağrı hata vermemeli VE read_at'i İLERİ ATMAMALI.
    notification::mark_read(&pool, alici.id, notif_id)
        .await
        .expect("ikinci çağrı da başarılı olmalı (idempotent)");
    let twice_read_at = notification::list_inbox(&pool, alici.id, false, None, 10, &[])
        .await
        .expect("inbox okunabilmeli")
        .items[0]
        .read_at
        .expect("read_at hâlâ dolu olmalı");

    assert_eq!(
        once_read_at, twice_read_at,
        "ikinci mark_read çağrısı read_at'i ileri atmamalı"
    );
}

/// Başka bir actor'ün bildirimini okundu işaretlemeye çalışmak `NotFound`
/// dönmeli — mevcudiyet bilgisi sızdırılmamalı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn baskasinin_bildirimini_isaretlemek_notfound_doner(pool: PgPool) {
    let alici = seed_actor(&pool, "baskasi_alici").await;
    let gonderen = seed_actor(&pool, "baskasi_gonderen").await;
    let yabanci = seed_actor(&pool, "baskasi_yabanci").await;
    interaction::follow(&pool, gonderen.id, &alici.username)
        .await
        .expect("takip edilebilmeli");

    let notif_id = notification::list_inbox(&pool, alici.id, false, None, 10, &[])
        .await
        .expect("inbox okunabilmeli")
        .items[0]
        .id;

    let sonuc = notification::mark_read(&pool, yabanci.id, notif_id).await;
    assert!(matches!(
        sonuc,
        Err(actos_core::Error::NotFound("notification"))
    ));
}

/// Toplu okundu işaretleme: `cursor` verilmezse TÜMÜ, verilirse yalnızca o
/// cursor'a kadar olanlar işaretlenir; ikinci bir çağrı yeni satır bulamaz
/// (idempotent).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn toplu_okundu_isaretleme_cursor_ve_idempotentlik(pool: PgPool) {
    let alici = seed_actor(&pool, "toplu_alici").await;
    let a = seed_actor(&pool, "toplu_a").await;
    let b = seed_actor(&pool, "toplu_b").await;
    let c = seed_actor(&pool, "toplu_c").await;
    let post = seed_post(&pool, &alici).await;

    // Üç ayrı comment_on_post bildirimi üret (en yeniden eskiye: c, b, a).
    seed_comment(&pool, &a, post, None, "a").await;
    seed_comment(&pool, &b, post, None, "b").await;
    seed_comment(&pool, &c, post, None, "c").await;

    assert_eq!(
        notification::count_unread(&pool, alici.id, &[])
            .await
            .unwrap(),
        3
    );

    // İlk sayfayı `limit=2` ile çek: en yeni iki bildirim (c, b) + bir
    // sonraki sayfaya işaret eden cursor (b'nin konumu).
    let ilk_sayfa = notification::list_inbox(&pool, alici.id, false, None, 2, &[])
        .await
        .expect("inbox okunabilmeli");
    assert_eq!(ilk_sayfa.items.len(), 2);
    let cursor = ilk_sayfa
        .next_cursor
        .expect("ikinci sayfa olmalı (3 öğe, limit 2)");

    // "Şu cursor'a kadar hepsi": yalnızca ilk sayfadaki iki öğe (c, b)
    // okundu işaretlenmeli, a HÂLÂ okunmamış kalmalı.
    let marked = notification::mark_all_read(&pool, alici.id, Some(cursor))
        .await
        .expect("toplu işaretleme başarılı olmalı");
    assert_eq!(
        marked, 2,
        "yalnızca cursor'a kadar olan iki bildirim işaretlenmeli"
    );
    assert_eq!(
        notification::count_unread(&pool, alici.id, &[])
            .await
            .unwrap(),
        1
    );

    // Aynı çağrı tekrarlanırsa (idempotent) artık YENİ bir satır bulamaz.
    let marked_again = notification::mark_all_read(&pool, alici.id, Some(cursor))
        .await
        .expect("tekrar çağrı başarılı olmalı");
    assert_eq!(marked_again, 0);

    // `cursor: None` → kalan TÜMÜNÜ işaretle.
    let marked_rest = notification::mark_all_read(&pool, alici.id, None)
        .await
        .expect("kalanı işaretleme başarılı olmalı");
    assert_eq!(marked_rest, 1);
    assert_eq!(
        notification::count_unread(&pool, alici.id, &[])
            .await
            .unwrap(),
        0
    );
}

// --- `?unread=true` filtresi + `unread_count` ------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn unread_filtresi_ve_sayaci_dogru(pool: PgPool) {
    let alici = seed_actor(&pool, "unread_alici").await;
    let a = seed_actor(&pool, "unread_a").await;
    let b = seed_actor(&pool, "unread_b").await;
    let post = seed_post(&pool, &alici).await;

    seed_comment(&pool, &a, post, None, "a").await;
    seed_comment(&pool, &b, post, None, "b").await;

    assert_eq!(
        notification::count_unread(&pool, alici.id, &[])
            .await
            .unwrap(),
        2
    );

    // Birini okundu işaretle.
    let ilk = notification::list_inbox(&pool, alici.id, false, None, 1, &[])
        .await
        .expect("inbox okunabilmeli");
    notification::mark_read(&pool, alici.id, ilk.items[0].id)
        .await
        .expect("okundu işaretlenebilmeli");

    assert_eq!(
        notification::count_unread(&pool, alici.id, &[])
            .await
            .unwrap(),
        1
    );

    // `unread_only=true` yalnızca kalan okunmamışı döner.
    let unread_sayfa = notification::list_inbox(&pool, alici.id, true, None, 10, &[])
        .await
        .expect("inbox okunabilmeli");
    assert_eq!(unread_sayfa.items.len(), 1);
    assert!(unread_sayfa.items[0].read_at.is_none());

    // Filtresiz liste hâlâ ikisini de döner.
    let hepsi = notification::list_inbox(&pool, alici.id, false, None, 10, &[])
        .await
        .expect("inbox okunabilmeli");
    assert_eq!(hepsi.items.len(), 2);
}

// --- Cursor tutarlılığı -----------------------------------------------------

/// Cursor'ın `New` dışında bir sıralamayla (ör. `Top`) çözülmesi
/// [`actos_core::Error::InvalidCursor`] üretmeli — bu liste yalnızca `New`
/// sıralamasını destekliyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn yanlis_siralamali_cursor_reddedilir(pool: PgPool) {
    let alici = seed_actor(&pool, "yanlis_cursor_alici").await;

    let yanlis_cursor = Cursor {
        sort: SortKey::Top { score: 0 },
        id: 1,
    };

    let sonuc =
        notification::list_inbox(&pool, alici.id, false, Some(yanlis_cursor), 10, &[]).await;
    assert!(matches!(sonuc, Err(actos_core::Error::InvalidCursor)));

    let sonuc2 = notification::mark_all_read(&pool, alici.id, Some(yanlis_cursor)).await;
    assert!(matches!(sonuc2, Err(actos_core::Error::InvalidCursor)));
}
