//! Kimlik doğrulamanın domain katmanı: kayıt, API key doğrulama, key
//! yönetimi, kurtarma kodları ve rol atama.
//!
//! HTTP'yi bilmez — `Authorization` header'ını ayrıştırmak, middleware
//! kurmak, rota tanımlamak taşıma katmanının (`actos-api`) işi. Burada
//! sadece [`authenticate`] ham bir `actos_...` string'i alır, geri kalan her
//! fonksiyon zaten kimliği doğrulanmış bir `actor_id` ile çalışır.
//!
//! Sır üretimi/doğrulaması [`crate::secret`]'e, format doğrulaması
//! [`crate::text`]'e bırakılır; bu modül yalnızca onları veritabanı
//! satırlarıyla birleştirir.

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    error::{Error, Result},
    secret, text,
};

/// [`register`] ve [`regenerate_recovery_codes`] tarafından üretilen
/// kurtarma kodu sayısı.
pub const RECOVERY_CODE_COUNT: usize = 10;

// --- Veri tipleri -----------------------------------------------------------

/// `actors` tablosundan okunan bir satır.
#[derive(Debug, Clone)]
pub struct ActorRecord {
    pub id: i64,
    pub username: String,
    pub actor_type: ActorType,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Güven kademesi (0-2) — bkz. `migrations/0020_trust_levels.up.sql`
    /// ve `crate::actor::recompute_trust_levels`. Yeni satırlarda şema
    /// varsayılanı `0`'dır; bu tip yalnızca okuma tarafında, hesaplama bu
    /// modülde YAPILMAZ.
    pub trust_level: i16,
}

/// `migrations/0002_actors.up.sql` → `actor_type` Postgres enum'ının Rust
/// karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "actor_type", rename_all = "snake_case")]
pub enum ActorType {
    Human,
    AiAgent,
    SystemBot,
    Organization,
}

/// `migrations/0012_admin_roles.up.sql` → `admin_role` Postgres enum'ının
/// Rust karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "admin_role", rename_all = "snake_case")]
pub enum AdminRole {
    Admin,
    Moderator,
}

/// `api_keys` tablosundan okunan bir satır. **`secret_hash` kasıtlı olarak
/// yok** — bu tip her zaman istemciye dönebilecek bir yanıtın parçası
/// olacağı için, sırrın hash'ini bile taşımaması bilinçli bir tasarım.
#[derive(Debug, Clone)]
pub struct ApiKeyRecord {
    pub id: Uuid,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// [`authenticate`] çağrısının başarılı sonucu — middleware'in
/// `Extension`/`Request` üzerinden taşıyacağı kimlik.
#[derive(Debug, Clone)]
pub struct AuthenticatedActor {
    pub actor: ActorRecord,
    pub key_id: Uuid,
    pub roles: Vec<AdminRole>,
    /// Actor'ün aktif bir ban'i var mı.
    ///
    /// **Ban kimlik doğrulamayı düşürmüyor**, yalnızca işaretleniyor:
    /// PLAN.md Faz 14'ün kararı "banlı actor yazamaz ama okuyabilir".
    /// Doğrulamayı burada reddetseydik banlı bir kullanıcı kendi
    /// kayıtlarını (`GET /me/saves`) ya da profilini bile göremezdi —
    /// ceza yazmaya yönelik, okumaya değil.
    ///
    /// Yazma engelini uygulayan yer `actos-api`'deki `CurrentActor`
    /// extractor'ı: güvenli olmayan HTTP metotlarında bu bayrağa bakıp
    /// `403` döndürüyor, böylece kural handler başına elle yazılmıyor.
    pub banned: bool,
}

/// [`register`] çağrısının sonucu. `api_key` ve `recovery_codes` **ham**
/// (plaintext) değerlerdir ve bir daha hiçbir yerde saklanmaz — çağıran bu
/// struct'ı bir kez kullanıp atmalı.
#[derive(Debug, Clone)]
pub struct Registration {
    pub actor: ActorRecord,
    pub api_key: String,
    pub recovery_codes: Vec<String>,
}

// --- Kayıt --------------------------------------------------------------

/// Yeni bir actor kaydeder: actor satırı + ilk API key + 10 kurtarma kodu,
/// hepsi **tek transaction'da**.
///
/// Biri başarısız olursa (ör. kurtarma kodlarından biri yazılırken bağlantı
/// koparsa) hiçbiri kalıcı olmaz — yarım bir actor'ün (key'i veya kurtarma
/// kodu olmayan) ortaya çıkmasını istemiyoruz.
///
/// # Errors
/// Kullanıcı adı veya görünen ad doğrulamadan geçmezse
/// [`Error::Validation`]; kullanıcı adı zaten alınmışsa [`Error::Conflict`];
/// veritabanı hatası [`Error::Database`].
pub async fn register(
    pool: &PgPool,
    username: &str,
    actor_type: ActorType,
    display_name: Option<&str>,
) -> Result<Registration> {
    let username =
        text::validate_username(username).map_err(|e| Error::Validation(e.to_string()))?;
    let display_name = display_name
        .map(text::validate_display_name)
        .transpose()
        .map_err(|e| Error::Validation(e.to_string()))?;

    // Sırlar DB işleminden önce üretilir: üretim saf CPU işi, transaction'ı
    // (ve dolayısıyla tuttuğu satır kilitlerini) gereksiz uzatmayalım.
    let generated_key = secret::generate_api_key();
    let recovery_codes = secret::generate_recovery_codes(RECOVERY_CODE_COUNT)
        .map_err(|e| Error::Internal(format!("kurtarma kodları üretilemedi: {e}")))?;

    let mut tx = pool.begin().await?;

    let insert_result = sqlx::query_as!(
        ActorRecord,
        r#"
        INSERT INTO actors (username, actor_type, display_name)
        VALUES ($1, $2, $3)
        RETURNING id, username, actor_type AS "actor_type: ActorType", display_name, bio, created_at, trust_level
        "#,
        username.as_str(),
        actor_type,
        display_name.as_deref(),
    )
    .fetch_one(&mut *tx)
    .await;

    let actor = match insert_result {
        Ok(actor) => actor,
        // `actors.username` üzerindeki UNIQUE kısıtı (bkz.
        // migrations/0002_actors.up.sql). citext olduğu için büyük/küçük
        // harf farkı gözetmeksizin çakışmayı yakalar.
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            return Err(Error::Conflict(format!(
                "\"{username}\" kullanıcı adı zaten alınmış"
            )));
        }
        Err(e) => return Err(Error::from(e)),
    };

    sqlx::query!(
        r#"
        INSERT INTO api_keys (id, actor_id, secret_hash, label)
        VALUES ($1, $2, $3, $4)
        "#,
        generated_key.key_id,
        actor.id,
        generated_key.secret_hash,
        None::<&str>,
    )
    .execute(&mut *tx)
    .await?;

    for code in &recovery_codes {
        sqlx::query!(
            r#"
            INSERT INTO recovery_codes (actor_id, code_hash)
            VALUES ($1, $2)
            "#,
            actor.id,
            code.hash,
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(Registration {
        actor,
        api_key: generated_key.plaintext,
        recovery_codes: recovery_codes.into_iter().map(|c| c.plaintext).collect(),
    })
}

// --- Doğrulama (en sık çalışan yol) -----------------------------------------

/// `authenticate` sorgusunun ham satırı. Ayrı bir struct tutuyoruz çünkü
/// `AuthenticatedActor`'ın alan şekli (nested `ActorRecord`, `roles` ayrı
/// sorgudan) SQL satırının şekliyle birebir örtüşmüyor.
struct AuthRow {
    secret_hash: String,
    key_revoked_at: Option<DateTime<Utc>>,
    actor_id: i64,
    username: String,
    actor_type: ActorType,
    display_name: Option<String>,
    bio: Option<String>,
    created_at: DateTime<Utc>,
    trust_level: i16,
    deleted_at: Option<DateTime<Utc>>,
    is_banned: bool,
}

/// Ham bir `actos_<key_id>_<secret>` string'ini doğrular ve kimliği yükler.
///
/// Bu, sistemde **en sık çalışacak** fonksiyon (her istekte bir kez) — bu
/// yüzden iki sorguya sıkıştırılmış: actor + key + ban tek bir JOIN'li
/// sorguda, roller ayrı (çoğu actor'ün rolü yok, ama olsa bile tek satır).
///
/// **Hata ayrımı sızdırmaz:** key bulunamadı, iptal edilmiş, secret yanlış
/// ve actor silinmiş — dördü de aynı [`Error::InvalidKey`]'i döner.
/// Saldırgan hangi aşamada takıldığını anlayamaz. Ban ise ayrı bir durum:
/// kimlik bilgisi (key_id + doğru secret) zaten kanıtlanmış bir kullanıcıya,
/// hesabının neden çalışmadığını söylemek meşru bir bilgi sızıntısı değil —
/// bu yüzden ban kontrolü **secret doğrulamasından sonra** yapılır (bir
/// saldırgan sadece geçerli bir `key_id` tahmin ederek "bu hesap banlı mı"
/// diye sormasın diye).
///
/// # Errors
/// Key bozuksa, bulunamazsa, iptal edilmişse, secret yanlışsa veya actor
/// silinmişse [`Error::InvalidKey`]; actor banlıysa (ban süresi dolmamışsa)
/// [`Error::Banned`]; veritabanı hatası [`Error::Database`].
pub async fn authenticate(pool: &PgPool, raw_key: &str) -> Result<AuthenticatedActor> {
    let parsed = secret::parse_api_key(raw_key).map_err(|_| Error::InvalidKey)?;

    // Tek sorgu: key + actor + ban durumu. `is_banned` hesaplaması SQL'de
    // yapılıyor (`now()` karşılaştırması Postgres tarafında) — uygulama
    // sunucusunun saati ile veritabanının saati arasında sürüklenme
    // (clock skew) olsa bile tutarlı kalır.
    let row = sqlx::query_as!(
        AuthRow,
        r#"
        SELECT
            api_keys.secret_hash,
            api_keys.revoked_at AS key_revoked_at,
            actors.id AS actor_id,
            actors.username,
            actors.actor_type AS "actor_type: ActorType",
            actors.display_name,
            actors.bio,
            actors.created_at,
            actors.trust_level,
            actors.deleted_at,
            (
                bans.actor_id IS NOT NULL
                AND (bans.expires_at IS NULL OR bans.expires_at > now())
            ) AS "is_banned!"
        FROM api_keys
        JOIN actors ON actors.id = api_keys.actor_id
        LEFT JOIN bans ON bans.actor_id = actors.id
        WHERE api_keys.id = $1
        "#,
        parsed.key_id,
    )
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Err(Error::InvalidKey);
    };

    if row.key_revoked_at.is_some() {
        return Err(Error::InvalidKey);
    }

    if !secret::verify_api_secret(&parsed.secret_bytes, &row.secret_hash) {
        return Err(Error::InvalidKey);
    }

    if row.deleted_at.is_some() {
        return Err(Error::InvalidKey);
    }

    // Ban burada hata üretmiyor, aşağıda `banned` alanına taşınıyor —
    // gerekçe `AuthenticatedActor::banned` üzerinde.

    let roles = sqlx::query_scalar!(
        r#"SELECT role AS "role: AdminRole" FROM admin_roles WHERE actor_id = $1"#,
        row.actor_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(AuthenticatedActor {
        actor: ActorRecord {
            id: row.actor_id,
            username: row.username,
            actor_type: row.actor_type,
            display_name: row.display_name,
            bio: row.bio,
            created_at: row.created_at,
            trust_level: row.trust_level,
        },
        key_id: parsed.key_id,
        roles,
        banned: row.is_banned,
    })
}

/// `last_used_at`'i günceller — "kritik olmayan" bir yan etki.
///
/// **Kısıtlı yazma:** yalnızca son güncellemenin üzerinden en az bir dakika
/// geçmişse `UPDATE` çalışır (`WHERE` koşulunun bir parçası). Aksi halde
/// yoğun bir actor'ün her isteği bir yazma tetikler, bu da `api_keys`
/// tablosunu (ve indexlerini) gereksiz yere ısıtır. Faz 6'da bu tamamen
/// kaldırılıp Redis'te biriktirilecek, periyodik bir job toplu `UPDATE`
/// atacak.
///
/// Hata döndürmez: `last_used_at` istekle ilgili kritik bir bilgi değil,
/// güncellenemedi diye isteği düşürmenin bir anlamı yok — sadece loglanır.
pub async fn touch_key(pool: &PgPool, key_id: Uuid) {
    let result = sqlx::query!(
        r#"
        UPDATE api_keys
        SET last_used_at = now()
        WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')
        "#,
        key_id,
    )
    .execute(pool)
    .await;

    if let Err(err) = result {
        tracing::warn!(key_id = %key_id, error = %err, "api_keys.last_used_at güncellenemedi");
    }
}

// --- Key yönetimi ------------------------------------------------------

/// Bir actor için yeni bir API key üretir.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`] (ör. `actor_id` mevcut değilse
/// yabancı anahtar ihlali).
pub async fn issue_key(
    pool: &PgPool,
    actor_id: i64,
    label: Option<&str>,
) -> Result<(ApiKeyRecord, String)> {
    let generated = secret::generate_api_key();

    let record = sqlx::query_as!(
        ApiKeyRecord,
        r#"
        INSERT INTO api_keys (id, actor_id, secret_hash, label)
        VALUES ($1, $2, $3, $4)
        RETURNING id, label, created_at, last_used_at, revoked_at
        "#,
        generated.key_id,
        actor_id,
        generated.secret_hash,
        label,
    )
    .fetch_one(pool)
    .await?;

    Ok((record, generated.plaintext))
}

/// Bir actor'ün tüm key'lerini listeler (iptal edilmişler dahil, secret asla
/// dönmez). En yeni önce.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn list_keys(pool: &PgPool, actor_id: i64) -> Result<Vec<ApiKeyRecord>> {
    let records = sqlx::query_as!(
        ApiKeyRecord,
        r#"
        SELECT id, label, created_at, last_used_at, revoked_at
        FROM api_keys
        WHERE actor_id = $1
        ORDER BY created_at DESC
        "#,
        actor_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(records)
}

/// Bir API key'i iptal eder.
///
/// **Sadece kendi key'ini iptal edebilir:** `actor_id` eşleşmezse
/// [`Error::NotFound`] döner — [`Error::Forbidden`] değil. `Forbidden`
/// döndürmek "bu key var ama senin değil"i doğrulamış olurdu; bir actor
/// başka birinin key `id`'lerini bu şekilde keşfedebilirdi (aynı actor'ın
/// key sayısını, ilk oluşturma zamanını vb. sızdıran bir yan kanal).
/// `NotFound`, key'in kendi actor'üne göre hiç var olmadığını söyler.
///
/// **İdempotent:** zaten iptal edilmiş bir key'i tekrar iptal etmeye
/// çalışmak hata değildir, no-op'tur.
///
/// # Errors
/// Key bu actor'e ait değilse veya hiç yoksa [`Error::NotFound`];
/// veritabanı hatası [`Error::Database`].
pub async fn revoke_key(pool: &PgPool, actor_id: i64, key_id: Uuid) -> Result<()> {
    let revoked = sqlx::query!(
        r#"
        UPDATE api_keys
        SET revoked_at = now()
        WHERE id = $1 AND actor_id = $2 AND revoked_at IS NULL
        RETURNING id
        "#,
        key_id,
        actor_id,
    )
    .fetch_optional(pool)
    .await?;

    if revoked.is_some() {
        return Ok(());
    }

    // UPDATE hiçbir satırı etkilemedi: ya key bu actor'e ait değil/hiç yok
    // (NotFound), ya da zaten iptal edilmişti (idempotent — hata değil).
    // İkisini ayırt etmek için ayrı bir varlık sorgusu atıyoruz; revoke
    // sıcak bir yol değil (kullanıcı başına nadiren, elle çağrılır), ekstra
    // sorgunun bedeli önemsiz.
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM api_keys WHERE id = $1 AND actor_id = $2) AS "exists!""#,
        key_id,
        actor_id,
    )
    .fetch_one(pool)
    .await?;

    if exists {
        Ok(())
    } else {
        Err(Error::NotFound("API key"))
    }
}

// --- Kurtarma ------------------------------------------------------------

/// [`verify_fixed_slots`] içinde, gerçek bir kurtarma kodu karşılığı
/// olmayan slotları doldurmak için kullanılan sabit, hiçbir kullanıcıya
/// ait olmayan bir Argon2id hash'i.
///
/// **Neden gerekli — bu, kullanıcı adı enumeration'ı için DEĞİL:**
/// kullanıcı adlarının var olup olmadığı bu platformda zaten public bilgi
/// (`GET /actors/{username}` profil döner, `GET /actors?type=...` bir
/// keşif dizinidir — bkz. `PLAN.md` Faz 7). Burada gizlenmeye çalışılan
/// şey **hesapta kaç kullanılmamış kurtarma kodu kaldığı**: yalnızca
/// gerçekten var olan kod sayısı kadar Argon2 doğrulaması yapılsaydı,
/// yanlış bir kod denemesinin süresi bu sayıyı sızdırırdı — 1 kodu kalan
/// bir hesapla 10 kodu duran bir hesap belirgin şekilde farklı sürede
/// yanıt verirdi. "Bu hesap son kurtarma kodunda" bilgisi saldırgan için
/// gerçek bir sinyaldir: hesabın kırılgan olduğunu ve yakın zamanda
/// kurtarma yapıldığını gösterir. Aynı sebeple kullanıcı bulunamadığında
/// da (kullanılmamış kod sayısı "0" kabul edilerek) tüm slotlar bu
/// hash'le doldurulur — kullanıcının var olup olmadığını gizlemek için
/// değil, "var olan bir hesabın hiç kodu kalmamış" durumuyla aynı
/// zamanlama profilini korumak için (bkz. [`verify_fixed_slots`]).
///
/// Süreç başına bir kez, ilk kullanımda üretilir (`LazyLock`) — her
/// `recover` çağrısında yeniden hash'lemenin (Argon2, pahalı) bir anlamı
/// yok, önemli olan her boş slot için bir `verify` çalıştırmak.
static DUMMY_RECOVERY_HASH: LazyLock<String> = LazyLock::new(|| {
    // `generate_recovery_codes`, yalnızca Argon2 parametreleri geçersizse
    // hata döner. `Argon2::default()` (bkz. secret.rs) sabit ve her zaman
    // geçerli olduğu için burada pratikte asla `Err` dönmez — `cursor.rs`
    // ve `id.rs`'teki "yapısal olarak başarısız olamaz" gerekçesiyle aynı
    // desen: tek noktada, gerekçesiyle birlikte `expect` izinli.
    #[allow(clippy::expect_used)]
    let mut codes = secret::generate_recovery_codes(1)
        .expect("Argon2::default() sabit parametreleriyle hash'leme başarısız olmaz");
    codes.remove(0).hash
});

thread_local! {
    /// Bu iş parçacığında [`verify_fixed_slots`] tarafından şimdiye kadar
    /// çalıştırılan toplam Argon2 kurtarma kodu doğrulama sayısı (gerçek
    /// slotlar + dummy slotlar, hepsi dahil).
    ///
    /// Üretim davranışını etkilemez, yalnızca okunur. Tek amacı:
    /// `tests/auth.rs`'in "recover, kalan kod sayısından bağımsız olarak
    /// her zaman tam `RECOVERY_CODE_COUNT` doğrulama yapıyor mu" sorusunu
    /// zamanlama ölçüp gürültüyle uğraşmadan, sayaç okuyarak güvenilir
    /// şekilde test edebilmesi. `thread_local` seçildi (global `static`
    /// değil): `#[sqlx::test]` fonksiyonları paralel çalışabiliyor, iş
    /// parçacığı başına sayaç bu paralel testlerin birbirini kirletmesini
    /// engelliyor.
    static RECOVERY_VERIFICATION_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Yalnızca test/gözlemlenebilirlik amaçlı: bkz.
/// [`RECOVERY_VERIFICATION_COUNT`] üzerindeki yorum.
#[doc(hidden)]
#[must_use]
pub fn recovery_verification_count() -> usize {
    RECOVERY_VERIFICATION_COUNT.with(std::cell::Cell::get)
}

/// Yalnızca test/gözlemlenebilirlik amaçlı: bkz.
/// [`RECOVERY_VERIFICATION_COUNT`] üzerindeki yorum.
#[doc(hidden)]
pub fn reset_recovery_verification_count() {
    RECOVERY_VERIFICATION_COUNT.with(|c| c.set(0));
}

struct RecoveryLookupRow {
    actor_id: i64,
    is_banned: bool,
}

struct UnusedRecoveryCode {
    id: i64,
    code_hash: String,
}

/// `code`'u, kaç tanesi gerçek olursa olsun **tam olarak
/// [`RECOVERY_CODE_COUNT`] kez** Argon2 doğrulamasından geçirir ve
/// eşleşen varsa `recovery_codes.id`'sini döner.
///
/// `unused_codes` kısaysa (ya da kullanıcı hiç bulunamadığı için boşsa)
/// eksik slotlar [`DUMMY_RECOVERY_HASH`] ile doldurulur. Eşleşme
/// bulunduktan sonra da döngü **kırılmaz**: hem toplam doğrulama sayısı
/// hem de "eşleşme kaçıncı slotta bulundu" bilgisi zamanlamadan
/// okunabilir olmasın diye tüm slotlar her zaman gezilir. Her doğrulama
/// sonucu `std::hint::black_box`'tan geçiriliyor ki derleyici, eşleşme
/// zaten bulunmuşken kalan çağrıların sonucunun kullanılmadığını
/// düşünüp onları eleyemesin.
///
/// **Maliyet kabul edilebilir:** `/auth/recover` Faz 6'da IP başına
/// günde 5 istekle sınırlanacak (bkz. `PLAN.md`); bu hacimde, isteği
/// gecikten en fazla `RECOVERY_CODE_COUNT` (10) Argon2 doğrulaması hiç
/// sorun değil.
fn verify_fixed_slots(code: &str, unused_codes: &[UnusedRecoveryCode]) -> Option<i64> {
    let mut matched: Option<i64> = None;

    // Slot sayısı en az `RECOVERY_CODE_COUNT`, ama kullanılmamış kod sayısı
    // bir şekilde bunu aşarsa hepsini geziyoruz. Sabit 10'da dursaydık
    // 11. koddaki GEÇERLİ bir kurtarma kodu sessizce reddedilir ve kullanıcı
    // hesabından kilitlenirdi. Şu an bu durum oluşamaz (register 10 kod
    // yazıyor, regenerate eskileri geçersiz kılıyor), ama doğruluğu bu
    // değişmezin korunmasına bağlamak istemiyoruz: sızıntı riski
    // (kod sayısı 10'u aşarsa süre uzar) kilitlenme riskinden iyidir.
    let slots = RECOVERY_CODE_COUNT.max(unused_codes.len());

    for slot in 0..slots {
        let (hash, id): (&str, Option<i64>) = match unused_codes.get(slot) {
            Some(row) => (row.code_hash.as_str(), Some(row.id)),
            None => (DUMMY_RECOVERY_HASH.as_str(), None),
        };

        let ok = std::hint::black_box(secret::verify_recovery_code(code, hash));
        RECOVERY_VERIFICATION_COUNT.with(|c| c.set(c.get() + 1));

        if ok {
            // `matched` zaten doluysa üzerine yazmıyoruz (ilk eşleşme
            // kazanır) — ama bunun için döngüyü erken bitirmiyoruz.
            matched = matched.or(id);
        }
    }

    matched
}

/// Bir kurtarma koduyla yeni bir API key talep eder.
///
/// Dönüş: `(ham_yeni_key, kalan_kullanılmamış_kod_sayısı)`.
///
/// **Kalan kod sayısı zamanlamadan sızmaz:** doğrulama her zaman
/// [`verify_fixed_slots`] üzerinden, kullanıcının kaç kullanılmamış kodu
/// olduğuna bakılmaksızın tam olarak [`RECOVERY_CODE_COUNT`] Argon2
/// doğrulamasıyla yapılır — kullanıcı hiç bulunamadığında bile (bkz.
/// [`DUMMY_RECOVERY_HASH`] üzerindeki gerekçe: burada gizlenen
/// kullanıcının varlığı değil, hesabın kurtarma kodu bütçesi;
/// kullanıcı adları zaten public — `GET /actors/{username}`).
///
/// **Sıra bilinçli:** kod doğrulaması banlı olup olmadığından **önce**
/// yapılır — [`authenticate`]'teki gerekçenin aynısı: kimlik kanıtlanmadan
/// (doğru kod sunulmadan) hesabın banlı olduğunu söylemek, kanıtlanmamış
/// bir iddiaya (kod deneyen kişinin gerçekten o hesabın sahibi olduğuna)
/// meşru bilgi sızdırmak olur.
///
/// Eşleşen kod aynı transaction içinde `used_at` ile işaretlenir ve yeni
/// key üretilir — kod tek kullanımlıktır, `UPDATE ... WHERE used_at IS
/// NULL` yarış durumunda (aynı kod eşzamanlı iki istekte) ikinci isteğin
/// kaybetmesini garantiler.
///
/// # Errors
/// Kullanıcı yoksa, actor silinmişse veya kod yanlışsa/tükenmişse
/// [`Error::InvalidKey`]; actor banlıysa [`Error::Banned`]; veritabanı
/// hatası [`Error::Database`].
pub async fn recover(pool: &PgPool, username: &str, code: &str) -> Result<(String, i64)> {
    let normalized_username = text::normalize_text(username);

    // `deleted_at IS NULL`: soft-delete edilmiş bir actor kurtarma
    // YAPAMAZ — actors + deleted_at kuralı platformun her yerinde aynı
    // (silinmiş hesap işlem yapamaz). Bu filtrenin amacı kullanıcı
    // adının var olup olmadığını gizlemek DEĞİL: kullanıcı adları bu
    // platformda zaten public (`GET /actors/{username}`, `PLAN.md` Faz 7
    // keşif dizini). Zamanlama tarafında korunan bilgi — kullanıcı
    // bulunamasa da bulunsa da aynı sayıda Argon2 doğrulaması yapılması
    // — aşağıda [`verify_fixed_slots`] ile sağlanıyor.
    let found = sqlx::query_as!(
        RecoveryLookupRow,
        r#"
        SELECT
            actors.id AS actor_id,
            (
                bans.actor_id IS NOT NULL
                AND (bans.expires_at IS NULL OR bans.expires_at > now())
            ) AS "is_banned!"
        FROM actors
        LEFT JOIN bans ON bans.actor_id = actors.id
        WHERE actors.username = $1 AND actors.deleted_at IS NULL
        "#,
        normalized_username.as_str(),
    )
    .fetch_optional(pool)
    .await?;

    // Kullanıcı bulunamadıysa `unused_codes` boş kalır; `verify_fixed_slots`
    // bu durumda 10 slotun tamamını `DUMMY_RECOVERY_HASH` ile doldurur —
    // yani kullanıcı var/yok farkı burada da doğrulama sayısını değiştirmez.
    let unused_codes: Vec<UnusedRecoveryCode> = match &found {
        Some(row) => sqlx::query_as!(
            UnusedRecoveryCode,
            r#"SELECT id, code_hash FROM recovery_codes WHERE actor_id = $1 AND used_at IS NULL"#,
            row.actor_id,
        )
        .fetch_all(pool)
        .await?,
        None => Vec::new(),
    };

    let matched_id = verify_fixed_slots(code, &unused_codes);

    let (found, code_row_id) = match (found, matched_id) {
        (Some(found), Some(code_row_id)) => (found, code_row_id),
        _ => return Err(Error::InvalidKey),
    };

    if found.is_banned {
        return Err(Error::Banned);
    }

    let mut tx = pool.begin().await?;

    // `used_at IS NULL` koşulu yarış durumuna karşı: aynı kod eşzamanlı iki
    // `recover` çağrısında kullanılmaya çalışılırsa, ikincisi burada 0 satır
    // günceller ve InvalidKey alır.
    let consumed = sqlx::query!(
        r#"
        UPDATE recovery_codes
        SET used_at = now()
        WHERE id = $1 AND used_at IS NULL
        RETURNING id
        "#,
        code_row_id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    if consumed.is_none() {
        return Err(Error::InvalidKey);
    }

    let generated = secret::generate_api_key();

    sqlx::query!(
        r#"
        INSERT INTO api_keys (id, actor_id, secret_hash, label)
        VALUES ($1, $2, $3, $4)
        "#,
        generated.key_id,
        found.actor_id,
        generated.secret_hash,
        "kurtarma ile üretildi",
    )
    .execute(&mut *tx)
    .await?;

    let remaining = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM recovery_codes WHERE actor_id = $1 AND used_at IS NULL"#,
        found.actor_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok((generated.plaintext, remaining))
}

/// `code`'un `actor_id`'ye ait geçerli, kullanılmamış bir kurtarma kodu
/// olup olmadığını, [`verify_fixed_slots`] ile aynı zamanlama-güvenli
/// mekanizmayla doğrular. Eşleşirse `recovery_codes.id`'sini döner ama
/// **tüketmez** (`used_at` işaretlemez) — tüketme ayrı bir adım
/// ([`consume_recovery_code`]).
///
/// **Neden ikiye ayrıldı — [`recover`]'daki aynı desen:** buradaki iş
/// (Argon2 doğrulaması) pahalı bir CPU işi; bunu bir veritabanı
/// transaction'ı açıkken yapmak istemiyoruz (satır kilitlerini gereksiz
/// uzatır). Çağıran (`crate::actor::delete_account`) önce bunu
/// transaction'sız çağırıp kodu doğrular, eşleşme varsa ancak o zaman bir
/// transaction açıp [`consume_recovery_code`]'u ve diğer yazmaları
/// (hesabı işaretleme, key'leri iptal etme) o transaction içinde yapar.
///
/// **Ban kontrolü burada YOK:** [`recover`]'ın aksine bu fonksiyonun
/// çağıranı ([`crate::actor::delete_account`]) her zaman zaten kimliği
/// doğrulanmış (`authenticate()`'ten geçmiş) bir actor için çalışır —
/// `authenticate()` banlı actor'leri zaten reddediyor, burada tekrar
/// kontrol etmenin bir anlamı yok.
///
/// # Errors
/// Kod yanlış veya tükenmişse [`Error::InvalidKey`]; veritabanı hatası
/// [`Error::Database`].
pub async fn verify_recovery_code_for_actor(
    pool: &PgPool,
    actor_id: i64,
    code: &str,
) -> Result<i64> {
    let unused_codes: Vec<UnusedRecoveryCode> = sqlx::query_as!(
        UnusedRecoveryCode,
        r#"SELECT id, code_hash FROM recovery_codes WHERE actor_id = $1 AND used_at IS NULL"#,
        actor_id,
    )
    .fetch_all(pool)
    .await?;

    verify_fixed_slots(code, &unused_codes).ok_or(Error::InvalidKey)
}

/// [`verify_recovery_code_for_actor`] ile bulunan bir kodu, çağıranın kendi
/// transaction'ı içinde tüketir (`used_at = now()`).
///
/// `used_at IS NULL` koşulu, aynı kodun eşzamanlı iki istekte tüketilmeye
/// çalışılmasına karşı korur (bkz. [`recover`]'daki aynı gerekçe) — bu
/// arada başka bir istek kodu zaten tükettiyse burada `0` satır güncellenir
/// ve [`Error::InvalidKey`] dönülür.
///
/// # Errors
/// Kod bu arada başka bir istekte tüketildiyse [`Error::InvalidKey`];
/// veritabanı hatası [`Error::Database`].
pub async fn consume_recovery_code(tx: &mut sqlx::PgConnection, code_row_id: i64) -> Result<()> {
    let consumed = sqlx::query!(
        r#"
        UPDATE recovery_codes
        SET used_at = now()
        WHERE id = $1 AND used_at IS NULL
        RETURNING id
        "#,
        code_row_id,
    )
    .fetch_optional(tx)
    .await?;

    if consumed.is_none() {
        return Err(Error::InvalidKey);
    }

    Ok(())
}

/// Bir actor'ün tüm kullanılmamış kurtarma kodlarını geçersiz kılıp
/// [`RECOVERY_CODE_COUNT`] yenisini üretir. Tek transaction.
///
/// **Eski kodlar `used_at` ile işaretlenmek yerine SİLİNİR.** Gerekçe:
/// `used_at` sütununun anlamı "bu kod kurtarma için fiilen kullanıldı" —
/// yenileme sırasında iptal edilen bir kod hiç kullanılmadı, `used_at`
/// yazmak bu denetim izini yanlış temsil ederdi. Zaten kullanılmış
/// (`used_at` dolu) satırlara dokunulmaz; onlar geçmişin bir parçası.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`]; kod üretimi (pratikte hemen hiç
/// olmayan bir iç hata dışında) başarısız olursa [`Error::Internal`].
pub async fn regenerate_recovery_codes(pool: &PgPool, actor_id: i64) -> Result<Vec<String>> {
    let new_codes = secret::generate_recovery_codes(RECOVERY_CODE_COUNT)
        .map_err(|e| Error::Internal(format!("kurtarma kodları üretilemedi: {e}")))?;

    let mut tx = pool.begin().await?;

    sqlx::query!(
        r#"DELETE FROM recovery_codes WHERE actor_id = $1 AND used_at IS NULL"#,
        actor_id,
    )
    .execute(&mut *tx)
    .await?;

    for code in &new_codes {
        sqlx::query!(
            r#"INSERT INTO recovery_codes (actor_id, code_hash) VALUES ($1, $2)"#,
            actor_id,
            code.hash,
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(new_codes.into_iter().map(|c| c.plaintext).collect())
}

// --- Roller ----------------------------------------------------------------

/// Bir actor'e admin/moderatör rolü verir (ya da mevcut rolünü değiştirir).
///
/// `admin_roles.actor_id` PK olduğu için (bkz.
/// `migrations/0012_admin_roles.up.sql`) bir actor'ın en fazla bir rolü
/// olabilir; ikinci bir `grant_role` çağrısı hata vermez, rolü ve
/// `granted_by`/`granted_at`'i günceller (upsert).
///
/// # Errors
/// Veritabanı hatası [`Error::Database`] (ör. `actor_id` ya da
/// `granted_by` mevcut değilse yabancı anahtar ihlali).
pub async fn grant_role(
    pool: &PgPool,
    actor_id: i64,
    role: AdminRole,
    granted_by: Option<i64>,
) -> Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO admin_roles (actor_id, role, granted_by)
        VALUES ($1, $2, $3)
        ON CONFLICT (actor_id) DO UPDATE
        SET role = EXCLUDED.role, granted_by = EXCLUDED.granted_by, granted_at = now()
        "#,
        actor_id,
        role,
        granted_by,
    )
    .execute(pool)
    .await?;

    Ok(())
}
