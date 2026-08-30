//! Sır üretme ilkelleri: API key'ler ve kurtarma kodları.
//!
//! İki farklı sır sınıfı, iki farklı hash stratejisi kullanır:
//!
//! - **API key secret'i**: 256 bit, bizim ürettiğimiz rastgele bir değer.
//!   SHA-256 ile hash'lenir (bkz. [`hash_api_secret`] üzerindeki gerekçe).
//! - **Kurtarma kodu**: kısa, insan tarafından yazılabilir, veritabanı
//!   sızarsa çevrimdışı deneme riski taşır. Argon2id ile hash'lenir (bkz.
//!   [`generate_recovery_codes`] üzerindeki gerekçe).

use argon2::{
    Argon2,
    password_hash::{PasswordHasher as _, PasswordVerifier as _, phc::PasswordHash},
};
use rand::RngExt as _;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

/// API key'in string önekindeki sabit kısım. Bilinçli olarak seçildi: sızan
/// key'ler bir kod tabanına veya log'a düştüğünde sır tarayıcıları (gitleaks,
/// trufflehog vb.) bu öneki tanıyıp yakalayabilsin.
const API_KEY_PREFIX: &str = "actos";

/// Secret'in base62'ye kodlanmış hâlinin sabit uzunluğu.
///
/// base62 crate'i yalnızca `u128` (en fazla 16 bayt) kodlar; 32 baytlık
/// secret'i tek parça kodlayamayız. Bu yüzden 16+16 baytlık iki `u128`
/// parçasına bölünür, her biri `'0'` ile 22 karaktere (bir `u128`'in base62
/// içindeki azami genişliği) sola doldurulur ve ardışık yazılır. Sabit
/// genişlik sayesinde ayrıştırma sırasında bir ayraca ihtiyaç duymadan
/// ortadan bölünebilir.
const SECRET_CHUNK_WIDTH: usize = 22;
const SECRET_ENCODED_LEN: usize = SECRET_CHUNK_WIDTH * 2;

/// Sır üretme/ayrıştırma sırasında oluşan hatalar.
///
/// Hiçbiri panik değildir: kullanıcıdan veya ağdan gelen bozuk bir sır,
/// bu tiplerden biriyle geri döner.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("API key önekini tanımıyorum (\"actos_\" bekleniyordu)")]
    InvalidPrefix,

    #[error("API key üç parçadan oluşmalı: actos_<key_id>_<secret>")]
    MalformedStructure,

    #[error("key_id base62 olarak çözülemedi: {0}")]
    InvalidKeyIdEncoding(String),

    #[error("secret base62 olarak çözülemedi: {0}")]
    InvalidSecretEncoding(String),

    #[error("secret uzunluğu geçersiz (kodlanmış hâli {SECRET_ENCODED_LEN} karakter olmalı)")]
    InvalidSecretLength,

    #[error("Argon2 hash'leme başarısız: {0}")]
    Hashing(String),
}

/// [`generate_api_key`] çağrısının sonucu.
pub struct GeneratedApiKey {
    /// `api_keys.id` olarak saklanacak değer; aynı zamanda `plaintext`
    /// içine base62 kodlanmış hâliyle gömülüdür.
    pub key_id: Uuid,
    /// `api_keys.secret_hash` kolonuna giden değer.
    pub secret_hash: String,
    /// Kullanıcıya **bir kez** gösterilecek ham key. Saklanmaz.
    pub plaintext: String,
}

/// [`parse_api_key`] çağrısının sonucu.
pub struct ParsedApiKey {
    /// Veritabanında hangi `api_keys` satırına bakılacağını belirler.
    pub key_id: Uuid,
    /// Henüz hash'lenmemiş, doğrulama için `verify_api_secret`'e verilecek
    /// ham secret baytları.
    pub secret_bytes: Vec<u8>,
}

/// Yeni bir API key üretir.
///
/// Biçim: `actos_<key_id_b62>_<secret_b62>`. `key_id` `api_keys.id` olarak
/// kullanılacak bir UUID'dir; string içinde açıkça taşınır çünkü doğrulama
/// akışı önce bu id ile tek bir satır bulur, sonra yalnızca o satırın
/// `secret_hash`'ine karşı tek bir karşılaştırma yapar (bkz.
/// `migrations/0003_api_keys.up.sql` üzerindeki yorum). Secret ise 32
/// baytlık kriptografik rastgele bir değerdir; kimse tahmin edemez.
#[must_use]
pub fn generate_api_key() -> GeneratedApiKey {
    let key_id = Uuid::new_v4();

    // 32 baytı 16+16'lık iki parça olarak üretiyoruz ki base62 kodlaması
    // için doğrudan u128'e çevrilebilsin (bkz. SECRET_CHUNK_WIDTH yorumu).
    let mut hi_bytes = [0u8; 16];
    let mut lo_bytes = [0u8; 16];
    rand::rng().fill(&mut hi_bytes);
    rand::rng().fill(&mut lo_bytes);

    let mut secret_bytes = [0u8; 32];
    secret_bytes[..16].copy_from_slice(&hi_bytes);
    secret_bytes[16..].copy_from_slice(&lo_bytes);

    let hi_num = u128::from_be_bytes(hi_bytes);
    let lo_num = u128::from_be_bytes(lo_bytes);
    let secret_encoded = format!(
        "{:0>SECRET_CHUNK_WIDTH$}{:0>SECRET_CHUNK_WIDTH$}",
        base62::encode(hi_num),
        base62::encode(lo_num),
    );

    let key_id_encoded = base62::encode(key_id.as_u128());
    let plaintext = format!("{API_KEY_PREFIX}_{key_id_encoded}_{secret_encoded}");
    let secret_hash = hash_api_secret(&secret_bytes);

    GeneratedApiKey {
        key_id,
        secret_hash,
        plaintext,
    }
}

/// API key secret'ini hash'ler (veritabanına giden değer).
///
/// **SHA-256 kullanılır, Argon2 değil.** Argon2 gibi yavaş KDF'lerin varlık
/// sebebi düşük entropili, insan tarafından seçilmiş şifreleri kaba kuvvete
/// karşı yavaşlatmaktır. Buradaki secret 256 bit kriptografik rastgele bir
/// değer ve biz ürettik: kaba kuvvetle bulunması zaten hesaplama açısından
/// imkânsız. Yavaşlatmanın hiçbir kazancı olmaz, bedeli ise her tek istekte
/// (her API çağrısında) ödenirdi.
#[must_use]
pub fn hash_api_secret(secret_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret_bytes);
    hex::encode(hasher.finalize())
}

/// Ham bir `actos_<key_id>_<secret>` string'ini ayrıştırır.
///
/// Bozuk girdide asla panik atmaz; her hata durumu ayrı bir
/// [`SecretError`] varyantıyla döner.
///
/// # Errors
/// Önek yanlışsa, parça sayısı yanlışsa, base62 çözülemiyorsa veya
/// secret'in kodlanmış uzunluğu tutmuyorsa.
pub fn parse_api_key(raw: &str) -> Result<ParsedApiKey, SecretError> {
    let mut parts = raw.splitn(3, '_');
    let prefix = parts.next().ok_or(SecretError::MalformedStructure)?;
    let key_id_part = parts.next().ok_or(SecretError::MalformedStructure)?;
    let secret_part = parts.next().ok_or(SecretError::MalformedStructure)?;

    if prefix != API_KEY_PREFIX {
        return Err(SecretError::InvalidPrefix);
    }

    let key_id_num = base62::decode(key_id_part)
        .map_err(|e| SecretError::InvalidKeyIdEncoding(e.to_string()))?;
    let key_id = Uuid::from_u128(key_id_num);

    let secret_bytes = decode_secret(secret_part)?;

    Ok(ParsedApiKey {
        key_id,
        secret_bytes,
    })
}

/// Sabit genişlikte, ardışık iki base62 parçası hâlindeki secret'i çözer.
fn decode_secret(encoded: &str) -> Result<Vec<u8>, SecretError> {
    // `str::split_at` UTF-8 karakter sınırında olmayan bir konumda panik
    // atar; bu yüzden bayt uzunluğunu kontrol ettikten sonra ham baytlar
    // üzerinde bölüyoruz (base62::decode `&[u8]` da kabul ediyor).
    let raw = encoded.as_bytes();
    if raw.len() != SECRET_ENCODED_LEN {
        return Err(SecretError::InvalidSecretLength);
    }

    let (hi_part, lo_part) = raw.split_at(SECRET_CHUNK_WIDTH);
    let hi =
        base62::decode(hi_part).map_err(|e| SecretError::InvalidSecretEncoding(e.to_string()))?;
    let lo =
        base62::decode(lo_part).map_err(|e| SecretError::InvalidSecretEncoding(e.to_string()))?;

    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(&hi.to_be_bytes());
    bytes.extend_from_slice(&lo.to_be_bytes());
    Ok(bytes)
}

/// Ham secret baytlarının, saklanan hash'e karşı doğrulaması.
///
/// **Sabit zamanlı karşılaştırma** kullanır (`subtle::ConstantTimeEq`):
/// `==` ile string karşılaştırması, ilk farklı bayta kadar geçen süreden
/// karakter karakter hash'i sızdırabilecek bir zamanlama kanalı açar.
#[must_use]
pub fn verify_api_secret(secret_bytes: &[u8], stored_hash: &str) -> bool {
    let computed = hash_api_secret(secret_bytes);
    computed.as_bytes().ct_eq(stored_hash.as_bytes()).into()
}

/// [`generate_recovery_codes`] çağrısının ürettiği tek bir kod.
pub struct GeneratedRecoveryCode {
    /// `recovery_codes.code_hash` kolonuna giden Argon2id PHC string'i.
    pub hash: String,
    /// Kullanıcıya **bir kez** gösterilecek ham kod. Saklanmaz.
    pub plaintext: String,
}

/// Kurtarma kodlarının kullandığı Crockford base32 alfabesi.
///
/// I, L, O, U bilinçli olarak dışarıda: I/L rakam 1 ile, O rakam 0 ile
/// karışabilir; U ise yanlışlıkla küfürlü kelimeler oluşmasını önlemek için
/// Crockford'un kendi tavsiyesiyle çıkarılmış.
const CROCKFORD_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// `count` adet kurtarma kodu üretir.
///
/// Biçim: `XXXX-XXXX-XXXX` (12 Crockford base32 karakteri = 60 bit entropi).
///
/// **Argon2id kullanılır** (varsayılan OWASP parametreleriyle) — bu, API
/// key secret'inin tersine bilinçli bir seçim: kurtarma kodları insan
/// tarafından elle yazılabilsin diye kısa tutulur (60 bit), yani API key
/// secret'inin 256 bitine kıyasla kaba kuvvete çok daha açıktır. Veritabanı
/// sızarsa saldırgan hash'leri çevrimdışı deneyebilir; Argon2id'nin
/// yavaşlığı burada gerçek bir kazanç sağlar.
///
/// # Errors
/// Argon2 hash'leme (pratikte hemen hiç olmayan bir iç hata dışında)
/// başarısız olursa.
pub fn generate_recovery_codes(count: usize) -> Result<Vec<GeneratedRecoveryCode>, SecretError> {
    let argon2 = Argon2::default();
    let mut codes = Vec::with_capacity(count);

    for _ in 0..count {
        let plaintext = generate_one_recovery_code();
        // Hash her zaman normalize edilmiş hâl üzerinden alınır ki
        // doğrulama sırasında kullanıcının kodu nasıl yazdığı (tire var mı,
        // büyük/küçük harf, I/L/O karışıklığı) sonucu etkilemesin.
        let normalized = normalize_recovery_code(&plaintext);
        let hash = argon2
            .hash_password(normalized.as_bytes())
            .map_err(|e| SecretError::Hashing(e.to_string()))?
            .to_string();
        codes.push(GeneratedRecoveryCode { hash, plaintext });
    }

    Ok(codes)
}

fn generate_one_recovery_code() -> String {
    let mut rng = rand::rng();
    let mut chars = [0u8; 12];
    for c in &mut chars {
        let idx = rng.random_range(0..CROCKFORD_ALPHABET.len());
        *c = CROCKFORD_ALPHABET[idx];
    }

    // SAFETY yok, sadece ASCII: CROCKFORD_ALPHABET tamamen ASCII olduğu için
    // bu her zaman geçerli bir UTF-8 string'dir.
    let raw = std::str::from_utf8(&chars).unwrap_or_default();
    format!("{}-{}-{}", &raw[0..4], &raw[4..8], &raw[8..12])
}

/// Bir kurtarma kodunu doğrulanabilir kanonik hâline indirger.
///
/// Tireleri ve boşlukları atar, büyük harfe çevirir ve Crockford'un okuma
/// toleransını uygular (`I`/`L` → `1`, `O` → `0`). Doğrulama her zaman bu
/// normalize edilmiş hâl üzerinden yapılır.
#[must_use]
pub fn normalize_recovery_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .map(|c| match c {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect()
}

/// Bir kurtarma kodu adayının, saklanan Argon2id hash'ine karşı doğrulaması.
///
/// `candidate` önce [`normalize_recovery_code`] ile kanonikleştirilir; yani
/// kullanıcı kodu tireli/tiresiz, büyük/küçük harf ya da I/L/O karışık
/// yazmış olsa da doğrulama aynı sonucu verir.
#[must_use]
pub fn verify_recovery_code(candidate: &str, stored_hash: &str) -> bool {
    let normalized = normalize_recovery_code(candidate);
    let Ok(parsed_hash) = PasswordHash::new(stored_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(normalized.as_bytes(), &parsed_hash)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn üretilen_key_geri_ayrıştırılıyor_ve_key_id_eşleşiyor() {
        let generated = generate_api_key();
        let parsed = parse_api_key(&generated.plaintext).expect("geçerli key ayrıştırılmalı");
        assert_eq!(parsed.key_id, generated.key_id);
    }

    #[test]
    fn üretilen_secret_kendi_hash_ini_doğruluyor() {
        let generated = generate_api_key();
        let parsed = parse_api_key(&generated.plaintext).expect("geçerli key ayrıştırılmalı");
        assert!(verify_api_secret(
            &parsed.secret_bytes,
            &generated.secret_hash
        ));
    }

    #[test]
    fn farklı_secret_doğrulamıyor() {
        let a = generate_api_key();
        let b = generate_api_key();
        let parsed_b = parse_api_key(&b.plaintext).expect("geçerli key ayrıştırılmalı");
        assert!(!verify_api_secret(&parsed_b.secret_bytes, &a.secret_hash));
    }

    #[test]
    fn bozuk_key_biçimleri_panik_atmadan_hata_dönüyor() {
        assert!(matches!(
            parse_api_key("boşönek_abc_def"),
            Err(SecretError::InvalidPrefix)
        ));
        assert!(parse_api_key("actos_sadece_iki_parça").is_err());
        assert!(parse_api_key("actos_abc").is_err());
        assert!(parse_api_key("").is_err());
        assert!(parse_api_key("actos__").is_err());
        // Geçersiz base62 karakteri (secret bölümünde '!' var).
        let bogus = format!("actos_{}_{}", "1", "!".repeat(SECRET_ENCODED_LEN));
        assert!(parse_api_key(&bogus).is_err());
        // Uzunluğu tutmayan secret.
        let short = format!("actos_{}_{}", "1", "1".repeat(SECRET_ENCODED_LEN - 1));
        assert!(matches!(
            parse_api_key(&short),
            Err(SecretError::InvalidSecretLength)
        ));
        // ASCII olmayan / geçersiz UTF-8 sınırı denemesi de panik atmamalı.
        assert!(parse_api_key("actos_é_é").is_err());
    }

    #[test]
    fn bin_key_üret_hepsi_benzersiz() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let generated = generate_api_key();
            assert!(seen.insert(generated.plaintext), "çakışan key üretildi");
        }
    }

    #[test]
    fn kurtarma_kodu_kendi_hash_ini_doğruluyor() {
        let codes = generate_recovery_codes(1).expect("üretim başarılı olmalı");
        let code = &codes[0];
        assert!(verify_recovery_code(&code.plaintext, &code.hash));
    }

    #[test]
    fn yanlış_kurtarma_kodu_doğrulamıyor() {
        let codes = generate_recovery_codes(2).expect("üretim başarılı olmalı");
        assert!(!verify_recovery_code(&codes[0].plaintext, &codes[1].hash));
    }

    #[test]
    fn normalize_farklı_yazımları_aynı_sonuca_indirgiyor() {
        let a = normalize_recovery_code("abcd-efgh-jkmn");
        let b = normalize_recovery_code("ABCDEFGHJKMN");
        let c = normalize_recovery_code("abcd efgh jkmn");
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a, "ABCDEFGHJKMN");
    }

    #[test]
    fn normalize_crockford_okuma_toleransı() {
        assert_eq!(normalize_recovery_code("I"), "1");
        assert_eq!(normalize_recovery_code("L"), "1");
        assert_eq!(normalize_recovery_code("O"), "0");
        assert_eq!(normalize_recovery_code("il-o"), "110");
    }

    #[test]
    fn üretilen_kurtarma_kodları_benzersiz() {
        let codes = generate_recovery_codes(1000).expect("üretim başarılı olmalı");
        let mut seen = HashSet::new();
        for code in &codes {
            assert!(seen.insert(code.plaintext.clone()), "çakışan kod üretildi");
        }
    }
}
