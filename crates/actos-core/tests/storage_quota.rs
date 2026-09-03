//! `actos_core::attachment` depolama kotası entegrasyon testleri — Faz 18.A,
//! bkz. NOTES.md §9.8 (*"biri 1000 hesap açıp her biriyle 100 tane 8 MB'lık
//! görsel yükleyip diski doldurabilir"*).
//!
//! **Neden `crates/actos-core/tests/attachment.rs`'e değil, ayrı bir dosya:**
//! o dosya bilerek erişilemez bir `Storage` kullanıyor (`http://127.0.0.1:1`)
//! — yalnızca [`attachment::cleanup_orphaned`]'in veritabanı tarafını
//! sınıyor, hiçbir zaman gerçekten yüklemiyor (bkz. o dosyanın modül
//! dokümanı). Kota kontrolü ise tam olarak [`attachment::create_attachment`]
//! içinde, gerçek bir `storage.put_object`'ten **önce** çalışıyor — bunu
//! anlamlı biçimde sınamak gerçek MinIO'ya (bkz. `docker-compose.yml`,
//! `127.0.0.1:3103`) karşı gerçek bir yükleme yapmayı gerektiriyor.
//! `crates/actos-api/tests/*`'teki `POST /uploads`'a değen testler de aynı
//! sebeple bunu hiç yapmıyor (`test_config()` orada da bilerek erişilemez
//! bir adrese işaret ediyor, ek satırları ham `INSERT` ile "seed"leniyor)
//! — yani bu dosya, projede `create_attachment`'ın gerçek depolamaya karşı
//! uçtan uca çalıştığı tek yer.
//!
//! **Boyut determinizmi:** `media::process_image` girdi baytlarından
//! deterministik bir WebP üretiyor (yeniden kodlama, rastgelelik yok — bkz.
//! `crates/actos-core/tests/media.rs`'teki aynı varsayım). Bu sayede testler
//! `create_attachment`'ı çağırmadan **önce** aynı fonksiyonu doğrudan
//! çağırıp nihai `byte_size`'ı öğrenebiliyor ve kotayı buna göre, kesin
//! sayılarla kuruyor — "yaklaşık büyük bir dosya" yerine "tam N bayt"
//! sınaması.

use actos_core::{
    Error, Storage, attachment,
    auth::{self as core_auth, ActorType},
    config::Config,
    id::IdCodec,
    media,
};
use sqlx::PgPool;

/// `media::process_image`'a geçilen üst sınır — testteki görseller çok
/// küçük, sınırın kendisi hiçbir testte tetiklenmiyor, yalnızca fonksiyonun
/// imzası istiyor.
const MAX_UPLOAD: usize = 8 * 1024 * 1024;

#[allow(clippy::expect_used)]
fn real_env() -> Config {
    Config::from_env().expect(
        "ortam .env'den okunabilmeli — `set -a && . ./.env && set +a` ile \
         export edilmiş olmalı (bkz. docker-compose.yml)",
    )
}

/// **Gerçek** MinIO'ya işaret eden bir `Storage` — `crates/actos-core/
/// tests/attachment.rs`'teki `test_storage()`'ın tam tersi (o bilerek
/// erişilemez). `create_attachment` gerçekten `storage.put_object`
/// çağırdığı için burada gerçek bir uç nokta şart.
#[allow(clippy::expect_used)]
fn real_storage() -> Storage {
    Storage::new(&real_env().storage)
}

#[allow(clippy::expect_used)]
fn real_id_codec() -> IdCodec {
    IdCodec::new(&real_env().security.id_obfuscation_key).expect("geçerli anahtar")
}

/// Kimlik doğrulama yolundan geçmeden bir actor oluşturur — `crates/
/// actos-core/tests/attachment.rs`'teki `seed_actor` ile aynı desen/gerekçe
/// (her test dosyası kendi bağımsız binary'si, paylaşılan bir yardımcı yok).
#[allow(clippy::expect_used)]
async fn seed_actor(pool: &PgPool, username: &str) -> i64 {
    core_auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("fixture actor oluşturulabilmeli")
        .actor
        .id
}

/// Verilen boyutlarda gerçek bir PNG üretir — `crates/actos-core/tests/
/// media.rs`'teki `gercek_png` ile birebir aynı (ayrı test binary'si,
/// tekrar yazıldı).
#[allow(clippy::expect_used)]
fn gercek_png(genislik: u32, yukseklik: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(genislik, yukseklik, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
    });
    let mut cikti = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut cikti, image::ImageFormat::Png)
        .expect("png kodlanabilmeli");
    cikti.into_inner()
}

/// Bir PNG'nin `create_attachment` tarafından üretilecek **nihai** WebP
/// boyutunu, gerçekten yüklemeden önce öğrenir — bkz. modül dokümanındaki
/// "boyut determinizmi" notu.
#[allow(clippy::expect_used)]
fn islenmis_boyut(png: &[u8]) -> i64 {
    let islenmis = media::process_image(png, MAX_UPLOAD).expect("geçerli png işlenebilmeli");
    i64::try_from(islenmis.data.len()).expect("boyut i64'e sığmalı")
}

// --- `StorageQuotaConfig::for_trust_level` (saf fonksiyon, DB/depolama yok) -

#[test]
fn for_trust_level_kademeleri_doğru_eşler_ve_aralık_dışını_kenetler() {
    let quota = real_env().storage_quota;

    assert_eq!(quota.for_trust_level(0), quota.trust_level_0_bytes);
    assert_eq!(quota.for_trust_level(1), quota.trust_level_1_bytes);
    assert_eq!(quota.for_trust_level(2), quota.trust_level_2_bytes);

    // Şema `CHECK (trust_level BETWEEN 0 AND 2)` ile garanti veriyor ama bu
    // fonksiyon savunmacı — aralık dışı bir değer en yakın uca kenetlenir.
    assert_eq!(quota.for_trust_level(-1), quota.trust_level_0_bytes);
    assert_eq!(quota.for_trust_level(99), quota.trust_level_2_bytes);

    // Kademeler artan sırada olmalı — `StorageQuotaConfig::validate`'in
    // `Config::from_env()` içinde zaten doğruladığı şart, burada da
    // (belgelenmiş varsayımı doğrulamak için) tekrar sınanıyor.
    assert!(quota.trust_level_0_bytes <= quota.trust_level_1_bytes);
    assert!(quota.trust_level_1_bytes <= quota.trust_level_2_bytes);
}

// --- `total_storage_bytes` ------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn total_storage_bytes_hiç_yüklemesi_olmayan_actor_için_sıfır_döner(pool: PgPool) {
    let actor_id = seed_actor(&pool, "kota_bos_actor").await;

    let toplam = attachment::total_storage_bytes(&pool, actor_id)
        .await
        .expect("sorgulanabilmeli");

    assert_eq!(
        toplam, 0,
        "hiç yüklemesi olmayan actor için SUM NULL döner, COALESCE ile 0'a çevrilmeli"
    );
}

// --- Kota aşımı: reddediliyor, mesaj kullanım bilgisi taşıyor -------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
#[allow(clippy::expect_used)]
async fn kota_aşılınca_yükleme_reddediliyor_mesaj_kullanım_bilgisi_taşıyor(pool: PgPool) {
    let storage = real_storage();
    let id_codec = real_id_codec();
    let actor_id = seed_actor(&pool, "kota_asan_actor").await;

    let png = gercek_png(32, 32);
    let boyut = islenmis_boyut(&png);

    // Kota tam olarak 1.5 yükleme kadar: ilk yükleme (boyut) rahat sığar,
    // ikinci yükleme toplamı (2×boyut) kotayı (1.5×boyut) aşar — sınırda
    // değil, kesin bir aşım (`saturating_add` ve `>` karşılaştırmasının her
    // ikisi de bu testte gerçekten tetikleniyor).
    let kota = boyut + boyut / 2;

    let ilk =
        attachment::create_attachment(&pool, &storage, &id_codec, actor_id, &png, MAX_UPLOAD, kota)
            .await
            .expect("kotanın altındaki ilk yükleme kabul edilmeli");
    assert_eq!(ilk.byte_size, boyut);

    let hata =
        attachment::create_attachment(&pool, &storage, &id_codec, actor_id, &png, MAX_UPLOAD, kota)
            .await
            .expect_err("kotayı aşan ikinci yükleme reddedilmeli");

    match hata {
        Error::Validation(mesaj) => {
            // Mesaj üç ayrı sayıyı da taşımalı: şu anki kullanım (boyut),
            // kademe sınırı (kota — bilerek `boyut`tan FARKLI seçildi ki bu
            // assert "hangi sayı olursa olsun geçer" tuzağına düşmesin) ve
            // bu isteğin ekleyeceği bayt (boyut). Kullanıcı/ajan bu üçünü
            // görmeden "ne kadar yer açmam gerekiyor" sorusunu yanıtlayamaz.
            assert!(
                mesaj.contains(&boyut.to_string()),
                "mesaj mevcut kullanımı/yeni yükleme boyutunu içermeli: {mesaj}"
            );
            assert!(
                mesaj.contains(&kota.to_string()),
                "mesaj kademe kotasını içermeli: {mesaj}"
            );
            assert!(
                mesaj.contains("quota"),
                "mesaj 'quota' kelimesini içermeli: {mesaj}"
            );
        }
        diğer => panic!("beklenen Error::Validation: {diğer:?}"),
    }

    // Temizlik: gerçek MinIO'ya yazılan tek nesneyi geri sil.
    attachment::delete_attachment(&pool, &storage, ilk.id, actor_id)
        .await
        .expect("temizlik: ilk yükleme silinebilmeli");
}

// --- Silme kotayı boşaltıyor ------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
#[allow(clippy::expect_used)]
async fn ek_silinince_kota_boşalıyor(pool: PgPool) {
    let storage = real_storage();
    let id_codec = real_id_codec();
    let actor_id = seed_actor(&pool, "kota_bosalan_actor").await;

    let png = gercek_png(32, 32);
    let boyut = islenmis_boyut(&png);
    // Kota tam olarak tek yüklemelik: ikinci yükleme denemesi kotanın
    // gerçekten "dolu" olduğunu kanıtlıyor, silme sonrası üçüncü deneme de
    // "gerçekten boşaldığını" kanıtlıyor.
    let kota = boyut;

    let ilk =
        attachment::create_attachment(&pool, &storage, &id_codec, actor_id, &png, MAX_UPLOAD, kota)
            .await
            .expect("kotaya tam sığan ilk yükleme kabul edilmeli");

    assert_eq!(
        attachment::total_storage_bytes(&pool, actor_id)
            .await
            .expect("sorgulanabilmeli"),
        boyut
    );

    attachment::create_attachment(&pool, &storage, &id_codec, actor_id, &png, MAX_UPLOAD, kota)
        .await
        .expect_err("kota doluyken ikinci yükleme reddedilmeli");

    attachment::delete_attachment(&pool, &storage, ilk.id, actor_id)
        .await
        .expect("ek silinebilmeli");

    assert_eq!(
        attachment::total_storage_bytes(&pool, actor_id)
            .await
            .expect("sorgulanabilmeli"),
        0,
        "silinen ek kotadan (SUM(byte_size)) otomatik düşmeli"
    );

    // Kota gerçekten boşaldı: aynı boyutta üçüncü bir yükleme artık kabul
    // ediliyor olmalı.
    let ucuncu =
        attachment::create_attachment(&pool, &storage, &id_codec, actor_id, &png, MAX_UPLOAD, kota)
            .await
            .expect("kota boşaldıktan sonra yeniden yükleme kabul edilmeli");

    // Temizlik.
    attachment::delete_attachment(&pool, &storage, ucuncu.id, actor_id)
        .await
        .expect("temizlik: üçüncü yükleme silinebilmeli");
}
