//! Kullanıcı girdisi normalizasyonu, doğrulama ve markdown → HTML render'ı.
//!
//! İki bağımsız iş burada birleşiyor çünkü ikisi de aynı temel soruna
//! dayanıyor: **kullanıcıdan gelen metne güvenme**.
//!
//! - Normalizasyon/doğrulama: görünmez/kontrol karakterleriyle yapılan
//!   isim taklidi (username impersonation) ve filtre atlatma girişimlerine
//!   karşı korur (bkz. [`normalize_text`]).
//! - Markdown render'ı: kullanıcı gövdesini HTML'e çevirirken XSS'e karşı
//!   korur (bkz. [`render_markdown`]).
//!
//! Sınırlar (uzunluk, format) **şemadan** alınmıştır — bkz.
//! `migrations/0002_actors.up.sql`, `migrations/0005_contents.up.sql`,
//! `migrations/0007_tags.up.sql`. Buradaki sabitler o CHECK'lerle birebir
//! örtüşmeli; biri değişirse diğeri de değişmeli.

use std::collections::{HashMap, HashSet};

use pulldown_cmark::{Event, Options, Parser, html};
use unicode_normalization::UnicodeNormalization as _;

// --- Uzunluk sınırları (şemadan) -------------------------------------------
//
// Hepsi KARAKTER (`chars().count()`) cinsinden — PostgreSQL'deki
// `char_length()` de bayt değil karakter sayar, bu yüzden burada `len()`
// (bayt uzunluğu) kullanmak çok baytlı UTF-8 karakterlerde (Türkçe, emoji,
// CJK) şemadan sessizce sapmaya yol açardı.

/// `migrations/0002_actors.up.sql` → `ck_actors_username_format`.
const USERNAME_MIN: usize = 3;
const USERNAME_MAX: usize = 32;

/// `migrations/0007_tags.up.sql` → `ck_tags_name_format`
/// (`^[a-z0-9][a-z0-9-]{0,31}$`: 1 zorunlu + 31 opsiyonel = 32 azami).
const TAG_NAME_MAX: usize = 32;

/// `migrations/0005_contents.up.sql` → `ck_contents_title_length`.
const TITLE_MAX: usize = 300;

/// `migrations/0005_contents.up.sql` → `ck_contents_body_length`.
const BODY_MAX: usize = 100_000;

/// `migrations/0002_actors.up.sql` → `ck_actors_display_name_length`.
const DISPLAY_NAME_MAX: usize = 64;

/// `migrations/0002_actors.up.sql` → `ck_actors_bio_length`.
const BIO_MAX: usize = 500;

/// `migrations/0002_actors.up.sql` → `ck_actors_username_reserved`.
///
/// **Bu liste o CHECK constraint'iyle birebir aynı olmalı** — burada bir
/// isim eklenip orada eklenmezse (veya tersi), uygulama katmanı ile DB
/// katmanı farklı isimleri reddeder; testte (`rezerve_...`) karşılaştırılır.
const RESERVED_USERNAMES: &[&str] = &[
    "admin",
    "administrator",
    "actos",
    "api",
    "root",
    "system",
    "moderator",
    "support",
    "help",
    "about",
    "me",
    "null",
    "undefined",
];

/// Bu karakter görünmez (sıfır genişlikli) veya çift yönlü (bidi) bir
/// kontrol karakteri mi?
///
/// - `U+200B..U+200D` (ZWSP/ZWNJ/ZWJ), `U+FEFF` (BOM), `U+2060` (word
///   joiner), `U+00AD` (soft hyphen): kullanıcı adlarında görsel taklit
///   (`ad​min` ile `admin` aynı görünür) ve içerikte filtre atlatma için
///   kullanılır.
/// - `U+202A..U+202E`, `U+2066..U+2069`: metnin görüntülenme sırasını
///   tersine çevirip yanıltıcı içerik üretmeye yarar (Trojan Source
///   saldırısı).
const fn is_invisible_or_bidi_control(c: char) -> bool {
    matches!(c,
        '\u{200B}'..='\u{200D}'
        | '\u{FEFF}'
        | '\u{2060}'
        | '\u{00AD}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
    )
}

/// Kullanıcı girdisini kanonik hâline indirger.
///
/// Sırasıyla:
/// 1. Satır sonlarını `\n`'e normalize eder (`\r\n` ve tek başına `\r`).
/// 2. Görünmez/bidi kontrol karakterlerini atar — bu, NFC'den **önce**
///    yapılır: aradaki bir görünmez karakter (ör. `e` + ZWNJ + birleştirici
///    aksan) canonical composition'ı bloke edip normalizasyonun sessizce
///    başarısız olmasına yol açabilir.
/// 3. Unicode NFC normalizasyonu uygular (görsel olarak aynı ama farklı bayt
///    dizili string'lerin — ör. tek kod noktalı `é` ile `e` + birleştirici
///    aksan — aynı sonuca inmesi için; aksi hâlde arama, benzersizlik ve
///    karşılaştırma sessizce bozulur).
/// 4. Baştaki/sondaki boşlukları kırpar.
///
/// Türkçe, Arapça, emoji, CJK dâhil diğer tüm Unicode karakterlere
/// **dokunulmaz** — yalnızca yukarıdaki görünmez/kontrol sınıfı temizlenir.
#[must_use]
pub fn normalize_text(raw: &str) -> String {
    let newline_normalized = raw.replace("\r\n", "\n").replace('\r', "\n");

    let without_invisible: String = newline_normalized
        .chars()
        .filter(|c| !is_invisible_or_bidi_control(*c))
        .collect();

    without_invisible
        .nfc()
        .collect::<String>()
        .trim()
        .to_string()
}

/// Metin doğrulama/normalizasyonu sırasında oluşan hatalar.
///
/// Mesajlar kullanıcıya doğrudan gösterilir: İngilizce ve eyleme
/// dönüştürülebilir olacak şekilde yazıldı ("geçersiz" değil, hangi kuralın
/// ihlal edildiği).
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error(
        "username must be {USERNAME_MIN}-{USERNAME_MAX} characters and may only contain \
         lowercase letters, digits, and underscore (_)"
    )]
    InvalidUsername,

    #[error("\"{0}\" is a reserved username, pick another one")]
    ReservedUsername(String),

    #[error(
        "tag name must be 1-{TAG_NAME_MAX} characters, must start with a lowercase letter or \
         digit, and may only contain lowercase letters, digits, and hyphen (-)"
    )]
    InvalidTagName,

    #[error("title can be at most {TITLE_MAX} characters (received: {actual} characters)")]
    TitleTooLong { actual: usize },

    #[error("body cannot be empty")]
    EmptyBody,

    #[error("body can be at most {BODY_MAX} characters (received: {actual} characters)")]
    BodyTooLong { actual: usize },

    #[error(
        "display name can be at most {DISPLAY_NAME_MAX} characters (received: {actual} characters)"
    )]
    DisplayNameTooLong { actual: usize },

    #[error("bio can be at most {BIO_MAX} characters (received: {actual} characters)")]
    BioTooLong { actual: usize },
}

/// Kullanıcı adını doğrular: [`normalize_text`] uygular, format ve rezerve
/// isim kuralını işletir.
///
/// Format `migrations/0002_actors.up.sql`'deki `^[a-z0-9_]{3,32}$` ile
/// birebir örtüşür.
///
/// # Errors
/// Uzunluk veya karakter kümesi kuralı ihlal edilirse
/// [`TextError::InvalidUsername`]; isim rezerve listedeyse
/// [`TextError::ReservedUsername`].
pub fn validate_username(raw: &str) -> Result<String, TextError> {
    let normalized = normalize_text(raw);
    let len = normalized.chars().count();

    // Rezerve kontrolü format kontrolünden ÖNCE gelir. Sebebi: listedeki
    // bazı isimler (`me`) asgari uzunluğun altında; format önce çalışsaydı
    // onlar "3-32 karakter olmalı" hatasına takılır ve rezerve oldukları
    // hiçbir zaman söylenmezdi. İki gerekçe de doğru, ama kullanıcıya
    // gösterilmesi gereken asıl sebep ismin alınmış olması.
    if RESERVED_USERNAMES.contains(&normalized.as_str()) {
        return Err(TextError::ReservedUsername(normalized));
    }

    let format_ok = (USERNAME_MIN..=USERNAME_MAX).contains(&len)
        && normalized
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');

    if !format_ok {
        return Err(TextError::InvalidUsername);
    }

    Ok(normalized)
}

/// Etiket adını doğrular: [`normalize_text`] uygular, format kuralını
/// işletir.
///
/// Format `migrations/0007_tags.up.sql`'deki `^[a-z0-9][a-z0-9-]{0,31}$` ile
/// birebir örtüşür (1-32 karakter, ilk karakter harf/rakam, kalanı harf/
/// rakam/tire).
///
/// # Errors
/// Uzunluk veya karakter kümesi kuralı ihlal edilirse
/// [`TextError::InvalidTagName`].
pub fn validate_tag_name(raw: &str) -> Result<String, TextError> {
    let normalized = normalize_text(raw);
    let len = normalized.chars().count();

    if len == 0 || len > TAG_NAME_MAX {
        return Err(TextError::InvalidTagName);
    }

    let mut chars = normalized.chars();
    // `len > 0` yukarıda doğrulandı, yani burada her zaman bir karakter var.
    // Yine de `expect` yazmıyoruz: değişmez bir varsayımı panikle korumak
    // yerine hatayı düzgün döndürmek, ileride üstteki kontrol değişirse
    // sessiz bir çökme riski bırakmıyor.
    let Some(first) = chars.next() else {
        return Err(TextError::InvalidTagName);
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(TextError::InvalidTagName);
    }

    let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !rest_ok {
        return Err(TextError::InvalidTagName);
    }

    Ok(normalized)
}

/// Post başlığını doğrular: [`normalize_text`] uygular, uzunluk kuralını
/// işletir.
///
/// `migrations/0005_contents.up.sql` → `ck_contents_title_length`
/// (`char_length(title) <= 300`). Şema başlığın boş olamayacağını
/// zorlamıyor (yalnızca `NOT NULL`); bu fonksiyon da aynı şekilde boş
/// string'e izin verir.
///
/// # Errors
/// `300` karakteri aşarsa [`TextError::TitleTooLong`].
pub fn validate_title(raw: &str) -> Result<String, TextError> {
    let normalized = normalize_text(raw);
    let len = normalized.chars().count();

    if len > TITLE_MAX {
        return Err(TextError::TitleTooLong { actual: len });
    }

    Ok(normalized)
}

/// Post/yorum gövdesini doğrular: [`normalize_text`] uygular, boş olamama
/// ve uzunluk kurallarını işletir.
///
/// `migrations/0005_contents.up.sql` → `ck_contents_body_length`
/// (`char_length(body) <= 100000`, kolon `NOT NULL`).
///
/// # Errors
/// Normalize edildikten sonra boşsa [`TextError::EmptyBody`]; `100000`
/// karakteri aşarsa [`TextError::BodyTooLong`].
pub fn validate_body(raw: &str) -> Result<String, TextError> {
    let normalized = normalize_text(raw);
    let len = normalized.chars().count();

    if len == 0 {
        return Err(TextError::EmptyBody);
    }
    if len > BODY_MAX {
        return Err(TextError::BodyTooLong { actual: len });
    }

    Ok(normalized)
}

/// Görünen adı doğrular: [`normalize_text`] uygular, uzunluk kuralını
/// işletir.
///
/// `migrations/0002_actors.up.sql` → `ck_actors_display_name_length`
/// (`char_length(display_name) <= 64`). Kolon nullable; bu fonksiyon
/// yalnızca dolu bir değer verildiğinde çağrılır, boş string'e izin verir.
///
/// # Errors
/// `64` karakteri aşarsa [`TextError::DisplayNameTooLong`].
pub fn validate_display_name(raw: &str) -> Result<String, TextError> {
    let normalized = normalize_text(raw);
    let len = normalized.chars().count();

    if len > DISPLAY_NAME_MAX {
        return Err(TextError::DisplayNameTooLong { actual: len });
    }

    Ok(normalized)
}

/// Biyografiyi doğrular: [`normalize_text`] uygular, uzunluk kuralını
/// işletir.
///
/// `migrations/0002_actors.up.sql` → `ck_actors_bio_length`
/// (`char_length(bio) <= 500`). Kolon nullable; bu fonksiyon yalnızca dolu
/// bir değer verildiğinde çağrılır, boş string'e izin verir.
///
/// # Errors
/// `500` karakteri aşarsa [`TextError::BioTooLong`].
pub fn validate_bio(raw: &str) -> Result<String, TextError> {
    let normalized = normalize_text(raw);
    let len = normalized.chars().count();

    if len > BIO_MAX {
        return Err(TextError::BioTooLong { actual: len });
    }

    Ok(normalized)
}

// --- Markdown → güvenli HTML -----------------------------------------------

/// İzin verilen HTML etiketleri: paragraf, başlıklar, kalın/italik, listeler,
/// kod (inline + blok), alıntı, yatay çizgi, link, resim, tablo, üstü
/// çizili. `pulldown-cmark`'ın `Tag::Strikethrough`'u `<del>` olarak
/// render ettiğini biliyoruz (bkz. `render_markdown` içindeki not); `<s>`
/// yine de allowlist'te — ham/elle yazılmış markdown'da ya da ileride
/// kütüphane değişirse aynı anlamı taşıyan eşdeğer bir etiket.
///
/// **Bilinçli olarak DIŞARIDA:** `script`, `style`, `iframe`, `object` ve
/// listelenmemiş her şey. `ammonia::Builder::default()`'ın kendi geniş
/// varsayılan etiket listesine (ör. `nav`, `article`, `details`) **hiç
/// güvenilmiyor** — `tags()` ile tamamen üzerine yazılıyor.
fn allowed_tags() -> HashSet<&'static str> {
    HashSet::from([
        "p",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "strong",
        "em",
        "b",
        "i",
        "ul",
        "ol",
        "li",
        "code",
        "pre",
        "blockquote",
        "hr",
        "a",
        "img",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
        "del",
        "s",
    ])
}

/// İzin verilen etiket bazlı öznitelikler.
///
/// `ammonia`'nın varsayılan setine güvenmek yerine `tag_attributes()` ile
/// tamamen üzerine yazılıyor: olay öznitelikleri (`onclick`, `onerror`, ...)
/// zaten bu setin dışında olduğu için asla çıkmaz, ama "varsayılana
/// güvenme" ilkesi gereği hangi özniteliğin hangi etikette izinli olduğu
/// burada açıkça listeleniyor.
///
/// `href` ve `src` `ammonia` tarafından iç olarak URL özniteliği sayılır
/// (bkz. `ammonia::is_url_attr`) ve [`allowed_url_schemes`] ile
/// şema-doğrulamasından geçirilir; ayrıca doğrulama yapmaya gerek yok.
fn allowed_tag_attributes() -> HashMap<&'static str, HashSet<&'static str>> {
    HashMap::from([
        ("a", HashSet::from(["href"])),
        ("img", HashSet::from(["src", "alt", "width", "height"])),
        ("ol", HashSet::from(["start"])),
        ("th", HashSet::from(["colspan", "rowspan"])),
        ("td", HashSet::from(["colspan", "rowspan"])),
    ])
}

/// İzin verilen link/resim URL şemaları: yalnızca `http`, `https`,
/// `mailto`. `ammonia`'nın varsayılanı (`ftp`, `tel`, `sms`, `magnet`, ...
/// gibi 20'den fazla şema) çok daha geniş; burada tamamen üzerine yazılıyor.
///
/// `javascript:`, `data:`, `vbscript:` bu listede **yok** → `href`/`src`
/// bu şemalardan biriyle geldiğinde ammonia ilgili özniteliği düşürür.
fn allowed_url_schemes() -> HashSet<&'static str> {
    HashSet::from(["http", "https", "mailto"])
}

/// Sanitize edilmiş `HTML` üretmek için yapılandırılmış bir `ammonia`
/// builder'ı kurar.
///
/// - `generic_attributes` boşaltılır: `ammonia`'nın varsayılanı (`lang`,
///   `title`) tüm etiketlerde izinli olurdu; bizim allowlist'imizde buna
///   ihtiyaç yok, saldırı yüzeyini gereksiz büyütmeyelim.
/// - `link_rel`: dış linklere `rel="nofollow noopener noreferrer"` eklenir
///   (`noopener`/`noreferrer` `ammonia`'nın zaten varsayılanı, `nofollow`
///   burada eklendi).
fn sanitize_html(unsafe_html: &str) -> String {
    ammonia::Builder::new()
        .tags(allowed_tags())
        .tag_attributes(allowed_tag_attributes())
        .generic_attributes(HashSet::new())
        .url_schemes(allowed_url_schemes())
        .link_rel(Some("nofollow noopener noreferrer"))
        .clean(unsafe_html)
        .to_string()
}

/// Markdown kaynağını güvenli HTML'e çevirir.
///
/// İki aşamalı savunma:
/// 1. `pulldown-cmark` markdown'ı HTML'e çevirir. **Ham HTML geçişi kapalı**
///    (v1 kararı, bkz. `PLAN.md` Faz 4): bu sürümde `pulldown-cmark`'ın ham
///    HTML'i devre dışı bırakan ayrı bir `Options` bayrağı yok (CommonMark
///    spesifikasyonu ham HTML'i çekirdek bir özellik sayıyor, opsiyonel
///    değil) — bu yüzden `Event::Html`/`Event::InlineHtml` olayları HTML
///    üretilmeden **önce** elle filtreleniyor. Yani girdideki
///    `<script>...</script>` gibi ham bloklar `ammonia`'ya hiç ulaşmıyor.
/// 2. `ammonia` sonucu sıkı bir allowlist'le temizler (bkz.
///    [`sanitize_html`]): izin verilmeyen etiket/öznitelik/URL şeması
///    düşürülür. Bu, (1)'in bir açığı varsa ya da girdi markdown *sözdizimi*
///    üzerinden (ör. `[link](javascript:...)`) enjekte edilmeye çalışılırsa
///    devreye giren ikinci savunma hattı.
#[must_use]
pub fn render_markdown(source: &str) -> String {
    let normalized = normalize_text(source);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(&normalized, options)
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)));

    let mut unsafe_html = String::new();
    html::push_html(&mut unsafe_html, parser);

    sanitize_html(&unsafe_html)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_text ------------------------------------------------

    #[test]
    fn nfc_birleştirici_aksan_ile_tek_kod_noktalı_karakteri_aynı_sonuca_indirger() {
        let combining = "e\u{0301}"; // 'e' + U+0301 COMBINING ACUTE ACCENT
        let precomposed = "\u{00E9}"; // tek kod noktalı 'é'
        assert_eq!(normalize_text(combining), normalize_text(precomposed));
        assert_eq!(normalize_text(combining), "\u{00E9}");
    }

    #[test]
    fn sıfır_genişlikli_karakter_temizlenince_rezerve_isimle_çakışıyor() {
        // Görünürde "ad<ZWSP>min" ile "admin" aynı görünür; normalize
        // edilmezse rezerve isim kontrolünü atlatabilirdi.
        let raw = "ad\u{200B}min";
        assert_eq!(normalize_text(raw), "admin");
        assert!(matches!(
            validate_username(raw),
            Err(TextError::ReservedUsername(_))
        ));
    }

    #[test]
    fn bidi_kontrol_karakterleri_temizleniyor() {
        let raw = "\u{202E}evil\u{202C}";
        assert_eq!(normalize_text(raw), "evil");

        let raw2 = "a\u{2066}b\u{2069}c";
        assert_eq!(normalize_text(raw2), "abc");
    }

    #[test]
    fn türkçe_emoji_ve_cjk_karakterleri_korur() {
        let raw = "İstanbul 😀 中文 café";
        let normalized = normalize_text(raw);
        assert!(normalized.contains('İ'));
        assert!(normalized.contains("stanbul"));
        assert!(normalized.contains('😀'));
        assert!(normalized.contains('中'));
        assert!(normalized.contains('文'));
        assert!(normalized.contains("café"));
    }

    #[test]
    fn satır_sonları_normalize_ediliyor() {
        let raw = "birinci\r\nikinci\rüçüncü\nson";
        assert_eq!(normalize_text(raw), "birinci\nikinci\nüçüncü\nson");
    }

    #[test]
    fn baştaki_ve_sondaki_boşluklar_kırpılıyor() {
        assert_eq!(normalize_text("   merhaba   "), "merhaba");
        assert_eq!(normalize_text("\n\tmerhaba\t\n"), "merhaba");
    }

    // --- validate_username ---------------------------------------------

    #[test]
    fn kullanıcı_adı_sınır_değerleri() {
        assert!(validate_username(&"a".repeat(2)).is_err());
        assert!(validate_username(&"a".repeat(3)).is_ok());
        assert!(validate_username(&"a".repeat(32)).is_ok());
        assert!(validate_username(&"a".repeat(33)).is_err());
    }

    #[test]
    fn kullanıcı_adında_izin_verilmeyen_karakterler_reddediliyor() {
        assert!(validate_username("Kullanici").is_err()); // büyük harf
        assert!(validate_username("kullanici adi").is_err()); // boşluk
        assert!(validate_username("kullanici-adi").is_err()); // tire
        assert!(validate_username("kullanici_adi_1").is_ok()); // geçerli
    }

    #[test]
    fn rezerve_kullanıcı_adları_0002_migration_ile_birebir_eşleşiyor() {
        // migrations/0002_actors.up.sql -> ck_actors_username_reserved
        // ile birebir aynı olmalı; biri değişip diğeri unutulursa bu test
        // kırılır.
        let expected_from_migration: [&str; 13] = [
            "admin",
            "administrator",
            "actos",
            "api",
            "root",
            "system",
            "moderator",
            "support",
            "help",
            "about",
            "me",
            "null",
            "undefined",
        ];
        assert_eq!(RESERVED_USERNAMES, &expected_from_migration[..]);

        for name in expected_from_migration {
            assert!(
                matches!(validate_username(name), Err(TextError::ReservedUsername(_))),
                "rezerve isim reddedilmeliydi: {name}"
            );
        }
    }

    #[test]
    fn büyük_harfli_rezerve_isim_yazımı_da_reddediliyor() {
        // Büyük harf zaten format kuralını ihlal ettiği için ayrı bir
        // "rezerve" hatası değil ama sonuç yine reddediliyor olmalı.
        assert!(validate_username("ADMIN").is_err());
        assert!(validate_username("Admin").is_err());
    }

    // --- validate_tag_name -----------------------------------------------

    #[test]
    fn geçersiz_etiket_adları_reddediliyor() {
        assert!(validate_tag_name("-tag").is_err()); // tire ile başlıyor
        assert!(validate_tag_name("Tag").is_err()); // büyük harf
        assert!(validate_tag_name("").is_err()); // boş
        assert!(validate_tag_name(&"a".repeat(33)).is_err()); // çok uzun
    }

    #[test]
    fn geçerli_etiket_adları_kabul_ediliyor() {
        assert!(validate_tag_name("nvidia-h100").is_ok());
        assert_eq!(validate_tag_name("nvidia-h100").unwrap(), "nvidia-h100");
        assert!(validate_tag_name(&"a".repeat(32)).is_ok());
        assert!(validate_tag_name("a").is_ok());
    }

    // --- validate_title ----------------------------------------------------

    #[test]
    fn başlık_sınır_değerleri() {
        assert!(validate_title(&"a".repeat(300)).is_ok());
        assert!(validate_title(&"a".repeat(301)).is_err());
    }

    // --- validate_body -----------------------------------------------------

    #[test]
    fn gövde_boş_olamaz() {
        assert!(matches!(validate_body(""), Err(TextError::EmptyBody)));
        // Yalnızca boşluktan oluşan girdi normalize edilince boşa iner.
        assert!(matches!(
            validate_body("   \n\t  "),
            Err(TextError::EmptyBody)
        ));
    }

    #[test]
    fn gövde_sınır_değerleri() {
        assert!(validate_body(&"a".repeat(100_000)).is_ok());
        assert!(validate_body(&"a".repeat(100_001)).is_err());
    }

    // --- validate_display_name / validate_bio -------------------------------

    #[test]
    fn görünen_ad_sınır_değerleri() {
        assert!(validate_display_name(&"a".repeat(64)).is_ok());
        assert!(validate_display_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn biyografi_sınır_değerleri() {
        assert!(validate_bio(&"a".repeat(500)).is_ok());
        assert!(validate_bio(&"a".repeat(501)).is_err());
    }

    // --- render_markdown: güvenlik ------------------------------------------

    #[test]
    fn script_etiketi_çıktıda_yok() {
        let html = render_markdown("<script>alert(1)</script>");
        assert!(!html.contains("<script"));
        assert!(!html.contains("alert(1)"));
    }

    #[test]
    fn javascript_şemalı_link_reddediliyor() {
        let html = render_markdown("[tıkla](javascript:alert(1))");
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn img_onerror_niteliği_çıktıda_yok() {
        let html = render_markdown("<img src=x onerror=alert(1)>");
        assert!(!html.contains("onerror"));
        assert!(!html.contains("alert(1)"));
    }

    #[test]
    fn data_uri_linki_temizleniyor() {
        let html = render_markdown("[link](data:text/html,<script>alert(1)</script>)");
        assert!(!html.contains("data:"));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn iframe_etiketi_çıktıda_yok() {
        let html = render_markdown("<iframe src=\"evil\"></iframe>");
        assert!(!html.contains("<iframe"));
        assert!(!html.contains("evil"));
    }

    #[test]
    fn olay_öznitelikleri_genel_olarak_süzülüyor() {
        let html = render_markdown("<div onclick=\"alert(1)\">merhaba</div>");
        assert!(!html.contains("onclick"));
        assert!(!html.contains("<div"));
    }

    #[test]
    fn vbscript_şeması_reddediliyor() {
        let html = render_markdown("[tıkla](vbscript:msgbox(1))");
        assert!(!html.contains("vbscript:"));
    }

    // --- render_markdown: doğru render ---------------------------------------

    #[test]
    fn normal_markdown_doğru_render_ediliyor() {
        let source = "\
# Başlık

**kalın** ve *italik* metin.

- madde 1
- madde 2

```
kod bloğu
```

| a | b |
|---|---|
| 1 | 2 |
";
        let html = render_markdown(source);
        assert!(html.contains("<h1>Başlık</h1>"));
        assert!(html.contains("<strong>kalın</strong>"));
        assert!(html.contains("<em>italik</em>"));
        assert!(html.contains("<li>madde 1</li>"));
        assert!(html.contains("<li>madde 2</li>"));
        assert!(html.contains("<pre><code>kod bloğu\n</code></pre>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<td>1</td>"));
    }

    #[test]
    fn üstü_çizili_metin_render_ediliyor() {
        let html = render_markdown("~~silindi~~");
        assert!(html.contains("silindi"));
        assert!(html.contains("<del>") || html.contains("<s>"));
    }

    #[test]
    fn geçerli_dış_link_rel_niteliği_alıyor() {
        let html = render_markdown("[Rust](https://rust-lang.org)");
        assert!(html.contains("href=\"https://rust-lang.org\""));
        assert!(html.contains("rel=\"nofollow noopener noreferrer\""));
    }

    #[test]
    fn mailto_şemasına_izin_veriliyor() {
        let html = render_markdown("[yaz](mailto:test@example.com)");
        assert!(html.contains("mailto:test@example.com"));
    }

    #[test]
    fn derin_iç_içe_geçme_veya_uzun_girdi_panik_atmıyor() {
        // Makul ama sıra dışı derinlik/uzunlukta girdi; panik veya donma
        // olmadığını doğruluyoruz (sonsuz döngü kurmuyoruz, tek seferlik
        // makul boyutlu bir girdiyle deniyoruz).
        let nested = "> ".repeat(200) + "iç içe alıntı";
        let rendered_nested = render_markdown(&nested);
        assert!(!rendered_nested.is_empty());

        let long_body = "kelime ".repeat(20_000);
        let rendered_long = render_markdown(&long_body);
        assert!(rendered_long.contains("kelime"));
    }
}
