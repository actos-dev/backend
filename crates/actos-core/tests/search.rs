//! `actos_core::search` entegrasyon testleri (domain katmanı, HTTP yok).
//!
//! HTTP uçları `crates/actos-api/tests/search_api.rs`'te; burada yalnızca
//! `actos_core::search`'ün SQL/sıralama/cursor mantığı, veritabanına karşı
//! doğrudan çağrılarak sınanıyor (`tag.rs`/`crates/actos-api/tests/
//! tags_api.rs` ile aynı iş bölümü).

use actos_core::{
    Storage,
    auth::{self, ActorType},
    comment,
    config::StorageConfig,
    content::{self, ContentType},
    cursor::Cursor,
    id::IdCodec,
    search,
};
use sqlx::PgPool;

/// See the identical helper (and its rationale) in `crates/actos-core/tests/
/// notification.rs` — neither `seed_post` nor `seed_comment` here ever
/// carries a file.
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

/// Bir actor oluşturur, döner: `ActorRecord`.
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> auth::ActorRecord {
    let reg = auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("actor oluşturulabilmeli");
    reg.actor
}

/// Bir post oluşturur, döner: iç `bigint` id.
#[allow(clippy::expect_used)]
async fn seed_post(pool: &PgPool, author: &auth::ActorRecord, title: &str, body: &str) -> i64 {
    content::create_post(
        pool,
        &test_storage(),
        &test_id_codec(),
        author,
        None,
        title,
        body,
        &[],
        &[],
        8 * 1024 * 1024,
        i64::MAX,
    )
    .await
    .expect("post oluşturulabilmeli")
    .id
}

/// Bir yorum oluşturur (verilen post'un doğrudan çocuğu), döner: iç id.
#[allow(clippy::expect_used)]
async fn seed_comment(pool: &PgPool, author: &auth::ActorRecord, post_id: i64, body: &str) -> i64 {
    comment::create_comment(
        pool,
        &test_storage(),
        &test_id_codec(),
        author,
        post_id,
        None,
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

// --- Türkçe aksan-duyarsızlığı -------------------------------------------

/// Canlı veritabanında doğrulanan `actos_simple` konfigürasyonu iki yönde
/// de çalışmalı: aksansız sorgu aksanlı içeriği bulmalı, aksanlı sorgu
/// aksansız içeriği bulmalı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn turkce_aksan_duyarsizligi_iki_yonde(pool: PgPool) {
    let author = seed_actor(&pool, "aksantest").await;

    let aksanli = seed_post(&pool, &author, "Yeni sürücü çıktı", "gövde").await;
    let aksansiz = seed_post(&pool, &author, "Yeni surucu haberi", "govde iki").await;

    // Aksansız sorgu ("surucu") aksanlı başlığı ("sürücü") bulmalı.
    let sayfa = search::search_content(&pool, "surucu", ContentType::Post, None, 10, &[])
        .await
        .expect("arama çalışmalı");
    let ids: Vec<i64> = sayfa.items.iter().map(|c| c.id).collect();
    assert!(
        ids.contains(&aksanli),
        "aksansız sorgu aksanlı başlığı bulmalı: {ids:?}"
    );

    // Aksanlı sorgu ("sürücü") aksansız başlığı ("surucu") bulmalı.
    let sayfa = search::search_content(&pool, "sürücü", ContentType::Post, None, 10, &[])
        .await
        .expect("arama çalışmalı");
    let ids: Vec<i64> = sayfa.items.iter().map(|c| c.id).collect();
    assert!(
        ids.contains(&aksansiz),
        "aksanlı sorgu aksansız başlığı bulmalı: {ids:?}"
    );
}

// --- A/B ağırlığı: title > body ------------------------------------------

/// `title` A ağırlıklı, `body` B ağırlıklı; eşit koşullar altında title
/// eşleşmesi body eşleşmesinden yüksek sıralanmalı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn title_eslesmesi_body_eslesmesinden_yuksek_siralaniyor(pool: PgPool) {
    let author = seed_actor(&pool, "agirliktest").await;

    let baslikta = seed_post(&pool, &author, "widget haberleri", "alakasız gövde").await;
    let govdede = seed_post(&pool, &author, "alakasız başlık", "burada widget geçiyor").await;

    let sayfa = search::search_content(&pool, "widget", ContentType::Post, None, 10, &[])
        .await
        .expect("arama çalışmalı");

    let ids: Vec<i64> = sayfa.items.iter().map(|c| c.id).collect();
    assert_eq!(ids.len(), 2, "her iki post da eşleşmeli: {ids:?}");
    assert_eq!(
        ids[0], baslikta,
        "title eşleşmesi önce gelmeli (A ağırlığı > B ağırlığı): {ids:?}"
    );
    assert_eq!(ids[1], govdede, "{ids:?}");
}

// --- Karışımın gerçekten çalıştığı: popülerlik + tazelik ------------------
//
// Bu iki test **bilinçli olarak** `score`/`created_at`'i `sqlx::query!` ile
// doğrudan güncelliyor, `hot_score`'a hiç dokunmuyor — tıpkı
// `tags_api.rs::etiket_postlari_sort_top_skora_gore`'ın `score`'u
// güncellerken yaptığı gibi (üretimde bu ikisi yalnızca `interaction::
// set_vote` üzerinden birlikte güncellenir; burada testin amacı gerçek oy
// akışını simüle etmek değil, sıralamanın *hangi kolona* baktığını ortaya
// çıkarmak).
//
// **Bu bilinçli bir teşhis aracı:** eski formül sıralamayı yalnızca
// (bayatlayabilen, yalnızca oy anında/periyodik job'da tazelenen)
// `hot_score` kolonundan okuyordu. `hot_score` dokunulmadan bırakılırsa iki
// postun da `hot_score`'u (gerçek oluşturma anını yansıtan, birbirine çok
// yakın) neredeyse eşit kalır, `ts_rank` de eşittir (metin birebir aynı) —
// sıralama tamamen `contents.id DESC` ikincil anahtarına düşer. Testler bu
// yüzden **id'si küçük olan postu** (`ilk`) kazanması gereken taraf olarak
// kuruyor: eski formülde id tie-break bunun TAM TERSİNİ üretir (id'si büyük
// olan `ikinci` kazanır), bu yüzden bu testler eski formülle **kırmızı**
// çalışır — bunu değiştirmeden önce bizzat doğruladım (bkz. bu dosyanın
// üzerindeki görev raporu). Yeni formül `score`/`created_at`'i doğrudan (
// `hot_score` üzerinden değil) okuduğu için bu tie-break'e hiç düşmüyor.

/// Metin birebir aynı, skorları çok farklı iki post → yüksek skorlu önce
/// gelmeli. Eski formülde `hot_score` güncellenmediği için bu sinyal görünmez
/// oluyordu (bkz. yukarıdaki blok yorumu).
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn skor_farki_yakin_alakada_siralamayi_degistiriyor(pool: PgPool) {
    let author = seed_actor(&pool, "skorkarisimtest").await;

    // `ilk` küçük id alır (önce oluşturuluyor) ve YÜKSEK skoru alacak —
    // eski formülün id tie-break'i tam tersini (büyük id'li `ikinci`'yi)
    // seçerdi, bu yüzden bu düzen kusuru ortaya çıkarıyor.
    let ilk = seed_post(&pool, &author, "aynı metin karışım testi", "aynı gövde").await;
    let ikinci = seed_post(&pool, &author, "aynı metin karışım testi", "aynı gövde").await;

    sqlx::query!(r#"UPDATE contents SET score = 1000 WHERE id = $1"#, ilk)
        .execute(&pool)
        .await
        .expect("skor güncellenebilmeli");
    sqlx::query!(r#"UPDATE contents SET score = 1 WHERE id = $1"#, ikinci)
        .execute(&pool)
        .await
        .expect("skor güncellenebilmeli");

    let sayfa = search::search_content(&pool, "karışım", ContentType::Post, None, 10, &[])
        .await
        .expect("arama çalışmalı");

    let ids: Vec<i64> = sayfa.items.iter().map(|c| c.id).collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_eq!(
        ids[0], ilk,
        "yüksek skorlu (score=1000) post önce gelmeli: {ids:?}"
    );
}

/// Metin ve skor aynı, `created_at`'leri belirgin farklı iki post → yeni
/// olan önce gelmeli. Eski formülde `hot_score` güncellenmediği için bu
/// sinyal de görünmez oluyordu.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn tazelik_farki_yakin_alakada_siralamayi_degistiriyor(pool: PgPool) {
    let author = seed_actor(&pool, "tazelikkarisimtest").await;

    // `ilk` küçük id alır ve GERÇEKTEN TAZE kalacak (created_at'e
    // dokunulmuyor); `ikinci` büyük id alır ama 2 yıl geriye çekiliyor —
    // eski formülün id tie-break'i yine tam tersini (büyük id'li,
    // aslında eski olan `ikinci`'yi) seçerdi.
    let ilk = seed_post(&pool, &author, "aynı metin tazelik testi", "aynı gövde").await;
    let ikinci = seed_post(&pool, &author, "aynı metin tazelik testi", "aynı gövde").await;

    sqlx::query!(
        r#"UPDATE contents SET created_at = now() - interval '2 years' WHERE id = $1"#,
        ikinci
    )
    .execute(&pool)
    .await
    .expect("created_at güncellenebilmeli");

    let sayfa = search::search_content(&pool, "tazelik", ContentType::Post, None, 10, &[])
        .await
        .expect("arama çalışmalı");

    let ids: Vec<i64> = sayfa.items.iter().map(|c| c.id).collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_eq!(
        ids[0], ilk,
        "gerçekten taze olan post (2 yıl önce değil) önce gelmeli: {ids:?}"
    );
}

// --- type filtresi ---------------------------------------------------------

/// `type=post`/`type=comment`/`type=actor` yalnızca kendi türünü döner.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn type_filtresi_dogru_calisiyor(pool: PgPool) {
    // Yazarın kullanıcı adı bilinçli olarak sorgu kelimesinden (`filtretest`)
    // tamamen farklı: aksi hâlde trigram benzerliği (`similarity`) yazarı da
    // actor aramasına dahil edebilir ve testin "yalnızca bir actor eşleşti"
    // iddiasını yanlış sebeple kırardı.
    let author = seed_actor(&pool, "yazarhesabi").await;

    let post_id = seed_post(&pool, &author, "filtretest postu", "gövde").await;
    let baglantili_post = seed_post(&pool, &author, "ilgisiz", "ilgisiz gövde").await;
    let comment_id = seed_comment(&pool, &author, baglantili_post, "filtretest yorumu").await;
    let _actor = seed_actor(&pool, "filtretest_kisi").await;

    let post_sonuc = search::search_content(&pool, "filtretest", ContentType::Post, None, 10, &[])
        .await
        .expect("post araması çalışmalı");
    let post_ids: Vec<i64> = post_sonuc.items.iter().map(|c| c.id).collect();
    assert_eq!(post_ids, vec![post_id], "{post_ids:?}");

    let comment_sonuc =
        search::search_content(&pool, "filtretest", ContentType::Comment, None, 10, &[])
            .await
            .expect("yorum araması çalışmalı");
    let comment_ids: Vec<i64> = comment_sonuc.items.iter().map(|c| c.id).collect();
    assert_eq!(comment_ids, vec![comment_id], "{comment_ids:?}");

    let actor_sonuc = search::search_actors(&pool, "filtretest", None, 10)
        .await
        .expect("actor araması çalışmalı");
    assert_eq!(actor_sonuc.items.len(), 1, "{actor_sonuc:?}");
    assert_eq!(actor_sonuc.items[0].username, "filtretest_kisi");
}

// --- Silinmiş içerik -----------------------------------------------------

/// Soft-delete edilmiş bir post arama sonuçlarında hiç görünmemeli.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_icerik_aramada_cikmiyor(pool: PgPool) {
    let author = seed_actor(&pool, "silmetest").await;
    let post_id = seed_post(&pool, &author, "silinecek arama konusu", "gövde").await;

    content::delete_post(&pool, post_id, author.id, &[])
        .await
        .expect("post silinebilmeli");

    let sayfa = search::search_content(&pool, "silinecek", ContentType::Post, None, 10, &[])
        .await
        .expect("arama çalışmalı");
    assert!(
        sayfa.items.is_empty(),
        "silinmiş post sonuçlarda görünmemeli: {:?}",
        sayfa.items.iter().map(|c| c.id).collect::<Vec<_>>()
    );
}

/// Soft-delete edilmiş bir actor arama sonuçlarında hiç görünmemeli.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_actor_aramada_cikmiyor(pool: PgPool) {
    let author = seed_actor(&pool, "silinecekaktor").await;

    sqlx::query!(
        r#"UPDATE actors SET deleted_at = now() WHERE id = $1"#,
        author.id
    )
    .execute(&pool)
    .await
    .expect("actor soft-delete edilebilmeli");

    let sayfa = search::search_actors(&pool, "silinecekaktor", None, 10)
        .await
        .expect("arama çalışmalı");
    assert!(sayfa.items.is_empty(), "{:?}", sayfa.items);
}

// --- Cursor'lu sayfalama ----------------------------------------------------

/// İki sayfa çekildiğinde tekrar eden ya da atlanan kayıt olmamalı.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn cursorlu_sayfalama_tekrar_atlama_yok(pool: PgPool) {
    let author = seed_actor(&pool, "sayfalamatest").await;

    let mut beklenen: Vec<i64> = Vec::new();
    for i in 0..3 {
        let id = seed_post(&pool, &author, &format!("sayfalama konusu {i}"), "gövde").await;
        beklenen.push(id);
    }

    let mut gorulen: Vec<i64> = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let sayfa = search::search_content(&pool, "sayfalama", ContentType::Post, cursor, 1, &[])
            .await
            .expect("arama çalışmalı");
        assert_eq!(sayfa.items.len(), 1, "her sayfa tam olarak 1 öğe taşımalı");
        gorulen.push(sayfa.items[0].id);
        match sayfa.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        // Sonsuz döngüye karşı savunma: 3 post var, 3 sayfadan fazla olamaz.
        assert!(gorulen.len() <= 3, "beklenenden fazla sayfa döndü");
    }

    gorulen.sort_unstable();
    let mut beklenen_sirali = beklenen.clone();
    beklenen_sirali.sort_unstable();
    assert_eq!(
        gorulen, beklenen_sirali,
        "sayfalar arasında tekrar eden ya da atlanan kayıt var"
    );
}

// --- Boş/anlamsız q ----------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bos_ve_anlamsiz_q_bos_liste_donuyor(pool: PgPool) {
    let author = seed_actor(&pool, "bosaramatest").await;
    seed_post(&pool, &author, "herhangi bir başlık", "herhangi bir gövde").await;

    let bos = search::search_content(&pool, "", ContentType::Post, None, 10, &[])
        .await
        .expect("boş sorgu hata değil boş liste dönmeli");
    assert!(bos.items.is_empty(), "{:?}", bos.items);

    let sadece_bosluk = search::search_content(&pool, "   ", ContentType::Post, None, 10, &[])
        .await
        .expect("yalnızca boşluktan oluşan sorgu hata değil boş liste dönmeli");
    assert!(sadece_bosluk.items.is_empty(), "{:?}", sadece_bosluk.items);

    let actor_bos = search::search_actors(&pool, "", None, 10)
        .await
        .expect("actor aramasında da boş sorgu boş liste dönmeli");
    assert!(actor_bos.items.is_empty(), "{:?}", actor_bos.items);
}
