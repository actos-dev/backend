//! Dış (public) ID'ler: `bigint` birincil anahtarları, dışarıya **ardışık
//! olmayan, kısa, tip etiketli** stringler olarak taşır (`1` yerine
//! `a_7fGh2Kd`).
//!
//! ## Neden
//!
//! Ardışık sayısal ID'ler iki sorun çıkarır: (1) numaralandırma saldırısını
//! kolaylaştırır (`/contents/1`, `/contents/2`, ... diye tarayabilirsin), (2)
//! platformun hacmini sızdırır ("son ID 40000, demek ki bugün ~400 post
//! atılmış"). Ayrıca ham birincil anahtarı dışarı vermek, ileride ID
//! stratejisini (ör. UUID'ye geçiş, sharding) değiştirmeyi imkânsız kılar —
//! istemciler o sayıyı opak bir string sanmalı.
//!
//! ## Tasarım: anahtarlı Feistel permütasyonu
//!
//! Her ID, 64 bitlik **dengeli Feistel ağından** (4 tur) geçirilir. Feistel
//! ağının önemli özelliği: tur fonksiyonu (burada HMAC-SHA256) hiçbir şekilde
//! kendisi bijektif olmak zorunda değildir, ama ağın **tamamı her zaman bir
//! bijeksiyondur** — her 64 bit girdi tam olarak bir 64 bit çıktıya gider ve
//! tersi de geçerlidir. Bu bize üç şey kazandırır:
//!
//! - **Çakışma imkânsız**: iki farklı satır asla aynı dış ID'yi almaz.
//! - **Tersine çevrilebilir**: dış ID'den iç `bigint`'e dönüş, aynı anahtarla
//!   turları ters sırada uygulamaktan ibaret; ekstra bir arama tablosuna
//!   gerek yok.
//! - **Ekstra DB kolonu gerekmez**: permütasyon anahtar + tur sayısı
//!   dışında hiçbir durum (state) taşımaz.
//!
//! Tur fonksiyonu `HMAC-SHA256(key, domain || entity_tag || round || right)`
//! çıktısının ilk 4 baytıdır (`round_fn`). Anahtar
//! [`crate::config::SecurityConfig::id_obfuscation_key`].
//!
//! **Alan ayrımı (domain separation):** tur fonksiyonuna varlık türü etiketi
//! (`IdKind::TAG`) de girer. Bu sayede `actors` tablosundaki 5 numaralı satır
//! ile `contents` tablosundaki 5 numaralı satır **farklı** dış ID'lere
//! eşlenir — aynı sayıya eşlenselerdi, bir varlık türündeki ID'yi bilmek
//! diğerindeki karşılık gelen satır hakkında bilgi sızdırırdı (ör. "actor
//! #5'in kaydı content #5'le aynı anda mı oluşturuldu?").
//!
//! ## Ortak ön ek: post ve yorum
//!
//! `Actor` → `a`, `Content` → `c`, `Tag` → `t`. **Post ve yorum ayrı ön ek
//! ALMAZ**: ikisi de `contents` tablosunda aynı ID uzayında yaşar (bkz.
//! migration şeması). Ayrı ön ek verilseydi (`p_` post, `c_` yorum gibi)
//! ikisini birden kabul eden uçlar (`GET /contents/{id}` gibi, bir content
//! post da olabilir yorum da) ya iki ayrı prefix'i de kabul edecek şekilde
//! karmaşıklaşırdı ya da post/yorum arasında geçiş (ör. bir yorumun post'a
//! "yükseltilmesi" gibi bir özellik hiç olmasa bile, salt kavramsal olarak
//! ikisinin aynı tablo olması) ID biçimiyle çelişirdi. Tek bir `Content`
//! türü, tek bir prefix, DB şemasındaki gerçeği birebir yansıtır.
//!
//! ## Anahtarın taşınması: neden `IdCodec`
//!
//! `PublicId <-> i64` dönüşümü permütasyon anahtarına ihtiyaç duyar. Ama
//! `Display`/`FromStr` gibi standart trait'lerin imzalarında anahtar
//! parametresi yoktur — anahtarı oraya sokmanın tek yolu bir tür global
//! duruma (`static`, `OnceLock` içine gizlenmiş anahtar vb.) başvurmak
//! olurdu. Bilinçli olarak **bundan kaçındık**: anahtar test edilebilirliği
//! kırar (testte farklı anahtarlarla farklı davranış görmek isteriz),
//! rotasyonu imkânsızlaştırır ve "bu fonksiyon nereden anahtar okuyor?"
//! sorusunu koda gizli, izi sürülmesi zor bir bağımlılık olarak gömer.
//!
//! Bunun yerine dönüşüm ikiye ayrıldı:
//!
//! - [`PublicId<K>`] salt **tipli bir taşıyıcı**: içinde zaten kodlanmış ham
//!   string'i (`a_7fGh2Kd`) tutar, `Display`/`FromStr`/serde bu string
//!   üzerinde çalışır ve anahtara ihtiyaç duymaz. `FromStr`/serde ayrıştırma
//!   sırasında yalnızca **yapısal doğrulama** yapılır (doğru prefix, geçerli
//!   base62 gövde) — bu, anahtar olmadan da yapılabilir ve bozuk girdiyi
//!   veritabanına hiç gitmeden reddetmemizi sağlar. Ama bu string'in
//!   *hangi* iç `bigint`'e karşılık geldiğini `PublicId` kendi başına bilmez.
//! - [`IdCodec`] anahtarı açıkça taşıyan yapı: gerçek permütasyonu
//!   (`encode`/`decode`) o yapar. Çağıran taraf (repository katmanı)
//!   `Config`'ten okuduğu anahtarla bir `IdCodec` kurar ve onu parametre
//!   olarak geçirir — anahtar hep açık, izlenebilir bir değer olarak kalır.
//!
//! Bu ayrım sayesinde tip güvenliği (`get_post(actor_id)` derlenmez) ile
//! anahtar yönetimi birbirinden bağımsızlaşıyor: bir fonksiyon imzasında
//! `PublicId<Content>` görmek "bu bir content ID'si" der, `IdCodec` görmek
//! "bu fonksiyon gerçek dönüşüm yapıyor" der.
//!
//! (Not: görevin ilk taslağında `PublicId::from_internal`/`.internal()` gibi
//! anahtarsız metotlar öneriliyordu; bunlar kasıtlı olarak buraya
//! eklenmedi, çünkü anahtarsız çalışamazlar — yukarıdaki gerekçeyle bu
//! sorumluluk tamamen `IdCodec`'e taşındı.)

use std::{fmt, marker::PhantomData};

use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// HMAC girdisine eklenen sabit alan ayrımı ön eki.
///
/// `id_obfuscation_key` ileride başka bir amaçla (ör. farklı bir HMAC
/// tabanlı mekanizma) yeniden kullanılırsa, bu ön ek o kullanımla bizim
/// tur fonksiyonumuzun girdi uzaylarının çakışmasını engeller.
const HMAC_DOMAIN: &[u8] = b"actos:id:v1";

/// Feistel ağının tur sayısı. 4 tur, 32 bitlik yarım bloklarda tam
/// difüzyon (her çıktı bitinin her girdi bitine bağlı hâle gelmesi) için
/// kriptografi literatüründeki (Luby–Rackoff) asgari önerinin üzerinde bir
/// paydır; amacımız kanıtlanabilir kriptografik güvenlik değil (anahtar
/// zaten sunucuda kalıyor), ardışıklığı göz ile de istatistiksel olarak da
/// fark edilemez kılmak.
const ROUNDS: u8 = 4;

/// Kodlanmış bir ID'nin gövde (prefix hariç) kısmının azami base62 karakter
/// sayısı. `u64::MAX` base62'de en fazla 11 karakter tutar (`62^10` <
/// `u64::MAX` < `62^11`); bu permütasyonun çıktı uzayı tam olarak `u64`
/// olduğu için geçerli hiçbir kodlanmış ID bu sınırı aşamaz.
const MAX_BODY_LEN: usize = 11;

/// Kodlanmış bir ID'nin (prefix + `_` + gövde) azami toplam uzunluğu.
/// Şu an tüm prefix'ler tek karakter, dolayısıyla `1 + 1 + MAX_BODY_LEN`.
const MAX_RAW_LEN: usize = 1 + 1 + MAX_BODY_LEN;

/// Bir varlık türünü (entity kind) tip düzeyinde işaretler.
///
/// `PublicId<K>` üzerinden tip güvenliği bu trait sayesinde çalışır:
/// `PublicId<Actor>` ile `PublicId<Content>` derleyici için farklı tiplerdir,
/// biri diğerinin beklendiği yere örtük geçirilemez.
pub trait IdKind {
    /// Feistel tur fonksiyonuna giren alan ayrımı etiketi. Varlık türleri
    /// arasında benzersiz olmalı — aksi hâlde farklı tablolardaki aynı
    /// sayı aynı dış ID'ye eşlenir (bkz. modül başındaki gerekçe).
    const TAG: u8;
    /// Dış ID string'inin ön eki (ör. `"a"` → `a_7fGh2Kd`).
    const PREFIX: &'static str;
}

/// `actors` tablosundaki satırlar.
pub struct Actor;
impl IdKind for Actor {
    const TAG: u8 = 0;
    const PREFIX: &'static str = "a";
}

/// `contents` tablosundaki satırlar — hem post hem yorum (bkz. modül
/// başındaki "Ortak ön ek" bölümü).
pub struct Content;
impl IdKind for Content {
    const TAG: u8 = 1;
    const PREFIX: &'static str = "c";
}

/// `tags` tablosundaki satırlar.
pub struct Tag;
impl IdKind for Tag {
    const TAG: u8 = 2;
    const PREFIX: &'static str = "t";
}

/// `attachments` tablosundaki satırlar (Faz 13).
pub struct Attachment;
impl IdKind for Attachment {
    const TAG: u8 = 3;
    const PREFIX: &'static str = "f";
}

/// `reports` tablosundaki satırlar (Faz 14).
pub struct Report;
impl IdKind for Report {
    const TAG: u8 = 4;
    const PREFIX: &'static str = "r";
}

/// `notifications` tablosundaki satırlar (Faz 18.A, bildirimler).
pub struct Notification;
impl IdKind for Notification {
    const TAG: u8 = 5;
    const PREFIX: &'static str = "n";
}

/// ID üretme/ayrıştırma sırasında oluşan hatalar. Hiçbiri panik değildir:
/// istemciden gelen bozuk bir ID, bu tiplerden biriyle geri döner.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdError {
    #[error("ID cannot be empty")]
    Empty,

    #[error("unrecognized ID prefix (expected \"{expected}_\")")]
    InvalidPrefix { expected: &'static str },

    #[error("invalid ID structure: prefix and body must be separated by a single `_`")]
    MalformedStructure,

    #[error("ID too long (must be at most {max} characters)")]
    TooLong { max: usize },

    #[error("ID body could not be decoded as base62: {0}")]
    InvalidEncoding(String),

    #[error("ID out of range")]
    OutOfRange,

    #[error("internal value cannot be negative: {0}")]
    NegativeInternal(i64),

    #[error("ID encoding key cannot be empty")]
    EmptyKey,
}

/// Kodlanmış bir dış ID'nin yapısal olarak geçerli olup olmadığını denetler
/// (doğru prefix, boş olmayan ve base62 olarak çözülebilen bir gövde).
///
/// Bu denetim anahtar **gerektirmez**: base62 çözümü ve prefix karşılaştırma
/// salt string işlemleridir, permütasyonu tersine çevirmez. Başarılı olursa
/// gövdenin çözülmüş sayısal değerini döner — hem [`PublicId::parse`] hem
/// [`IdCodec::decode`] bunu kullanır, ikinci kez base62 çözmeye gerek kalmaz.
fn validate_structure<K: IdKind>(raw: &str) -> Result<u128, IdError> {
    if raw.is_empty() {
        return Err(IdError::Empty);
    }
    if raw.len() > MAX_RAW_LEN {
        return Err(IdError::TooLong { max: MAX_RAW_LEN });
    }

    let (prefix, body) = raw.split_once('_').ok_or(IdError::MalformedStructure)?;
    if prefix != K::PREFIX {
        return Err(IdError::InvalidPrefix {
            expected: K::PREFIX,
        });
    }
    if body.is_empty() {
        return Err(IdError::MalformedStructure);
    }
    if body.len() > MAX_BODY_LEN {
        return Err(IdError::TooLong { max: MAX_BODY_LEN });
    }

    base62::decode(body).map_err(|e| IdError::InvalidEncoding(e.to_string()))
}

/// Tipli, dış (public) bir ID.
///
/// İçinde zaten kodlanmış ham string'i (`a_7fGh2Kd`) tutar — permütasyonu
/// **çözmez**, dolayısıyla anahtara ihtiyaç duymaz (bkz. modül başındaki
/// "Anahtarın taşınması" bölümü). `K` yalnızca derleme zamanı tip etiketidir;
/// çalışma zamanında hiçbir yer kaplamaz.
pub struct PublicId<K: IdKind> {
    raw: String,
    _marker: PhantomData<K>,
}

impl<K: IdKind> PublicId<K> {
    /// Ham bir ID string'ini yapısal olarak doğrular ve sarmalar.
    ///
    /// Bu, string'in *gerçekten* var olan bir satıra karşılık geldiğini
    /// **garanti etmez** (bunun için anahtar ve genelde bir DB sorgusu
    /// gerekir) — sadece doğru tipte, doğru biçimde bir ID gibi göründüğünü
    /// doğrular. Asıl iç `bigint`'e çözüm [`IdCodec::decode_id`] iledir.
    ///
    /// # Errors
    /// Boşsa, önek yanlışsa, yapı bozuksa, gövde base62 değilse veya aşırı
    /// uzunsa.
    pub fn parse(raw: &str) -> Result<Self, IdError> {
        validate_structure::<K>(raw)?;
        Ok(Self {
            raw: raw.to_owned(),
            _marker: PhantomData,
        })
    }

    /// Ham string gösterimi (`a_7fGh2Kd`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl<K: IdKind> Clone for PublicId<K> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            _marker: PhantomData,
        }
    }
}

impl<K: IdKind> fmt::Debug for PublicId<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PublicId").field(&self.raw).finish()
    }
}

impl<K: IdKind> fmt::Display for PublicId<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl<K: IdKind> std::str::FromStr for PublicId<K> {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl<K: IdKind> PartialEq for PublicId<K> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<K: IdKind> Eq for PublicId<K> {}

impl<K: IdKind> std::hash::Hash for PublicId<K> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

// Şeffaf serde: JSON'da düz bir string olarak görünür (`"a_7fGh2Kd"`),
// `{"raw": "..."}` gibi bir sarmalayıcı obje değil. `#[serde(transparent)]`
// kullanmıyoruz çünkü o yalnızca tek alanlı struct'ı olduğu gibi
// serialize eder; biz ayrıştırma sırasında `validate_structure` doğrulamasını
// da çalıştırmak istiyoruz (bkz. `Deserialize` impl'i).
impl<K: IdKind> Serialize for PublicId<K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de, K: IdKind> Deserialize<'de> for PublicId<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// İç `bigint` (`i64`) ile dış [`PublicId`] arasında anahtarlı dönüşüm yapar.
///
/// Anahtar açıkça bu yapı içinde taşınır (bkz. modül başındaki gerekçe) —
/// global/gizli bir duruma başvurulmaz. Çağıran taraf `Config`'ten okuduğu
/// [`crate::config::SecurityConfig::id_obfuscation_key`] ile bir kere kurar
/// ve repository katmanına parametre olarak geçirir.
pub struct IdCodec {
    key: Vec<u8>,
}

impl IdCodec {
    /// Verilen anahtarla bir kodlayıcı kurar.
    ///
    /// Anahtar uzunluğu burada zorlanmıyor (HMAC her uzunlukta anahtarı
    /// kabul eder) — asgari 32 karakter kuralı zaten `Config::validate`'te
    /// uygulanıyor (bkz. `config.rs`); burada yalnızca boş anahtarı
    /// reddediyoruz, çünkü boş anahtarla permütasyon hâlâ "çalışır" ama
    /// tahmin edilebilir olur.
    ///
    /// # Errors
    /// Anahtar boşsa.
    pub fn new(key: &str) -> Result<Self, IdError> {
        // `trim()` ile bakıyoruz: yalnızca boşluktan oluşan bir anahtar da
        // pratikte boştur. config.rs'teki `required()` de aynı ölçüyü kullanıyor.
        if key.trim().is_empty() {
            return Err(IdError::EmptyKey);
        }
        Ok(Self {
            key: key.as_bytes().to_vec(),
        })
    }

    /// İç `bigint`'i `K` türünde dış bir ID string'ine kodlar.
    ///
    /// # Errors
    /// `internal` negatifse (geçerli bir `bigint` birincil anahtarı asla
    /// negatif olmaz; bu, çağıranın bir hatasını işaret eder ama yine de
    /// panik atmak yerine hata döneriz).
    pub fn encode<K: IdKind>(&self, internal: i64) -> Result<String, IdError> {
        let value = u64::try_from(internal).map_err(|_| IdError::NegativeInternal(internal))?;
        let permuted = self.permute::<K>(value, true);
        Ok(format!("{}_{}", K::PREFIX, base62::encode(permuted)))
    }

    /// İç `bigint`'i `K` türünde tipli bir [`PublicId`]'e kodlar.
    ///
    /// # Errors
    /// [`Self::encode`] ile aynı.
    pub fn encode_id<K: IdKind>(&self, internal: i64) -> Result<PublicId<K>, IdError> {
        Ok(PublicId {
            raw: self.encode::<K>(internal)?,
            _marker: PhantomData,
        })
    }

    /// Dış bir ID string'ini `K` türünde çözüp iç `bigint`'i döner.
    ///
    /// # Errors
    /// Boşsa, önek yanlışsa, yapı/base62 bozuksa, aşırı uzunsa veya çözülen
    /// değer geçerli bir `i64` aralığının dışına düşüyorsa (bu sonuncusu,
    /// bu koddan hiç geçmemiş, elle uydurulmuş bir string'e işaret eder).
    pub fn decode<K: IdKind>(&self, raw: &str) -> Result<i64, IdError> {
        let decoded = validate_structure::<K>(raw)?;
        let value = u64::try_from(decoded).map_err(|_| IdError::OutOfRange)?;
        let unpermuted = self.permute::<K>(value, false);
        i64::try_from(unpermuted).map_err(|_| IdError::OutOfRange)
    }

    /// Tipli bir [`PublicId`]'i `K` türünde çözüp iç `bigint`'i döner.
    ///
    /// # Errors
    /// [`Self::decode`] ile aynı.
    pub fn decode_id<K: IdKind>(&self, id: &PublicId<K>) -> Result<i64, IdError> {
        self.decode::<K>(&id.raw)
    }

    /// Feistel tur fonksiyonu: `HMAC-SHA256(key, domain || tag || round ||
    /// right)`'in ilk 4 baytı, büyük-uçlu (big-endian) bir `u32` olarak.
    fn round_fn(&self, tag: u8, round: u8, half: u32) -> u32 {
        // HMAC her uzunlukta anahtarı kabul eder — `hmac` crate'inin kendi
        // `KeyInit::new` implementasyonu da aynı gerekçeyle `expect`
        // kullanıyor (bkz. hmac-0.13.0/src/simple.rs). `self.key` burada
        // sabit, önceden doğrulanmış bir baytdizisi; bu `Result` hiçbir
        // koşulda `Err` olmaz.
        #[allow(clippy::expect_used)]
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC herhangi bir uzunluktaki anahtarı kabul eder");
        mac.update(HMAC_DOMAIN);
        mac.update(&[tag, round]);
        mac.update(&half.to_be_bytes());
        let digest = mac.finalize().into_bytes();
        u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]])
    }

    /// 64 bitlik dengeli Feistel ağı. `forward = true` iken permütasyon
    /// (kodlama), `false` iken tersi (çözme) uygulanır.
    ///
    /// Feistel ağının bijeksiyon olma özelliği `round_fn`'in kendisinin
    /// bijektif olmasına bağlı **değildir** — bu yüzden `round_fn`'in
    /// çıktısını doğrudan HMAC'ten kırpıp kullanabiliyoruz, ekstra bir
    /// tersinirlik kanıtına gerek yok (bkz. modül başındaki gerekçe).
    fn permute<K: IdKind>(&self, value: u64, forward: bool) -> u64 {
        let mut l = (value >> 32) as u32;
        let mut r = value as u32;

        if forward {
            for round in 0..ROUNDS {
                let f = self.round_fn(K::TAG, round, r);
                let new_r = l ^ f;
                l = r;
                r = new_r;
            }
        } else {
            for round in (0..ROUNDS).rev() {
                let f = self.round_fn(K::TAG, round, l);
                let new_l = r ^ f;
                r = l;
                l = new_l;
            }
        }

        (u64::from(l) << 32) | u64::from(r)
    }
}

impl From<IdError> for crate::Error {
    fn from(err: IdError) -> Self {
        Self::Validation(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde::{Deserialize, Serialize};

    use super::*;

    const KEY_A: &str = "bu-en-az-otuz-iki-karakterlik-bir-anahtar-metni";
    const KEY_B: &str = "tamamen-farkli-ikinci-bir-anahtar-metni-buraya";

    fn codec() -> IdCodec {
        IdCodec::new(KEY_A).expect("geçerli anahtar")
    }

    #[test]
    fn bijeksiyon_yuz_bine_kadar_round_trip() {
        let c = codec();
        for n in 0..=100_000i64 {
            let encoded = c.encode::<Actor>(n).expect("kodlama başarılı olmalı");
            let decoded = c
                .decode::<Actor>(&encoded)
                .unwrap_or_else(|e| panic!("çözme başarısız oldu ({encoded}): {e}"));
            assert_eq!(decoded, n, "round-trip {n} için tutmadı");
        }
    }

    #[test]
    fn cakisma_yok_yuz_bin_deger_benzersiz() {
        let c = codec();
        let mut seen = HashSet::new();
        for n in 0..=100_000i64 {
            let encoded = c.encode::<Actor>(n).expect("kodlama başarılı olmalı");
            assert!(seen.insert(encoded), "n={n} için çakışan ID üretildi");
        }
        assert_eq!(seen.len(), 100_001);
    }

    #[test]
    fn ardisiklik_gizleniyor() {
        let c = codec();
        let mut permuted_values = Vec::with_capacity(1000);
        for n in 1..=1000i64 {
            let encoded = c.encode::<Actor>(n).expect("kodlama başarılı olmalı");
            let (_, body) = encoded.split_once('_').expect("prefix ayracı olmalı");
            let value = base62::decode(body).expect("geçerli base62 olmalı");
            permuted_values.push(value);
        }

        // Girdi ardışık (1,2,3,...) olsa da çıktı ardışık olmamalı: ardışık
        // farkların hepsi aynı olsaydı bu, gizli bir doğrusal (affine)
        // dönüşüme işaret ederdi — permütasyonun amacı tam da bunu önlemek.
        let diffs: HashSet<i128> = permuted_values
            .windows(2)
            .map(|w| w[1] as i128 - w[0] as i128)
            .collect();
        assert!(
            diffs.len() > 1,
            "ardışık girdilerin farkları hep aynı, permütasyon ardışıklığı gizlemiyor"
        );

        // Ayrıca çıktı değerleri, girdiyle aynı sırada artmıyor olmalı.
        let is_sorted = permuted_values.windows(2).all(|w| w[0] < w[1]);
        assert!(!is_sorted, "permütasyon çıktısı hâlâ artan sırada");
    }

    #[test]
    fn alan_ayrimi_farkli_turler_farkli_id_uretir() {
        let c = codec();
        let actor_id = c.encode::<Actor>(42).expect("kodlama başarılı olmalı");
        let content_id = c.encode::<Content>(42).expect("kodlama başarılı olmalı");
        assert_ne!(actor_id, content_id);

        // Sadece prefix değil, gövde (permütasyon çıktısı) de farklı olmalı.
        let actor_body = actor_id.split_once('_').expect("ayraç").1;
        let content_body = content_id.split_once('_').expect("ayraç").1;
        assert_ne!(actor_body, content_body);
    }

    #[test]
    fn anahtar_duyarliligi_farkli_anahtar_farkli_id_uretir() {
        let a = IdCodec::new(KEY_A).expect("geçerli anahtar");
        let b = IdCodec::new(KEY_B).expect("geçerli anahtar");
        assert_ne!(
            a.encode::<Actor>(42).expect("kodlama başarılı olmalı"),
            b.encode::<Actor>(42).expect("kodlama başarılı olmalı"),
        );
    }

    #[test]
    fn onek_dogrulamasi_yanlis_turle_cozulemiyor() {
        let c = codec();
        let actor_id = c.encode::<Actor>(7).expect("kodlama başarılı olmalı");
        assert_eq!(
            c.decode::<Content>(&actor_id),
            Err(IdError::InvalidPrefix { expected: "c" })
        );
    }

    #[test]
    fn bozuk_girdiler_panik_atmadan_hata_donuyor() {
        let c = codec();

        assert_eq!(c.decode::<Actor>(""), Err(IdError::Empty));
        assert!(matches!(
            c.decode::<Actor>("oneksiz-govde"),
            Err(IdError::MalformedStructure)
        ));
        assert!(matches!(
            c.decode::<Actor>("c_7fGh2Kd"),
            Err(IdError::InvalidPrefix { expected: "a" })
        ));
        assert!(matches!(
            c.decode::<Actor>("a_!!!not-base62!!!"),
            Err(IdError::InvalidEncoding(_) | IdError::TooLong { .. })
        ));
        assert!(matches!(
            c.decode::<Actor>("a_"),
            Err(IdError::MalformedStructure)
        ));
        // Aşırı uzun girdi (ne prefix ne base62 olarak makul).
        let too_long = format!("a_{}", "1".repeat(500));
        assert!(matches!(
            c.decode::<Actor>(&too_long),
            Err(IdError::TooLong { .. })
        ));

        // `PublicId::parse` da aynı şekilde panik atmadan hata döner.
        assert!(PublicId::<Actor>::parse("").is_err());
        assert!(PublicId::<Actor>::parse("z_abc").is_err());
        assert!(PublicId::<Actor>::parse(&too_long).is_err());
    }

    #[test]
    fn bos_anahtarla_kodlayici_kurulamiyor() {
        // `IdCodec` bilerek `Debug`/`PartialEq` türetmiyor (içinde anahtar var,
        // yanlışlıkla loglanmasın), o yüzden `assert_eq!` yerine `matches!`.
        assert!(matches!(IdCodec::new(""), Err(IdError::EmptyKey)));
        assert!(matches!(IdCodec::new("   "), Err(IdError::EmptyKey)));
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Wrapper {
        id: PublicId<Actor>,
        label: String,
    }

    #[test]
    fn serde_public_id_json_string_olarak_tasiniyor() {
        let c = codec();
        let id = c.encode_id::<Actor>(123).expect("kodlama başarılı olmalı");
        let wrapper = Wrapper {
            id,
            label: "test".to_owned(),
        };

        let json = serde_json::to_string(&wrapper).expect("json'a yazılabilmeli");
        assert!(
            json.contains("\"id\":\"a_"),
            "id alanı düz string olarak yazılmalı: {json}"
        );

        let parsed: Wrapper = serde_json::from_str(&json).expect("json'dan okunabilmeli");
        assert_eq!(parsed, wrapper);
    }

    #[test]
    fn sinir_degerleri_sifir_ve_i64_max() {
        let c = codec();

        let zero = c.encode::<Content>(0).expect("kodlama başarılı olmalı");
        assert_eq!(c.decode::<Content>(&zero).expect("çözülebilmeli"), 0);

        let max = c
            .encode::<Content>(i64::MAX)
            .expect("kodlama başarılı olmalı");
        assert_eq!(c.decode::<Content>(&max).expect("çözülebilmeli"), i64::MAX);
    }

    #[test]
    fn negatif_ic_deger_hata_donuyor() {
        let c = codec();
        assert_eq!(c.encode::<Actor>(-1), Err(IdError::NegativeInternal(-1)));
    }
}
