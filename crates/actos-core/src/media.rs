//! Yüklenen görsellerin doğrulanması ve normalize edilmesi.
//!
//! Bu modül **ağ ve veritabanı bilmiyor**: girdisi ham baytlar, çıktısı
//! işlenmiş baytlar. Depolamaya yükleme ve `attachments` satırını yazma
//! [`crate::attachment`]'ın işi.
//!
//! ## Doğrulama sırası ve neden bu sırada
//!
//! 1. **Boyut** — en ucuz kontrol, en başta. Kod çözücüye hiç girmeden
//!    reddediyoruz.
//! 2. **Magic byte** ([`infer`]) — dosyanın gerçek biçimi. Uzantıya ve
//!    `Content-Type` header'ına **asla** güvenilmiyor: ikisi de istemcinin
//!    yazdığı metin. `.jpg` uzantılı bir HTML dosyası ya da `image/png`
//!    diyen bir ZIP burada durur.
//! 3. **Allowlist** — yalnızca [`ALLOWED_MIME_TYPES`]. Denylist değil:
//!    "şunlar yasak" listesi her yeni biçimde eksik kalır.
//! 4. **Boyut sınırı (piksel)** — kod çözmeden ÖNCE, yalnızca başlıktan
//!    okunan genişlik/yükseklik ile. Sıkıştırma bombası koruması bu:
//!    50 KB'lık bir PNG, 50 000 × 50 000 piksel (≈10 GB ham veri) olduğunu
//!    iddia edebilir. Baytların küçük olması kod çözüldüğünde de küçük
//!    olacağı anlamına gelmiyor.
//! 5. **Kod çözme** — `image::Limits` ile ayrıca ayrılabilecek bellek de
//!    sınırlı; başlıktaki boyutlar yalan söylüyorsa ikinci savunma hattı.
//!
//! ## EXIF neden ayrıca silinmiyor
//!
//! Silmeye gerek yok: görsel ham piksellere kod çözülüp **sıfırdan** WebP
//! olarak yeniden kodlanıyor. EXIF, XMP, ICC ve gömülü küçük resimler dahil
//! bütün metadata bu adımda düşüyor — çünkü çıktı kaynak dosyanın hiçbir
//! baytını taşımıyor. Konum bilgisi taşıyan bir fotoğrafın koordinatları
//! böylece asla depolamaya ulaşmıyor.

use image::{DynamicImage, ImageFormat, ImageReader, Limits, imageops::FilterType};

use crate::error::{Error, Result};

/// Kabul edilen MIME tipleri. Uzantı değil, magic byte'tan tespit edilen
/// gerçek biçim bunlarla karşılaştırılıyor.
pub const ALLOWED_MIME_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/webp", "image/gif"];

/// Normalize edilmiş görselin azami kenar uzunluğu. Daha büyükler oranı
/// korunarak küçültülüyor.
pub const MAX_DIMENSION: u32 = 2048;

/// Küçük önizleme görselinin azami kenar uzunluğu.
pub const THUMBNAIL_DIMENSION: u32 = 320;

/// Kod çözmeden önce kabul edilen azami piksel sayısı (genişlik × yükseklik).
///
/// 50 megapiksel: 8K bir fotoğraf (~33 MP) geçer, sıkıştırma bombası geçmez.
/// Ham RGBA olarak yaklaşık 200 MB'a karşılık geliyor — [`decode_limits`]
/// ayrıca bellek tavanı koyuyor.
pub const MAX_PIXELS: u64 = 50_000_000;

/// İşlenmiş görsel: normalize edilmiş asıl dosya + küçük önizleme.
#[derive(Debug, Clone)]
pub struct ProcessedImage {
    /// WebP baytları.
    pub data: Vec<u8>,
    /// WebP baytları, [`THUMBNAIL_DIMENSION`] sınırında.
    pub thumbnail: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl ProcessedImage {
    /// Çıktının MIME tipi — normalize sonrası her zaman WebP.
    #[must_use]
    pub const fn mime_type() -> &'static str {
        "image/webp"
    }
}

/// Kod çözücüye uygulanan bellek/boyut tavanları.
///
/// [`MAX_PIXELS`] kontrolü başlıktaki *iddia edilen* boyutu denetliyor;
/// bu limitler ise kod çözücünün gerçekten ayırabileceği belleği
/// sınırlıyor. İkisi ayrı savunma: bozuk ya da kötü niyetli bir başlık
/// birinciyi atlatsa bile ikincisi çalışıyor.
fn decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION * 8);
    limits.max_image_height = Some(MAX_DIMENSION * 8);
    // 256 MiB: MAX_PIXELS'in ham RGBA karşılığının biraz üstünde.
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

/// Ham baytları doğrulayıp normalize edilmiş WebP'ye çevirir.
///
/// `max_bytes` çağırandan geliyor (yapılandırmadan okunan yükleme sınırı) —
/// bu modül yapılandırma bilmiyor.
///
/// # Errors
/// Dosya boşsa, `max_bytes`'ı aşıyorsa, biçimi tanınmıyorsa, allowlist'te
/// değilse, [`MAX_PIXELS`]'i aşıyorsa ya da kod çözülemiyorsa
/// [`Error::UnsupportedMedia`] veya [`Error::Validation`].
pub fn process_image(bytes: &[u8], max_bytes: usize) -> Result<ProcessedImage> {
    if bytes.is_empty() {
        return Err(Error::Validation("dosya boş".to_owned()));
    }
    if bytes.len() > max_bytes {
        return Err(Error::Validation(format!(
            "dosya çok büyük: {} bayt, sınır {max_bytes} bayt",
            bytes.len()
        )));
    }

    let mime = detect_mime(bytes)?;

    // İki ayrı okuyucu kuruluyor: `ImageReader` `Clone` değil ve
    // `into_dimensions` okuyucuyu tüketiyor. Baytlar zaten bellekte olduğu
    // için ikinci okuyucuyu kurmanın maliyeti yok.
    let boyut_okuyucu = yeni_okuyucu(bytes)?;

    // Kod çözmeden ÖNCE boyut kontrolü: sıkıştırma bombası koruması burada.
    // `into_dimensions` yalnızca başlığı okuyor, piksel verisine hiç
    // dokunmuyor.
    let (genislik, yukseklik) = boyut_okuyucu
        .into_dimensions()
        .map_err(|_| Error::UnsupportedMedia("görsel boyutları okunamadı".to_owned()))?;

    let piksel = u64::from(genislik) * u64::from(yukseklik);
    if piksel > MAX_PIXELS {
        return Err(Error::Validation(format!(
            "görsel çok büyük: {genislik}x{yukseklik} = {piksel} piksel, sınır {MAX_PIXELS}"
        )));
    }

    let mut kod_okuyucu = yeni_okuyucu(bytes)?;
    kod_okuyucu.limits(decode_limits());

    let gorsel = kod_okuyucu
        .decode()
        .map_err(|_| Error::UnsupportedMedia(format!("{mime} dosyası çözülemedi")))?;

    let normalize = kucult(&gorsel, MAX_DIMENSION);
    let onizleme = kucult(&normalize, THUMBNAIL_DIMENSION);

    let width = normalize.width();
    let height = normalize.height();

    Ok(ProcessedImage {
        data: webp_kodla(&normalize)?,
        thumbnail: webp_kodla(&onizleme)?,
        width,
        height,
    })
}

/// Bayt dilimi üzerinde biçimi tahmin edilmiş bir okuyucu kurar.
///
/// # Errors
/// Biçim tahmin edilemezse [`Error::UnsupportedMedia`].
fn yeni_okuyucu(bytes: &[u8]) -> Result<ImageReader<std::io::Cursor<&[u8]>>> {
    ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| Error::UnsupportedMedia("dosya biçimi okunamadı".to_owned()))
}

/// Magic byte'lardan gerçek MIME tipini tespit eder ve allowlist'e bakar.
///
/// # Errors
/// Biçim tanınmıyorsa ya da allowlist'te değilse
/// [`Error::UnsupportedMedia`].
fn detect_mime(bytes: &[u8]) -> Result<&'static str> {
    let Some(kind) = infer::get(bytes) else {
        return Err(Error::UnsupportedMedia(
            "dosya biçimi tanınmadı (magic byte eşleşmesi yok)".to_owned(),
        ));
    };

    ALLOWED_MIME_TYPES
        .iter()
        .find(|izinli| **izinli == kind.mime_type())
        .copied()
        .ok_or_else(|| {
            Error::UnsupportedMedia(format!(
                "desteklenmeyen biçim: {} (izin verilenler: {})",
                kind.mime_type(),
                ALLOWED_MIME_TYPES.join(", ")
            ))
        })
}

/// Görseli oranı koruyarak `azami` kenar uzunluğuna küçültür.
///
/// Zaten küçükse **büyütmüyor**: yükleneni olduğundan büyük göstermek hem
/// kaliteyi düşürür hem dosyayı şişirir.
fn kucult(gorsel: &DynamicImage, azami: u32) -> DynamicImage {
    if gorsel.width() <= azami && gorsel.height() <= azami {
        return gorsel.clone();
    }
    // `Lanczos3`: küçültmede en iyi kaliteyi veren filtre. Yükleme yolu
    // zaten hız değil doğruluk odaklı (dosya başına bir kez çalışıyor).
    gorsel.resize(azami, azami, FilterType::Lanczos3)
}

/// Görseli WebP olarak kodlar.
///
/// # Errors
/// Kodlama başarısız olursa [`Error::Internal`] — bu bir sunucu hatası,
/// istemcinin girdisiyle ilgili değil: girdi zaten başarıyla çözülmüştü.
fn webp_kodla(gorsel: &DynamicImage) -> Result<Vec<u8>> {
    let mut cikti = std::io::Cursor::new(Vec::new());
    gorsel
        .write_to(&mut cikti, ImageFormat::WebP)
        .map_err(|e| Error::Internal(format!("webp kodlaması başarısız: {e}")))?;
    Ok(cikti.into_inner())
}
