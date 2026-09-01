//! `actos_core::media` testleri — yükleme yolunun güvenlik açısından
//! kritik kısmı.
//!
//! **Neden burada, HTTP testlerinde değil:** doğrulama zinciri (boyut →
//! magic byte → allowlist → piksel sınırı → kod çözme) ne ağ ne veritabanı
//! istiyor. `crates/actos-api` tarafında yükleme ucunu uçtan uca test etmek
//! çalışan bir MinIO gerektirirdi ve test edilen asıl şey yine bu
//! fonksiyon olurdu; HTTP katmanı yalnızca multipart'ı okuyup bunu
//! çağırıyor.
//!
//! Bu dosya veritabanına da dokunmuyor, `#[sqlx::test]` yok.

use actos_core::{Error, media};

/// Yükleme sınırı testlerinde kullanılan üst sınır.
const MAX: usize = 8 * 1024 * 1024;

/// Verilen boyutlarda gerçek bir PNG üretir.
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

/// CRC-32 (PNG chunk'ları için). `crc32fast` bağımlılığı eklememek için
/// tabloya dayanmayan kısa bir uygulama — test başına bir avuç bayt
/// hesaplanıyor, hız önemsiz.
fn crc32(veri: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in veri {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// **Sıkıştırma bombası:** yapısı tamamen geçerli, ama başlığında devasa
/// boyutlar iddia eden bir PNG.
///
/// Gerçek (küçük) bir PNG üretilip yalnızca IHDR'deki genişlik/yükseklik
/// alanları değiştiriliyor ve chunk'ın CRC'si yeniden hesaplanıyor. Yalnızca
/// imza + IHDR yazan bir fikstür işe yaramıyor: `png` kod çözücüsü boyutları
/// döndürmeden önce dosyanın devamını da görmek istiyor, dolayısıyla böyle
/// bir dosya "bozuk" diye reddediliyor ve testin sınamak istediği piksel
/// sınırına hiç gelinmiyor.
///
/// PNG yerleşimi: 8 bayt imza, 4 bayt uzunluk, `IHDR` (12..16), genişlik
/// (16..20), yükseklik (20..24), IHDR verisi 13 bayt (16..29), CRC (29..33).
/// CRC 12..29 aralığı üzerinden hesaplanıyor.
fn sikistirma_bombasi(genislik: u32, yukseklik: u32) -> Vec<u8> {
    let mut png = gercek_png(8, 8);

    png[16..20].copy_from_slice(&genislik.to_be_bytes());
    png[20..24].copy_from_slice(&yukseklik.to_be_bytes());

    let crc = crc32(&png[12..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());

    png
}

// --- Kabul edilen yol -------------------------------------------------------

#[test]
fn gecerli_png_webpe_normalize_ediliyor() {
    let girdi = gercek_png(64, 48);
    let sonuc = media::process_image(&girdi, MAX).expect("geçerli png kabul edilmeli");

    assert_eq!(sonuc.width, 64);
    assert_eq!(sonuc.height, 48);
    assert_eq!(&sonuc.data[0..4], b"RIFF", "çıktı WebP olmalı");
    assert_eq!(&sonuc.data[8..12], b"WEBP");
    assert_eq!(&sonuc.thumbnail[8..12], b"WEBP", "önizleme de WebP olmalı");
}

/// Büyük görseller `MAX_DIMENSION`'a küçültülüyor, oran korunuyor.
#[test]
fn buyuk_gorsel_kucultuluyor_oran_korunuyor() {
    let girdi = gercek_png(4096, 2048);
    let sonuc = media::process_image(&girdi, MAX).expect("kabul edilmeli");

    assert_eq!(sonuc.width, media::MAX_DIMENSION);
    assert_eq!(sonuc.height, media::MAX_DIMENSION / 2, "oran korunmalı");
}

/// Küçük görseller **büyütülmüyor**.
#[test]
fn kucuk_gorsel_buyutulmuyor() {
    let girdi = gercek_png(32, 32);
    let sonuc = media::process_image(&girdi, MAX).expect("kabul edilmeli");
    assert_eq!((sonuc.width, sonuc.height), (32, 32));
}

/// Yeniden kodlama metadata'yı düşürüyor: çıktı, girdinin hiçbir baytını
/// taşımıyor. EXIF'i ayrıca silmeye gerek olmamasının gerekçesi bu.
#[test]
fn cikti_girdinin_baytlarini_tasimiyor() {
    let girdi = gercek_png(40, 40);
    let sonuc = media::process_image(&girdi, MAX).expect("kabul edilmeli");

    // PNG imzası çıktıda hiç geçmemeli — çıktı baştan WebP olarak kuruldu.
    assert!(
        !sonuc
            .data
            .windows(8)
            .any(|w| w == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "çıktı kaynak dosyanın imzasını taşımamalı"
    );
}

// --- Reddedilen yollar ------------------------------------------------------

#[test]
fn bos_dosya_reddediliyor() {
    let hata = media::process_image(&[], MAX).expect_err("boş dosya reddedilmeli");
    assert!(matches!(hata, Error::Validation(_)), "{hata:?}");
}

/// Planın istediği "çok büyük dosya" senaryosu.
#[test]
fn cok_buyuk_dosya_reddediliyor() {
    let girdi = gercek_png(64, 64);
    // Sınırı dosyanın altına çekiyoruz: gerçekten 8 MB üretmeye gerek yok.
    let hata =
        media::process_image(&girdi, girdi.len() - 1).expect_err("sınırı aşan dosya reddedilmeli");
    assert!(matches!(hata, Error::Validation(_)), "{hata:?}");
}

/// Planın istediği "sahte uzantı" senaryosu: içerik görsel değil.
/// Uzantı ve `Content-Type` bu katmana hiç ulaşmıyor zaten — karar
/// yalnızca magic byte'lara bakıyor.
#[test]
fn gorsel_olmayan_icerik_reddediliyor() {
    let hata = media::process_image(b"<html>bu bir gorsel degil</html>", MAX)
        .expect_err("görsel olmayan içerik reddedilmeli");
    assert!(matches!(hata, Error::UnsupportedMedia(_)), "{hata:?}");
}

/// Tanınan ama allowlist'te olmayan bir biçim (PDF) reddedilmeli.
/// Denylist yerine allowlist kullanmanın sınandığı yer burası.
#[test]
fn allowlist_disi_bicim_reddediliyor() {
    // `%PDF-1.4` magic'i + biraz dolgu.
    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.extend_from_slice(&[0u8; 64]);

    let hata = media::process_image(&pdf, MAX).expect_err("pdf reddedilmeli");
    match hata {
        Error::UnsupportedMedia(mesaj) => {
            assert!(
                mesaj.contains("pdf") || mesaj.contains("tanınmadı"),
                "hata mesajı biçimi belirtmeli: {mesaj}"
            );
        }
        diger => panic!("beklenen UnsupportedMedia: {diger:?}"),
    }
}

/// Planın istediği "bozuk dosya" senaryosu: geçerli PNG magic'i ama
/// içeriği çöp.
#[test]
fn bozuk_dosya_reddediliyor() {
    let mut bozuk = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bozuk.extend_from_slice(&[0xFF; 128]);

    let hata = media::process_image(&bozuk, MAX).expect_err("bozuk png reddedilmeli");
    assert!(matches!(hata, Error::UnsupportedMedia(_)), "{hata:?}");
}

/// Planın istediği "zip bomb" (sıkıştırma bombası) senaryosu.
///
/// Dosya birkaç yüz bayt ama 50 000 × 50 000 piksel iddia ediyor — ham RGBA
/// olarak ~10 GB. Kontrol kod çözmeden önce yapıldığı için o bellek hiç
/// ayrılmıyor.
#[test]
fn sikistirma_bombasi_kod_cozmeden_reddediliyor() {
    let bomba = sikistirma_bombasi(50_000, 50_000);
    assert!(
        bomba.len() < 1024,
        "bombanın kendisi küçük olmalı: {} bayt",
        bomba.len()
    );

    let hata = media::process_image(&bomba, MAX).expect_err("sıkıştırma bombası reddedilmeli");

    match hata {
        Error::Validation(mesaj) => {
            assert!(
                mesaj.contains("piksel"),
                "hata piksel sınırını belirtmeli: {mesaj}"
            );
            assert!(
                mesaj.contains("50000x50000"),
                "hata iddia edilen boyutları belirtmeli: {mesaj}"
            );
        }
        diger => panic!("beklenen Validation: {diger:?}"),
    }
}

/// Sınırın hemen üstündeki bir boyut da **piksel** kontrolüne takılmalı —
/// kod çözmeye hiç gelinmemeli.
#[test]
fn piksel_siniri_hemen_ustunde_de_calisiyor() {
    // 8000 x 8000 = 64 milyon piksel, MAX_PIXELS (50M) üstünde.
    let bomba = sikistirma_bombasi(8_000, 8_000);
    let hata = media::process_image(&bomba, MAX).expect_err("sınır üstü reddedilmeli");
    assert!(
        matches!(hata, Error::Validation(ref m) if m.contains("piksel")),
        "{hata:?}"
    );
}

/// Sınırın altındaki bir boyut piksel kontrolünden **geçiyor** — kontrol
/// fazla agresif değil. (Bu fikstürün gövdesi başlığıyla uyuşmadığı için
/// kod çözmede düşüyor; önemli olan hatanın piksel sınırı olmaması.)
#[test]
fn piksel_siniri_altindaki_boyut_kontrolden_geciyor() {
    // 7000 x 7000 = 49 milyon piksel, MAX_PIXELS (50M) altında.
    let bomba = sikistirma_bombasi(7_000, 7_000);
    let hata = media::process_image(&bomba, MAX).expect_err("gövdesi bozuk png çözülemez");

    assert!(
        matches!(hata, Error::UnsupportedMedia(_)),
        "piksel sınırına değil kod çözmeye takılmalı: {hata:?}"
    );
}
