//! Keyset (cursor) sayfalaması.
//!
//! **`OFFSET` kullanmıyoruz.** Büyük veri setlerinde `OFFSET 50000` veritabanına
//! 50000 satırı okuyup atmayı zorlar — sayfa numarası büyüdükçe sorgu
//! yavaşlar. Daha kötüsü: sayfalar arasında yeni içerik eklenirse (ya da
//! silinirse) `OFFSET` kaymaya uğrar, kullanıcı bazı satırları iki kez görür
//! ya da hiç görmez.
//!
//! Bunun yerine cursor, "en son gördüğün satırın sıralama anahtarı + id'si"ni
//! taşır. Sonraki sayfa `WHERE (sort_key, id) < (cursor_key, cursor_id)` ile
//! (DESC sıralamada) gelir — bu da doğrudan `migrations/0006_contents_indexes`
//! içindeki `(sort_key DESC, id DESC)` bileşik index'lerine karşılık gelir,
//! ekstra bir sıralama adımı gerektirmez.
//!
//! ## Gövde kodlaması: JSON değil, sabit genişlikte ikili
//!
//! Gövde JSON yerine elle paketlenmiş sabit genişlikte baytlar olarak
//! kodlanıyor:
//!
//! - **Boyut:** JSON alan adları + ayraçlar + `DateTime` string temsili
//!   (`"2024-01-01T00:00:00.123456Z"`) cursor'ı gereksiz yere büyütür — bu
//!   string her sayfalama isteğinde URL'de taşınıyor. İkili gösterim sabit
//!   18 baytlık gövde (+ 32 baytlık HMAC etiketi) üretir; base64url'e
//!   döküldüğünde ~67 karakter.
//! - **Basitlik:** alanların sayısı ve tipleri sabit (sürüm + tür + 8 baytlık
//!   değer + 8 baytlık id); ayrıştırma dallanmasız, sabit ofsetlerle yapılıyor.
//!

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::error::Error;

/// Gövde biçiminin sürümü. Bkz. modül dokümantasyonundaki "Sürüm baytı" bölümü.
const CURSOR_VERSION: u8 = 1;

/// HMAC-SHA256 çıktısının bayt uzunluğu.
const HMAC_TAG_LEN: usize = 32;

/// İmzalanan gövdenin sabit bayt uzunluğu: 1 (sürüm) + 1 (sıralama türü) + 8
/// (değer: `created_at` mikrosaniye / `score` işaret-genişletilmiş /
/// `hot_score` bit deseni) + 8 (id).
const BODY_LEN: usize = 1 + 1 + 8 + 8;

/// Base64url'e kodlanmadan önceki toplam bayt uzunluğu (gövde + imza).
const TOTAL_LEN: usize = BODY_LEN + HMAC_TAG_LEN;

/// Bir feed sayfasının sıralanma biçimi, sıralama anahtarının değeriyle
/// birlikte.
///
/// Bu üç varyant, `migrations/0006_contents_indexes.up.sql` içindeki üç feed
/// index'ine bire bir karşılık gelir:
///
/// - [`SortKey::New`] → `idx_contents_new (created_at DESC, id DESC)`
/// - [`SortKey::Top`] → `idx_contents_top (score DESC, id DESC)`
/// - [`SortKey::Hot`] → `idx_contents_hot (hot_score DESC, id DESC)`
///
/// Üçünde de ikincil sıralama `id DESC` — bu yüzden [`Cursor::id`] her zaman
/// eşlik eder ve `(sort_key, id)` çifti sayfa sınırını tam olarak belirler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortKey {
    /// En yeni içerik önce. `idx_contents_new` ile hizalı.
    New { created_at: DateTime<Utc> },
    /// En yüksek skor önce. `idx_contents_top` ile hizalı.
    Top { score: i32 },
    /// En "sıcak" içerik önce (bkz. PLAN.md Faz 12 hot score formülü).
    /// `idx_contents_hot` ile hizalı.
    Hot { hot_score: f64 },
}

impl SortKey {
    /// Bu anahtarın hangi [`SortKind`]'a ait olduğu.
    #[must_use]
    pub const fn kind(&self) -> SortKind {
        match self {
            Self::New { .. } => SortKind::New,
            Self::Top { .. } => SortKind::Top,
            Self::Hot { .. } => SortKind::Hot,
        }
    }
}

/// [`SortKey`]'in taşıdığı değer olmadan, yalnızca *türü*.
///
/// [`CursorCodec::decode`] çağıranın "hangi sıralamayı bekliyorum"
/// bildirmesi için kullanılır — cursor gövdesindeki türle eşleşmezse
/// çözme reddedilir (bkz. modül dokümantasyonu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKind {
    New,
    Top,
    Hot,
}

impl SortKind {
    /// Gövdede bu türü temsil eden tek bayt.
    const fn discriminant(self) -> u8 {
        match self {
            Self::New => 0,
            Self::Top => 1,
            Self::Hot => 2,
        }
    }
}

/// "En son gördüğün satır" — bir feed sayfasının sonundaki konum.
///
/// Sonraki sayfa sorgusu bu değerden türetilir:
/// `WHERE (sort_key, id) < (cursor.sort, cursor.id)` (DESC sıralamada).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    pub sort: SortKey,
    pub id: i64,
}

/// Cursor kodlama/çözme sırasında oluşan hatalar. Hiçbiri panik değildir:
/// istemciden gelen bozuk, kurcalanmış ya da eski bir cursor bu tiplerden
/// biriyle geri döner.
#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("cursor base64url olarak çözülemedi: {0}")]
    Base64(String),

    #[error("cursor gövdesi ayrıştırılamadı: {0}")]
    Malformed(String),

    #[error("cursor imzası doğrulanamadı")]
    InvalidSignature,

    #[error("cursor farklı bir sıralama için üretilmiş")]
    SortMismatch,

    #[error("cursor sürümü desteklenmiyor")]
    UnsupportedVersion,
}

impl From<CursorError> for Error {
    fn from(_: CursorError) -> Self {
        // Sebep istemciye asla detaylandırılmıyor (bkz. error.rs
        // `public_detail`): hangi doğrulama adımının başarısız olduğu
        // (base64 mi, imza mı, sürüm mü) saldırgana bilgi sızdırabilir.
        // Detay yalnızca burada, iç `CursorError` içinde loglanabilir.
        Self::InvalidCursor
    }
}

/// HMAC-SHA256 ile cursor imzalayıp doğrulayan kod çözücü.
///
/// Global mutable state yok: anahtar bu struct içinde açıkça taşınır,
/// `SecurityConfig`'ten (ya da testte doğrudan bir string'ten) üretilir.
pub struct CursorCodec {
    key: Vec<u8>,
}

impl CursorCodec {
    /// Verilen anahtardan bir codec üretir.
    ///
    /// Anahtar HMAC-SHA256 için kullanılır; HMAC tasarımı gereği herhangi bir
    /// uzunluktaki anahtarı kabul eder (kısa anahtarlar sağdan sıfırla
    /// doldurulur, blok boyutunu aşanlar önce hash'lenir) — bu yüzden burada
    /// bir uzunluk doğrulaması yok, o `SecurityConfig`/`Config::validate`
    /// düzeyinde yapılır.
    #[must_use]
    pub fn new(key: &str) -> Self {
        Self {
            key: key.as_bytes().to_vec(),
        }
    }

    /// Bir cursor'ı imzalı, URL'de güvenle taşınabilir bir string'e kodlar.
    ///
    /// Çıktı yalnızca base64url alfabesindeki karakterleri içerir (`+`, `/`,
    /// `=` yok — no-pad kodlama), yani doğrudan bir sorgu string'i parametresi
    /// olarak ek kaçışlamaya gerek kalmadan kullanılabilir.
    #[must_use]
    pub fn encode(&self, cursor: &Cursor) -> String {
        let body = self.encode_body(cursor);
        let tag = self.sign(&body);

        let mut raw = Vec::with_capacity(TOTAL_LEN);
        raw.extend_from_slice(&body);
        raw.extend_from_slice(&tag);
        URL_SAFE_NO_PAD.encode(raw)
    }

    /// Kodlanmış bir cursor string'ini çözer ve doğrular.
    ///
    /// `expected`, çağıranın o an sayfaladığı sıralama türüdür. Cursor bu
    /// türden değilse (kullanıcı sıralamayı değiştirmiş demektir)
    /// [`CursorError::SortMismatch`] döner — bkz. modül dokümantasyonu.
    ///
    /// Doğrulama sırası bilinçli: önce uzunluk denetlenir, sonra **imza sabit
    /// zamanlı doğrulanır**, ancak imza geçtikten sonra gövdenin geri kalanı
    /// (sürüm, sıralama türü, değerler) yorumlanır. Doğrulanmamış baytlara
    /// asla güvenilmez.
    ///
    /// # Errors
    /// Girdi base64url değilse, uzunluğu tutmuyorsa, imza doğrulanamıyorsa,
    /// sürüm desteklenmiyorsa ya da sıralama türü `expected` ile
    /// uyuşmuyorsa.
    pub fn decode(&self, raw: &str, expected: SortKind) -> Result<Cursor, CursorError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|e| CursorError::Base64(e.to_string()))?;

        if bytes.len() != TOTAL_LEN {
            return Err(CursorError::Malformed(format!(
                "beklenen uzunluk {TOTAL_LEN} bayt, gelen {} bayt",
                bytes.len()
            )));
        }

        let (body, tag) = bytes.split_at(BODY_LEN);
        self.verify(body, tag)?;

        // Bu noktadan sonra `body`nin bizim ürettiğimiz, kurcalanmamış bir
        // gövde olduğu imza ile kanıtlandı; artık içeriğini yorumlayabiliriz.
        let version = body[0];
        if version != CURSOR_VERSION {
            return Err(CursorError::UnsupportedVersion);
        }

        let sort_byte = body[1];
        if sort_byte != expected.discriminant() {
            return Err(CursorError::SortMismatch);
        }

        // `body.len() == BODY_LEN` yukarıda garanti edildiği için bu
        // dilimlerin genişliği her zaman tutar; `try_into` panik atmaz,
        // hata döner (yine de burada asla `Err` olmaz).
        let value_bytes: [u8; 8] = body[2..10]
            .try_into()
            .map_err(|_| CursorError::Malformed("değer alanı 8 bayt olmalı".to_owned()))?;
        let id_bytes: [u8; 8] = body[10..18]
            .try_into()
            .map_err(|_| CursorError::Malformed("id alanı 8 bayt olmalı".to_owned()))?;
        let id = i64::from_be_bytes(id_bytes);

        let sort = match expected {
            SortKind::New => {
                let micros = i64::from_be_bytes(value_bytes);
                let created_at =
                    DateTime::<Utc>::from_timestamp_micros(micros).ok_or_else(|| {
                        CursorError::Malformed(format!("geçersiz created_at mikrosaniye: {micros}"))
                    })?;
                SortKey::New { created_at }
            }
            SortKind::Top => {
                // Kodlama sırasında i32 -> i64 işaret genişletmesiyle
                // saklandı (bkz. `encode_body`); geri dönüş kaybı olmayan
                // bir daraltmadır çünkü i32'nin tüm değer aralığı i64
                // içinde tam temsil edilir.
                let score = i64::from_be_bytes(value_bytes) as i32;
                SortKey::Top { score }
            }
            SortKind::Hot => {
                let bits = u64::from_be_bytes(value_bytes);
                SortKey::Hot {
                    hot_score: f64::from_bits(bits),
                }
            }
        };

        Ok(Cursor { sort, id })
    }

    /// Gövdeyi sabit uzunlukta baytlara paketler (bkz. modül dokümantasyonu
    /// "Gövde kodlaması" bölümü).
    fn encode_body(&self, cursor: &Cursor) -> [u8; BODY_LEN] {
        let mut body = [0u8; BODY_LEN];
        body[0] = CURSOR_VERSION;
        body[1] = cursor.sort.kind().discriminant();

        let value_bytes: [u8; 8] = match cursor.sort {
            SortKey::New { created_at } => created_at.timestamp_micros().to_be_bytes(),
            // i32 -> i64 işaret genişletmesi: MIN/MAX dahil kayıpsız.
            SortKey::Top { score } => i64::from(score).to_be_bytes(),
            SortKey::Hot { hot_score } => hot_score.to_bits().to_be_bytes(),
        };
        body[2..10].copy_from_slice(&value_bytes);
        body[10..18].copy_from_slice(&cursor.id.to_be_bytes());

        body
    }

    fn sign(&self, body: &[u8]) -> [u8; HMAC_TAG_LEN] {
        let mut mac = new_mac(&self.key);
        mac.update(body);
        let computed = mac.finalize().into_bytes();

        let mut tag = [0u8; HMAC_TAG_LEN];
        tag.copy_from_slice(&computed);
        tag
    }

    /// Sabit zamanlı imza doğrulaması.
    ///
    /// `hmac::Mac::verify_slice`, beklenen ve hesaplanan imzayı `==` ile
    /// karşılaştırmak yerine sabit zamanlı karşılaştırma kullanır — aksi
    /// halde ilk farklı bayta kadar geçen sürenin farkı, imzayı bayt bayt
    /// tahmin etmeye (timing attack) açık bir kanal oluştururdu.
    fn verify(&self, body: &[u8], tag: &[u8]) -> Result<(), CursorError> {
        let mut mac = new_mac(&self.key);
        mac.update(body);
        mac.verify_slice(tag)
            .map_err(|_| CursorError::InvalidSignature)
    }
}

/// `Hmac::<Sha256>::new_from_slice`'ı sarmalar.
///
/// Bu çağrı asla başarısız olmaz: HMAC tasarımı gereği herhangi bir
/// uzunluktaki anahtarı kabul eder (kısa anahtarlar sağdan sıfırla
/// doldurulur, blok boyutunu aşanlar önce hash'lenir) — `hmac` crate'inin
/// kendi `KeyInit::new` implementasyonu da aynı çağrıyı dahili olarak
/// `.expect(...)` ile sarar (bkz. `hmac::HmacCore::new`). `clippy::expect_used`
/// bunu statik olarak bilemediği için burada tek noktada, gerekçesiyle
/// birlikte izin veriyoruz.
#[allow(clippy::expect_used)]
fn new_mac(key: &[u8]) -> Hmac<Sha256> {
    Hmac::<Sha256>::new_from_slice(key).expect("HMAC herhangi bir anahtar uzunluğunu kabul eder")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &str = "test-anahtarı-en-az-32-karakter-uzunluğunda-01234567890";

    fn codec() -> CursorCodec {
        CursorCodec::new(TEST_KEY)
    }

    #[test]
    fn new_sıralaması_round_trip_ediyor_mikrosaniye_dahil() {
        let codec = codec();
        // `from_timestamp_micros` ile üretiyoruz ki mikrosaniyeden daha
        // hassas (nanosaniye) bir bileşen round-trip'i bozmasın.
        let created_at = DateTime::<Utc>::from_timestamp_micros(1_700_000_123_456_789)
            .expect("geçerli zaman damgası");
        let original = Cursor {
            sort: SortKey::New { created_at },
            id: 42,
        };

        let encoded = codec.encode(&original);
        let decoded = codec
            .decode(&encoded, SortKind::New)
            .expect("geçerli cursor çözülmeli");

        assert_eq!(decoded, original);
        if let SortKey::New { created_at: got } = decoded.sort {
            assert_eq!(got.timestamp_micros(), created_at.timestamp_micros());
        } else {
            panic!("beklenen sıralama türü New");
        }
    }

    #[test]
    fn top_sıralaması_round_trip_ediyor_negatif_skor_dahil() {
        let codec = codec();
        for score in [0, 1, -1, 12345, i32::MIN, i32::MAX] {
            let original = Cursor {
                sort: SortKey::Top { score },
                id: 7,
            };
            let encoded = codec.encode(&original);
            let decoded = codec
                .decode(&encoded, SortKind::Top)
                .expect("geçerli cursor çözülmeli");
            assert_eq!(decoded, original, "score={score} için round-trip bozuldu");
        }
    }

    #[test]
    fn hot_sıralaması_round_trip_ediyor() {
        let codec = codec();
        for hot_score in [0.0, -1.0, 12.3456789, f64::MIN, f64::MAX, -0.000_001] {
            let original = Cursor {
                sort: SortKey::Hot { hot_score },
                id: 999,
            };
            let encoded = codec.encode(&original);
            let decoded = codec
                .decode(&encoded, SortKind::Hot)
                .expect("geçerli cursor çözülmeli");
            assert_eq!(
                decoded, original,
                "hot_score={hot_score} için round-trip bozuldu"
            );
        }
    }

    #[test]
    fn kurcalanmış_cursor_reddediliyor() {
        let codec = codec();
        let original = Cursor {
            sort: SortKey::Top { score: 100 },
            id: 55,
        };
        let encoded = codec.encode(&original);
        let chars: Vec<char> = encoded.chars().collect();

        // Birkaç farklı konumda tek bir karakteri değiştir: baş, orta, son.
        for &pos in &[0usize, chars.len() / 2, chars.len() - 1] {
            let mut tampered = chars.clone();
            // '+' ve '/' base64url alfabesinde yok, farklı bir karakter
            // olduğundan emin oluyoruz.
            tampered[pos] = if tampered[pos] == 'A' { 'B' } else { 'A' };
            let tampered_str: String = tampered.into_iter().collect();

            let result = codec.decode(&tampered_str, SortKind::Top);
            assert!(
                result.is_err(),
                "pozisyon {pos}'daki kurcalama fark edilmeli"
            );
        }
    }

    #[test]
    fn kısaltılmış_cursor_reddediliyor() {
        let codec = codec();
        let original = Cursor {
            sort: SortKey::New {
                created_at: Utc::now(),
            },
            id: 1,
        };
        let encoded = codec.encode(&original);

        for cut in [1, 4, 10] {
            let shortened = &encoded[..encoded.len() - cut];
            assert!(codec.decode(shortened, SortKind::New).is_err());
        }
    }

    #[test]
    fn yanlış_anahtarla_üretilmiş_cursor_reddediliyor() {
        let codec_a = CursorCodec::new("anahtar-a-en-az-32-karakter-uzunluğunda-0123456789");
        let codec_b = CursorCodec::new("anahtar-b-en-az-32-karakter-uzunluğunda-9876543210");

        let original = Cursor {
            sort: SortKey::Hot { hot_score: 1.5 },
            id: 3,
        };
        let encoded = codec_a.encode(&original);

        assert!(codec_b.decode(&encoded, SortKind::Hot).is_err());
    }

    #[test]
    fn sıralama_türü_değişince_cursor_reddediliyor() {
        let codec = codec();
        let original = Cursor {
            sort: SortKey::New {
                created_at: Utc::now(),
            },
            id: 10,
        };
        let encoded = codec.encode(&original);

        // `New` ile kodlanan bir cursor `Top` beklenerek çözülmeye
        // çalışılınca reddedilmeli — farklı sıralamada anlamı olmayan bir
        // konuma sessizce atlamak, sessiz veri kaybı gibi görünürdü.
        let result = codec.decode(&encoded, SortKind::Top);
        assert!(matches!(result, Err(CursorError::SortMismatch)));

        // Doğru türle sorunsuz çözülebiliyor (kontrol).
        assert!(codec.decode(&encoded, SortKind::New).is_ok());
    }

    #[test]
    fn boş_ve_bozuk_girdiler_panik_atmadan_hata_dönüyor() {
        let codec = codec();

        assert!(codec.decode("", SortKind::New).is_err());
        assert!(codec.decode("!!!not-base64!!!", SortKind::New).is_err());
        assert!(codec.decode("+++///===", SortKind::New).is_err());
        // Base64url olarak geçerli ama boyutu tutmayan, çok uzun bir girdi.
        let too_long = "A".repeat(10_000);
        assert!(codec.decode(&too_long, SortKind::New).is_err());
        // Base64url olarak geçerli ama gövde uzunluğu yanlış, kısa bir girdi.
        assert!(codec.decode("QQ", SortKind::New).is_err());
    }

    #[test]
    fn sınır_id_değerleri_round_trip_ediyor() {
        let codec = codec();
        for id in [0i64, -1, 1, i64::MIN, i64::MAX] {
            let original = Cursor {
                sort: SortKey::Top { score: 0 },
                id,
            };
            let encoded = codec.encode(&original);
            let decoded = codec
                .decode(&encoded, SortKind::Top)
                .expect("geçerli cursor çözülmeli");
            assert_eq!(decoded.id, id, "id={id} için round-trip bozuldu");
        }
    }

    #[test]
    fn üretilen_cursor_url_güvenli_alfabeden_oluşuyor() {
        let codec = codec();
        // Birden çok farklı değerle üretip taşınabilirliği (`+`, `/`, `=`
        // yokluğunu) doğruluyoruz — bunlar URL query string'inde ek
        // kaçışlama gerektirirdi.
        for i in 0..200i64 {
            let cursor = Cursor {
                sort: SortKey::Hot {
                    hot_score: (i as f64) * 1.23456,
                },
                id: i,
            };
            let encoded = codec.encode(&cursor);
            assert!(
                !encoded.contains('+') && !encoded.contains('/') && !encoded.contains('='),
                "cursor URL-güvensiz karakter içeriyor: {encoded}"
            );
            assert!(
                encoded
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            );
        }
    }

    #[test]
    fn sürüm_baytı_uyuşmazsa_reddediliyor() {
        let codec = codec();
        let original = Cursor {
            sort: SortKey::Top { score: 1 },
            id: 1,
        };
        let encoded = codec.encode(&original);
        let mut bytes = URL_SAFE_NO_PAD
            .decode(&encoded)
            .expect("kendi ürettiğimiz cursor çözülebilmeli");

        // Sürüm baytını bozup imzayı yeniden hesaplayarak "gelecekteki bir
        // sürüm" senaryosunu simüle ediyoruz: imza geçerli ama sürüm
        // tanınmıyor.
        bytes[0] = CURSOR_VERSION.wrapping_add(1);
        let body = &bytes[..BODY_LEN];
        let tag = codec.sign(body);
        let mut re_signed = body.to_vec();
        re_signed.extend_from_slice(&tag);
        let re_encoded = URL_SAFE_NO_PAD.encode(re_signed);

        assert!(matches!(
            codec.decode(&re_encoded, SortKind::Top),
            Err(CursorError::UnsupportedVersion)
        ));
    }
}
