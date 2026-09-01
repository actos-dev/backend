//! `Idempotency-Key` desteği: aynı anahtarla tekrarlanan bir yazma isteği
//! yeni bir kayıt üretmez, ilk isteğin sonucunu aynen döndürür.
//!
//! HTTP'yi bilmez — `Idempotency-Key` header'ını okumak, dönen kararı HTTP
//! yanıtına çevirmek (`201` mi `409` mu, `Location` header'ı ne olacak)
//! `actos-api/src/routes/posts.rs`'in işi (bkz. `crate::ratelimit` ile aynı
//! katman ayrımı — orada da HTTP'ye çeviren middleware ayrı bir dosyada).
//! Bu modül yalnızca Redis'e dokunan kısmı taşıyor; görev tanımının "Redis'e
//! dokunduğu için `actos-core`'a ait" gerekçesi budur.
//!
//! ## Anahtar kapsamı: `actor_id` + istemcinin `key`'i
//!
//! Redis anahtarı `{prefix}idem:{actor_id}:{key}` biçiminde — `actor_id`
//! **kasıtlı olarak** anahtarın parçası. Yalnızca istemcinin gönderdiği ham
//! `key`'i kullansaydık, iki farklı actor aynı `Idempotency-Key` değerini
//! (ör. her ikisi de aynı istemci kütüphanesinin ürettiği "1" gibi zayıf bir
//! değer) gönderirse ikinci actor'ün isteği birincinin sakladığı sonucu
//! (birinin post'unu) geri alırdı — bu yalnızca yanlış davranış değil, bir
//! actor'ün başka bir actor'ün oluşturduğu kaydı kendi isteğinin yanıtıymış
//! gibi görmesi anlamına gelen bir güvenlik sınırı ihlali. `actor_id` içeri
//! alındığında bu çakışma yapısal olarak imkânsız hâle geliyor.
//!
//! ## Eşzamanlı çift istek: atomik yer tutucu
//!
//! [`IdempotencyStore::begin`] `SET key value NX EX` (tek atomik Redis
//! komutu, `redis::SetOptions` ile) kullanarak bir "beklemede" kaydı koymayı
//! dener. İki eşzamanlı istek aynı anda `begin` çağırırsa yalnızca biri `NX`
//! koşulunu geçer ([`Begin::Start`] alır, isteği gerçekten işlemekten
//! sorumludur); diğeri anahtarın zaten var olduğunu görür ve mevcut kaydı
//! okur. O kayıt hâlâ "beklemede" ise (birinci istek henüz
//! [`IdempotencyStore::complete`] çağırmadı) [`Begin::InProgress`] döner.
//!
//! **`InProgress` için `409 Conflict` kararı `actos-api` katmanında
//! veriliyor** ama gerekçesi burada: `202 Accepted` gibi bir "sonra tekrar
//! sor" yanıtı istemciyi polling yapmaya zorlardı (bu uç senkron, polling
//! için tasarlanmadı); `200`/`201` ile "sanki bitmiş gibi" boş bir gövde
//! dönmek yanlış bilgi verirdi. `409` en azından doğru sinyali taşıyor:
//! "bu anahtarla bir şey zaten oluyor, şu an değil" — istemci kısa bir
//! bekleme sonrası tekrar deneyebilir.
//!
//! ## Redis erişilemezse: burada bilinçli olarak **fail-open**
//!
//! `crate::ratelimit` modülündeki kuralla karşılaştırın: orada yazma
//! scope'ları (post/comment/vote/...) **fail-closed** — Redis yokken
//! sınırsız yazmaya izin vermemek için istek tamamen reddedilir. Burada
//! **tam tersi** bir karar veriliyor ve bu bilinçli:
//!
//! - Hız sınırlamada fail-closed'ın bedeli "bir süre yazamama"dır ama
//!   önlediği şey sınırsız spam/DoS — kritik bir tehdit.
//! - Burada fail-closed'ın bedeli aynı ("bir süre post atamama") ama
//!   önlediği şey yalnızca **nadir, kurtarılabilir** bir sonuç: aynı isteği
//!   tekrarlayan bir ajanın (Redis'in tam da bu birkaç saniyelik penceresinde
//!   çöktüğü ender durumda) iki post oluşturması. Bu bir güvenlik ihlali
//!   değil, `DELETE /posts/{id}` ile geri alınabilir bir tekrar.
//!
//! `Idempotency-Key` isteğe bağlı bir güvenlik ağı, `POST /posts`'un temel
//! işlevi değil — bu isteğe bağlı kolaylığı, ana yazma yolunun kullanılabilirliğini
//! Redis'in tamamına bağımlı kılacak kadar pahalıya satmak istemiyoruz. Bu
//! yüzden [`IdempotencyStore::begin`] Redis'e **hiç ulaşılamadığında**
//! (havuzdan bağlantı alınamadı, komut hata döndü, `NX` sonrası `GET` boş
//! geldi — bkz. aşağıdaki yarış notu) [`Begin::Start`] döner — istek sanki
//! `Idempotency-Key` hiç gönderilmemiş gibi işlenir. Her durum
//! `tracing::warn!` ile loglanır.
//!
//! **Ayrım:** Redis'e ulaşılıp da orada bozuk/ayrıştırılamayan bir kayıt
//! bulunursa (bkz. `IdempotencyStore::inspect_existing`) bilerek
//! [`Begin::Start`] DEĞİL, [`Begin::InProgress`] dönülür — bu fail-open
//! değil. Fark şu: Redis'e hiç ulaşamamak "bu anahtar hakkında hiçbir bilgim
//! yok" demektir (fail-open güvenli), ama orada bir kayıt bulup da onu
//! yorumlayamamak "bu anahtarla bir şey olduğunu biliyorum ama ne olduğunu
//! bilmiyorum" demektir — bu durumda sıfırdan başlamak (`Start`) bir çift
//! yazmayı tetikleyebilir, oysa `InProgress` (`409`, istemci kısa süre sonra
//! tekrar dener) hiçbir zaman yanlışlıkla ikinci bir post oluşturmaz.
//!
//! [`IdempotencyStore::complete`] hata döndürmez — post zaten oluşturuldu,
//! sonucu kaydedemedik diye **başarılı** bir isteği düşürmenin hiçbir anlamı
//! yok; bir sonraki tekrarda yalnızca dedup koruması kaybolur, o kadar.
//!
//! ## TTL: 24 saat, `begin`'de başlar
//!
//! `SET ... NX EX 86400` yer tutucuyu koyarken TTL'i başlatır.
//! [`IdempotencyStore::complete`] sonucu yazarken `KEEPTTL` kullanır — TTL'i
//! sıfırlamaz. Yani kayıt, ilk isteğin geldiği andan itibaren tam 24 saat
//! yaşar, işlemin ne kadar sürdüğü (birkaç ms de olsa dakikalar da olsa)
//! bunu etkilemez.

use deadpool_redis::Pool;
use redis::{AsyncTypedCommands as _, ExistenceCheck, RedisResult, SetExpiry, SetOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Bir anahtarın Redis'te ne kadar yaşayacağı (bkz. modül dokümantasyonu
/// "TTL" bölümü). Görev tanımındaki "24 saat" gereksinimi.
const TTL_SECONDS: u64 = 24 * 60 * 60;

/// İstemcinin gönderebileceği azami `Idempotency-Key` uzunluğu. Bu bir
/// protokol kısıtı değil, savunmacı bir sınır: bozuk/kaçak bir ajanın
/// rastgele kilobaytlarca veriyi anahtar diye Redis'e yazmasını önler.
/// Gerçek kullanım (UUID, ULID) çok daha kısa.
const MAX_KEY_LEN: usize = 200;

/// Daha önce tamamlanmış bir isteğin saklanan sonucu — aynen tekrar
/// döndürülür.
#[derive(Debug, Clone)]
pub struct StoredResponse {
    pub status: u16,
    /// Yalnızca `Location` header'ı taşıyan yanıtlar için (`POST /posts`
    /// → `201` + `Location`). Yoksa `None`.
    pub location: Option<String>,
    pub body: JsonValue,
}

/// [`IdempotencyStore::begin`]'in dönebileceği üç durum.
#[derive(Debug)]
pub enum Begin {
    /// Bu anahtarla ilk istek (ya da önceki kayıt süresi dolmuş/hiç
    /// yoktu). Çağıran isteği işlemeli ve bitince [`IdempotencyStore::
    /// complete`] çağırmalı.
    Start,
    /// Aynı anahtarla başka bir istek şu anda işleniyor (yer tutucu var,
    /// sonuç henüz yok).
    InProgress,
    /// Bu anahtarla daha önce tamamlanmış bir istek var; saklanan sonuç
    /// aynen döndürülmeli.
    Completed(StoredResponse),
}

/// Redis'te saklanan ham kayıt biçimi.
///
/// Tek bir yapı iki durumu (`beklemede` / `tamamlandı`) taşıyor —
/// `done: false` iken `status`/`location`/`body` hep `None`, `done: true`
/// iken `status`/`body` hep `Some`. Ayrı bir enum yerine bu düz biçim
/// tercih edildi çünkü serde'nin dahili etiketli enum kodlaması (`tag`)
/// burada ekstra bir karmaşıklık katmadan aynı bilgiyi taşıyabilir; alan
/// sayısı zaten küçük.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body: Option<JsonValue>,
}

impl Record {
    const fn pending() -> Self {
        Self {
            done: false,
            status: None,
            location: None,
            body: None,
        }
    }

    fn done(response: &StoredResponse) -> Self {
        Self {
            done: true,
            status: Some(response.status),
            location: response.location.clone(),
            body: Some(response.body.clone()),
        }
    }

    /// `done: true` ve gerekli alanlar doluysa bir [`StoredResponse`]'a
    /// çevirir. `status`/`body` eksikse (elle kurcalanmış ya da bozuk bir
    /// kayıt) `None` döner — çağıran bunu modül dokümantasyonundaki
    /// fail-open kuralına göre ele alır.
    fn into_stored_response(self) -> Option<StoredResponse> {
        if !self.done {
            return None;
        }
        Some(StoredResponse {
            status: self.status?,
            location: self.location,
            body: self.body?,
        })
    }
}

/// Redis destekli idempotency deposu.
///
/// `RateLimiter` ile aynı desen: ucuz klonlanabilir olmak zorunda değil
/// (havuz zaten `Arc` tabanlı), çağıran taraf tek bir örneği paylaşabilir.
pub struct IdempotencyStore {
    pool: Pool,
    /// Bkz. `crate::ratelimit` "Redis anahtar şeması" bölümündeki aynı
    /// gerekçe: üretimde her zaman boş, yalnızca testler `with_prefix` ile
    /// paralel test binary'lerini aynı Redis'i paylaşırken izole etmek için
    /// doldurur.
    key_prefix: String,
}

impl IdempotencyStore {
    #[must_use]
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            key_prefix: String::new(),
        }
    }

    /// [`Self::new`] ile aynı, ama tüm Redis anahtarlarının başına
    /// `key_prefix` eklenir.
    ///
    /// **Yalnızca testler için** — bkz. `crate::ratelimit::RateLimiter::
    /// with_prefix` üzerindeki gerekçe, burada birebir aynı sorun/çözüm
    /// geçerli: paralel `cargo test` çalıştırmaları aynı gerçek Redis'i
    /// (`127.0.0.1:3102`) paylaşıyor, izole edilmiş bir test veritabanının
    /// aksine (`#[sqlx::test]`) Redis için otomatik bir izolasyon yok.
    #[must_use]
    pub fn with_prefix(pool: Pool, key_prefix: impl Into<String>) -> Self {
        Self {
            pool,
            key_prefix: key_prefix.into(),
        }
    }

    fn redis_key(&self, actor_id: i64, key: &str) -> String {
        format!("{}idem:{actor_id}:{key}", self.key_prefix)
    }

    /// Bir isteğin idempotency anahtarını "işleme al" ya da mevcut
    /// durumunu/sonucunu öğren.
    ///
    /// [`Begin::Start`] dönerse çağıran isteği gerçekten işlemeli ve
    /// bitince [`Self::complete`]'i çağırmalı — aksi hâlde yer tutucu 24
    /// saat "beklemede" kalır ve o süre boyunca aynı anahtarla gelen her
    /// istek [`Begin::InProgress`] görür (kalıcı bir kilitlenme değil, TTL
    /// dolunca kendiliğinden temizlenir, ama yine de çağıranın sorumluluğu).
    ///
    /// Redis'e hiç ulaşılamazsa ya da kayıt bozuksa [`Begin::Start`] döner
    /// (bkz. modül dokümantasyonu "fail-open" bölümü) — bu fonksiyon bu
    /// yüzden pratikte hiçbir zaman `Err` dönmez, tek istisna `key`'in
    /// kendisinin geçersiz olduğu (boş ya da çok uzun) durumdur, bu da bir
    /// Redis hatası değil bir istemci hatasıdır.
    ///
    /// # Errors
    /// `key` boşsa ya da [`MAX_KEY_LEN`]'i aşarsa [`crate::Error::
    /// Validation`].
    pub async fn begin(&self, actor_id: i64, key: &str) -> crate::Result<Begin> {
        if key.is_empty() || key.chars().count() > MAX_KEY_LEN {
            return Err(crate::Error::Validation(format!(
                "Idempotency-Key 1-{MAX_KEY_LEN} karakter arasında olmalı"
            )));
        }

        let redis_key = self.redis_key(actor_id, key);

        let mut conn = match self.pool.get().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "redis havuzundan bağlantı alınamadı, idempotency kontrolü atlanıyor (fail-open)",
                );
                return Ok(Begin::Start);
            }
        };

        let pending = match serde_json::to_string(&Record::pending()) {
            Ok(s) => s,
            Err(err) => {
                // Sabit, alanları hep `None` olan bir yapı — pratikte
                // asla başarısız olmaz; yine de panik yerine fail-open.
                tracing::warn!(error = %err, "idempotency yer tutucusu serialize edilemedi (fail-open)");
                return Ok(Begin::Start);
            }
        };

        let set_opts = SetOptions::default()
            .conditional_set(ExistenceCheck::NX)
            .with_expiration(SetExpiry::EX(TTL_SECONDS));

        let set_result: RedisResult<Option<String>> = conn
            .set_options(redis_key.as_str(), pending.as_str(), set_opts)
            .await;

        match set_result {
            // `NX` koşulunu biz geçtik: anahtar boştu, yer tutucuyu biz koyduk.
            Ok(Some(_)) => Ok(Begin::Start),
            // Anahtar zaten vardı (`NX` koşulu geçmedi) — mevcut kaydı oku.
            Ok(None) => Ok(self.inspect_existing(&mut conn, &redis_key).await),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "idempotency yer tutucusu redis'e yazılamadı (fail-open)",
                );
                Ok(Begin::Start)
            }
        }
    }

    /// [`Self::begin`]'in `NX` koşulu geçmediğinde (anahtar zaten vardı)
    /// mevcut kaydı okuyup yorumlar.
    async fn inspect_existing(
        &self,
        conn: &mut deadpool_redis::Connection,
        redis_key: &str,
    ) -> Begin {
        let existing: RedisResult<Option<String>> = conn.get(redis_key).await;

        let raw = match existing {
            Ok(Some(raw)) => raw,
            // Anahtar `SET NX` ile bizim aramızda (yarış) süresi dolup
            // silinmiş olabilir — bu artık "boş" demek, yeniden başlanır.
            Ok(None) => {
                tracing::warn!(
                    "idempotency anahtarı NX sonrası GET öncesi süresi doldu, yeni istek gibi işleniyor",
                );
                return Begin::Start;
            }
            Err(err) => {
                tracing::warn!(error = %err, "idempotency kaydı redis'ten okunamadı (fail-open)");
                return Begin::Start;
            }
        };

        match serde_json::from_str::<Record>(&raw) {
            Ok(record) => match record.into_stored_response() {
                Some(stored) => Begin::Completed(stored),
                // İki farklı durum aynı sonuca (`InProgress`) düşüyor: (1)
                // asıl beklenen durum — `done: false`, başka bir istek hâlâ
                // işliyor; (2) nadir durum — `done: true` ama alanları eksik
                // bozuk bir kayıt. İkinci durumda da fail-open'a (`Start`)
                // DÜŞMÜYORUZ, bilerek: kaydın kendisi (yer tutucu ya da
                // gövde) hâlâ orada, yani bu anahtarla bir şey oldu/oluyor;
                // "başka biri işliyor" varsayımı burada "bilgi kaybettik,
                // sıfırdan başla" varsayımından daha güvenli — olası bir
                // çift-yazmayı önler. Yalnızca kayda hiç ulaşılamadığında
                // (bağlantı/okuma hatası, aşağıdaki `Err` kolu) fail-open'a
                // düşülüyor.
                None => Begin::InProgress,
            },
            Err(err) => {
                tracing::warn!(error = %err, raw, "idempotency kaydı ayrıştırılamadı, işlemde kabul ediliyor");
                Begin::InProgress
            }
        }
    }

    /// [`Self::begin`]'in [`Begin::Start`] döndürdüğü bir isteğin sonucunu
    /// kaydeder — sonraki tekrarlar bunu [`Begin::Completed`] olarak görür.
    ///
    /// Hata döndürmez (bkz. modül dokümantasyonu): istek zaten başarıyla
    /// işlendi, sonucu kaydedemedik diye çağırana bunu bir hata gibi
    /// yansıtmanın anlamı yok — yalnızca bir sonraki tekrarın dedup
    /// koruması kaybolur.
    pub async fn complete(&self, actor_id: i64, key: &str, response: &StoredResponse) {
        let redis_key = self.redis_key(actor_id, key);

        let mut conn = match self.pool.get().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "redis bağlantısı alınamadı, idempotency sonucu kaydedilemedi",
                );
                return;
            }
        };

        let record = Record::done(response);
        let raw = match serde_json::to_string(&record) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, "idempotency sonucu serialize edilemedi");
                return;
            }
        };

        // `KEEPTTL`: `begin`'in koyduğu 24 saatlik süreyi sıfırlamıyoruz
        // (bkz. modül dokümantasyonu "TTL" bölümü). `NX`/`XX` koşulu yok —
        // bu noktaya yalnızca `begin` bize `Begin::Start` döndürdüyse
        // ulaşılır, yani yer tutucunun sahibi biziz; koşulsuz üzerine
        // yazmak güvenli.
        let opts = SetOptions::default().with_expiration(SetExpiry::KEEPTTL);
        let result: RedisResult<Option<String>> = conn
            .set_options(redis_key.as_str(), raw.as_str(), opts)
            .await;

        if let Err(err) = result {
            tracing::warn!(error = %err, "idempotency sonucu redis'e yazılamadı");
        }
    }
}
