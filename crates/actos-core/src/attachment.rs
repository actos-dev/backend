//! Yüklenen dosyalar: depolamaya yazma (aktör başına toplam depolama
//! kotasına tabi — Faz 18.A, bkz. NOTES.md §9.8 ve [`create_attachment`]),
//! içeriğe bağlama, silme ve bağlanmamış yüklemeleri toplayan periyodik iş.
//!
//! Görselin doğrulanması ve normalize edilmesi burada değil,
//! [`crate::media`]'da; bu modül onun çıktısını alıp depolama ile
//! veritabanını birlikte yönetiyor.
//!
//! ## Yükleme ile bağlama neden iki ayrı adım
//!
//! `migrations/0008_attachments.up.sql`'in kararı: `content_id` `NULL`
//! olabiliyor. İstemci önce dosyayı yükleyip bir id alıyor, sonra post'u
//! o id'lerle oluşturuyor. Alternatif (post ile dosyayı tek multipart
//! istekte göndermek) bir ajanı, gövdesini kurmadan önce dosyayı hazır
//! etmeye zorlardı ve yeniden denemeyi pahalılaştırırdı: post oluşturma
//! başarısız olursa dosya da baştan yüklenmek zorunda kalırdı.
//!
//! Bedeli, hiçbir içeriğe bağlanmayan yüklemeler — onları
//! [`cleanup_orphaned`] topluyor.
//!
//! ## `object_key` neden kullanıcı girdisi içermiyor
//!
//! Anahtar `<actor dış id>/<uuidv7>.webp` biçiminde üretiliyor; dosya adı,
//! uzantı ya da başka bir istemci girdisi **hiç kullanılmıyor**. Sebep yol
//! aşımı (path traversal): `../../` içeren bir dosya adı bucket'ta başka
//! bir yere yazabilirdi. Uzantı da sabit çünkü çıktı her zaman WebP.
//!
//! Actor'ün **dış** id'si kullanılıyor (ham `bigint` değil): anahtar
//! public-read bir bucket üzerinden URL olarak görünüyor, iç id'yi oraya
//! yazmak `crate::id`'nin bütün amacını boşa çıkarırdı.

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    error::{Error, Result},
    id::IdCodec,
    media::{self, ProcessedImage},
    storage::Storage,
};

/// Küçük önizlemenin anahtarı, asıl dosyanınkinden türetiliyor.
///
/// Şemada ikinci bir sütun yok ve eklenmedi: önizleme her zaman asıl
/// dosyanın yanında, deterministik bir adla duruyor. İstemci de aynı kuralı
/// uygulayarak URL'yi kendisi üretebilir.
const THUMBNAIL_SUFFIX: &str = ".thumb.webp";

/// [`cleanup_orphaned`]'in varsayılan yaş eşiği: bu kadar süredir hiçbir
/// içeriğe bağlanmamış yüklemeler siliniyor.
///
/// 24 saat (PLAN.md Faz 13): bir istemcinin dosyayı yükleyip post'u
/// oluşturması arasında geçmesi makul olan sürenin çok üstünde, yani
/// gerçekten kullanılacak bir dosyayı yanlışlıkla silme riski yok.
pub const ORPHAN_MAX_AGE_HOURS: i64 = 24;

/// [`cleanup_orphaned`]'in PostgreSQL advisory lock anahtarı.
///
/// `crate::tag` ve `crate::feed`'inkilerden farklı — üç iş birbirini
/// bekletmemeli.
const CLEANUP_ADVISORY_LOCK_KEY: i64 = 0x0AC7_0513;

/// Tek turda silinecek azami yetim yükleme sayısı.
///
/// Her satır için depolamaya iki ağ çağrısı gidiyor (asıl dosya +
/// önizleme); sınırsız bir tur, uzun süre kapalı kalmış bir dağıtımda
/// dakikalarca sürebilirdi. Kalanlar bir sonraki turda toplanıyor.
const CLEANUP_BATCH: i64 = 500;

/// Bir yükleme kaydı.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub id: i64,
    pub actor_id: i64,
    pub content_id: Option<i64>,
    pub object_key: String,
    pub byte_size: i64,
    pub mime_type: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub checksum_sha256: String,
    pub created_at: DateTime<Utc>,
}

impl Attachment {
    /// Küçük önizlemenin nesne anahtarı.
    #[must_use]
    pub fn thumbnail_key(&self) -> String {
        format!("{}{THUMBNAIL_SUFFIX}", self.object_key)
    }
}

/// `POST /uploads`: ham baytları doğrula, normalize et, **depolama kotasını
/// kontrol et**, depolamaya yaz, kaydı oluştur.
///
/// **Sıra bilinçli:** önce depolamaya yazılıyor, sonra veritabanı satırı
/// ekleniyor. Ters sırada, veritabanı satırı yazılıp depolama yüklemesi
/// başarısız olursa hiçbir nesnesi olmayan bir kayıt kalırdı ve o kayda
/// bağlanan bir post kırık bir görsel gösterirdi. Bu sırada ise en kötü
/// ihtimalle sahipsiz bir nesne kalıyor — görünmez, zararsız ve
/// [`cleanup_orphaned`]'in tarayabileceği bir çöp.
///
/// ## Storage quota (see NOTES.md §9.8)
///
/// `quota_bytes` is the flat **total** byte limit applied by the caller
/// (`actos-api::routes::uploads`) (see `crate::config::StorageQuotaConfig`).
/// This module has no idea how the quota is determined — it just enforces
/// the final number, exactly like `max_bytes` (the single-file limit)
/// already does. The check is done via [`total_storage_bytes`], which
/// reads `SUM(attachments.byte_size)` and adds the bytes this upload would
/// contribute; **it wasn't turned into a counter column** — at this scale
/// (hundreds/thousands of rows per actor, already bounded by the quota
/// itself) a `SUM` query on every upload isn't a measurable cost, whereas
/// a separate counter column would require manually guaranteeing its
/// consistency (correct increment/decrement on every path, including
/// delete/undo) with no payoff at this scale. If scale grows (at a
/// threshold similar to `docs/query-plans.md`), this decision should be
/// revisited.
///
/// Kontrol **normalize edilmiş** (WebP) boyut üzerinden, `media::
/// process_image`'ın çıktısı hazır olduktan ama depolamaya hiç yazılmadan
/// önce yapılıyor: (1) `attachments.byte_size`'ın kendisi bu değeri
/// tutuyor, yani kota tam bu sütunun toplamıyla tutarlı olmalı — ham
/// yükleme boyutuyla kontrol etseydik WebP sıkıştırması sonrası gerçek
/// kullanım kotayla uyuşmayabilirdi; (2) normalize etme yerel/CPU-bağımlı
/// (ağ çağrısı yok), reddedilecek bir yükleme için bu adımı çalıştırmanın
/// maliyeti `storage.put_object`'in ağ üzerinden S3'e yazmasından çok daha
/// ucuz — asıl pahalı adımdan (depolamaya yazma) önce durmak, sonrasında
/// durup nesneyi geri silmekten (ki bu ek bir başarısızlık noktası daha
/// açardı) daha basit ve daha az riskli.
///
/// # Errors
/// Dosya doğrulamadan geçmezse [`Error::Validation`] /
/// [`Error::UnsupportedMedia`] (bkz. [`crate::media::process_image`]);
/// **kota aşılırsa [`Error::Validation`]** (bkz. aşağıdaki gerekçe — neden
/// `Forbidden` değil); depolama erişilemezse [`Error::Internal`]; veritabanı
/// hatası [`Error::Database`].
///
/// **Neden `Validation`, `Forbidden` değil:** bu kod tabanında `Forbidden`
/// bir *yetki/sahiplik* ihlalini işaret ediyor (bkz. [`resolve_as_avatar`] —
/// "bu senin değil"), kotanın anlamı bu değil; actor'ün yükleme *yetkisi*
/// hâlâ var, yalnızca şu anki *isteği* (bu boyutta, bu anda) mevcut
/// durumuyla (kullanımı) çakışıyor. Bu tam olarak [`Error::Validation`]'ın
/// `max_bytes`/görsel format kontrolleri için zaten kullandığı aile: girdi
/// biçimsel olarak geçerli ama bağlamıyla (kota) birlikte kabul edilemez.
/// `Conflict` (409) de düşünülebilirdi ama o bu kod tabanında "kaynağın şu
/// anki durumu" (ör. zaten bağlı bir ek) için ayrılmış (bkz.
/// [`resolve_as_avatar`]); burada çakışan kaynağın kendisi değil, isteğin
/// hacmi — `400 Validation` daha doğru. Mesaj kullanıcının **ne kadar
/// kullandığını ve sınırın ne olduğunu** taşıyor (bkz. aşağıdaki
/// `format!`) — yalnızca "kota doldu" demek, istemcinin (özellikle bir
/// ajanın, bu platformda birinci sınıf vatandaş) bir sonraki adımı
/// planlamasına (silmeli mi, ne kadar yer açmalı) yetmezdi.
pub async fn create_attachment(
    pool: &PgPool,
    storage: &Storage,
    id_codec: &IdCodec,
    actor_id: i64,
    bytes: &[u8],
    max_bytes: usize,
    quota_bytes: i64,
) -> Result<Attachment> {
    let islenmis: ProcessedImage = media::process_image(bytes, max_bytes)?;

    let object_key = object_key_uret(id_codec, actor_id)?;
    let thumbnail_key = format!("{object_key}{THUMBNAIL_SUFFIX}");

    // Checksum **normalize edilmiş** çıktının, yüklenen ham dosyanın değil:
    // saklanan şey bu, bütünlük doğrulaması da bunun üzerinden anlamlı.
    let checksum = sha256_hex(&islenmis.data);
    let byte_size = i64::try_from(islenmis.data.len())
        .map_err(|_| Error::Internal("file size does not fit in i64".to_owned()))?;

    let mevcut_kullanim = total_storage_bytes(pool, actor_id).await?;
    // Taşma savunması: `mevcut_kullanim` ve `byte_size` ayrı ayrı makul
    // (SUM zaten var olan satırlardan, `byte_size` tek bir görselden) ama
    // toplamları `i64::checked_add` olmadan teorik olarak taşabilir —
    // `saturating_add` en kötü ihtimalle kotayı "kesin aşılmış" sayar,
    // asla sessizce izin vermez.
    if mevcut_kullanim.saturating_add(byte_size) > quota_bytes {
        return Err(Error::Validation(format!(
            "storage quota exceeded: you are currently using {mevcut_kullanim} bytes, \
             your quota limit is {quota_bytes} bytes, and this upload would add {byte_size} \
             more bytes — you may need to delete some of your attachments first"
        )));
    }

    storage
        .put_object(&object_key, islenmis.data, ProcessedImage::mime_type())
        .await?;

    // Önizlemenin başarısız olması yüklemeyi düşürmüyor: asıl dosya zaten
    // yazıldı ve kayıt onun üzerinden anlamlı. Önizleme bir kolaylık.
    if let Err(err) = storage
        .put_object(
            &thumbnail_key,
            islenmis.thumbnail,
            ProcessedImage::mime_type(),
        )
        .await
    {
        tracing::warn!(object_key = %object_key, error = %err, "önizleme yüklenemedi");
    }

    let width = i32::try_from(islenmis.width).ok();
    let height = i32::try_from(islenmis.height).ok();

    let row = sqlx::query!(
        r#"
        INSERT INTO attachments
            (actor_id, object_key, byte_size, mime_type, width, height, checksum_sha256)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, created_at
        "#,
        actor_id,
        object_key,
        byte_size,
        ProcessedImage::mime_type(),
        width,
        height,
        checksum,
    )
    .fetch_one(pool)
    .await?;

    Ok(Attachment {
        id: row.id,
        actor_id,
        content_id: None,
        object_key,
        byte_size,
        mime_type: ProcessedImage::mime_type().to_owned(),
        width,
        height,
        checksum_sha256: checksum,
        created_at: row.created_at,
    })
}

/// Bir actor'ün **tüm** yüklemelerinin toplam bayt kullanımı —
/// [`create_attachment`]'ın kota kontrolünün ve `DELETE /uploads/{id}`'in
/// (bkz. [`delete_attachment`]) kotayı serbest bırakmasının tek doğruluk
/// kaynağı.
///
/// **Silinen ekler otomatik düşer:** ayrı bir "azalt" adımı yok çünkü hiç
/// gerekmiyor — `SUM(byte_size)`, sorgu her çalıştığında `attachments`
/// tablosunun **o anki** satırlarını topluyor; [`delete_attachment`]'ın
/// `DELETE FROM attachments` çağrısı satırı kaldırdığı an bir sonraki `SUM`
/// onu zaten görmüyor. Bir sayaç kolonu olsaydı bu senkronu (silme yolunun
/// hepsinde doğru azaltma) elle korumak gerekirdi; `SUM` bunu bedavaya
/// veriyor (bkz. [`create_attachment`] üzerindeki "sayaç kolonuna
/// çevrilmedi" gerekçesi).
///
/// `COALESCE(..., 0)`: actor'ün hiç yüklemesi yoksa `SUM` SQL'de `NULL`
/// döner (boş küme üzerinde toplam tanımsız) — `0`'a çeviriyoruz ki
/// çağıranın `Option` ile uğraşmasına gerek kalmasın, "kullanım yok" ile
/// "kullanım sıfır bayt" burada eşdeğer. `::bigint` cast'i sqlx'in `SUM`
/// çıktısını (Postgres'te `numeric`, `byte_size` `bigint` olsa bile)
/// `i64`'e güvenle eşlemesi için — `crate::actor`'daki `total_score!`
/// deseniyle aynı (bkz. `ProfileRow` sorgusu).
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn total_storage_bytes(pool: &PgPool, actor_id: i64) -> Result<i64> {
    let toplam = sqlx::query_scalar!(
        r#"
        SELECT COALESCE(SUM(byte_size), 0)::bigint AS "toplam!"
        FROM attachments
        WHERE actor_id = $1
        "#,
        actor_id,
    )
    .fetch_one(pool)
    .await?;

    Ok(toplam)
}

/// `<actor dış id>/<uuidv7>.webp`.
///
/// UUIDv7 zaman sıralı: aynı actor'ün yüklemeleri bucket listelemesinde
/// kronolojik görünüyor, ve rastgele bir v4'ün aksine tahmin edilebilir bir
/// sıra vermiyor (zaman damgası dışındaki bitler rastgele).
fn object_key_uret(id_codec: &IdCodec, actor_id: i64) -> Result<String> {
    let dis_id = id_codec.encode::<crate::id::Actor>(actor_id)?;
    Ok(format!("{dis_id}/{}.webp", Uuid::now_v7()))
}

/// Baytların SHA-256'sı, hex.
///
/// `ck_attachments_checksum_sha256_format` şemada 64 karakterlik küçük harf
/// hex bekliyor; `hex::encode` tam bunu üretiyor.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Yüklemeleri bir içeriğe bağlar.
///
/// **Aynı transaction'da çağrılmalı** (post/yorum oluşturma ile birlikte):
/// içerik yazılıp ekler bağlanmadan bir hata olursa, eklerin yetim kalması
/// yerine bütün işlem geri alınmalı.
///
/// Yalnızca çağıranın **kendi** ve **henüz bağlanmamış** yüklemeleri
/// bağlanabilir. Başkasının yüklemesini kendi post'una iliştirmek ya da bir
/// dosyayı iki içeriğe birden bağlamak `WHERE` koşuluyla engelleniyor; satır
/// sayısı tutmazsa hata dönüyor — sessizce daha az ek bağlamak, istemcinin
/// gönderdiğinden farklı bir post yaratmak olurdu.
///
/// # Errors
/// İstenen eklerden biri yoksa, başkasına aitse ya da zaten bağlıysa
/// [`Error::Validation`]; veritabanı hatası [`Error::Database`].
pub async fn attach_to_content(
    tx: &mut PgConnection,
    content_id: i64,
    actor_id: i64,
    attachment_ids: &[i64],
) -> Result<()> {
    if attachment_ids.is_empty() {
        return Ok(());
    }

    let sonuc = sqlx::query!(
        r#"
        UPDATE attachments
        SET content_id = $1
        WHERE id = ANY($2::bigint[])
          AND actor_id = $3
          AND content_id IS NULL
        "#,
        content_id,
        attachment_ids,
        actor_id,
    )
    .execute(&mut *tx)
    .await?;

    let beklenen = u64::try_from(attachment_ids.len()).unwrap_or(u64::MAX);
    if sonuc.rows_affected() != beklenen {
        return Err(Error::Validation(
            "one of the attachments was not found, does not belong to you, or is already \
             attached to a content item"
                .to_owned(),
        ));
    }

    Ok(())
}

/// `PATCH /actors/me`'nin `avatar` alanı için: bir ekin **çağıranın kendi**
/// ve **henüz bir içeriğe bağlanmamış** bir yüklemesi olduğunu doğrular,
/// doğrularsa `object_key`'ini döner — bu değer doğrudan
/// `actors.avatar_object_key`'e yazılır (bkz. `crate::actor::update_profile`).
///
/// **Neden burada, `crate::actor`'de değil:** bu, [`attach_to_content`] ile
/// aynı aile — ikisi de `attachments` tablosunun "sahiplik + henüz
/// bağlanmamışlık" kuralını uyguluyor, tek fark hedefin bir içerik değil
/// `actors.avatar_object_key` olması. `attachments`'a dair kurallar bu
/// modülde toplu kalsın diye `actor.rs`'e taşınmadı.
///
/// **`FOR UPDATE` ile satır kilitleniyor:** çağıran zaten bir transaction
/// içinde ([`crate::actor::update_profile`]) — kilit, bu fonksiyonun
/// `SELECT`'i ile çağıranın asıl `UPDATE actors ...`'ı arasındaki küçük
/// pencerede aynı ekin eşzamanlı bir [`attach_to_content`] çağrısıyla bir
/// içeriğe bağlanmasını engelliyor. Kilit yalnızca çağıranın transaction'ı
/// commit/rollback olana kadar tutulur — [`attach_to_content`] de kendi
/// transaction'ı içinde çalıştığı için burada çıkmaza (deadlock) yol açmaz,
/// yalnızca kısa bir bekleme olur.
///
/// **`content_id IS NOT NULL` neden `404`/`403` değil `409`:** istek
/// biçimsel olarak geçerli ve ek gerçekten var/çağırana ait — sorun
/// kaynağın (attachment satırının) **şu anki durumunun** istenen işlemle
/// çakışması (zaten başka bir yaşam döngüsüne, bir içeriğe, girmiş). Bu tam
/// olarak HTTP `409 Conflict`'in tanımı; `400 Validation` istekteki
/// biçimsel bir hata olduğunda daha doğru olurdu (id formatı bozuk gibi),
/// burada öyle değil.
///
/// # Errors
/// Ek yoksa [`Error::NotFound`]; çağırana ait değilse [`Error::Forbidden`];
/// zaten bir içeriğe bağlıysa [`Error::Conflict`]; veritabanı hatası
/// [`Error::Database`].
pub async fn resolve_as_avatar(
    tx: &mut PgConnection,
    attachment_id: i64,
    actor_id: i64,
) -> Result<String> {
    let kayit = sqlx::query!(
        r#"SELECT actor_id, content_id, object_key FROM attachments WHERE id = $1 FOR UPDATE"#,
        attachment_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("attachment"))?;

    if kayit.actor_id != actor_id {
        return Err(Error::Forbidden);
    }

    if kayit.content_id.is_some() {
        return Err(Error::Conflict(
            "this attachment is already attached to a content item and cannot be used as an \
             avatar"
                .to_owned(),
        ));
    }

    Ok(kayit.object_key)
}

/// Bir içeriğe bağlı ekleri döner.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn list_for_content(pool: &PgPool, content_id: i64) -> Result<Vec<Attachment>> {
    let rows = sqlx::query_as!(
        Attachment,
        r#"
        SELECT id, actor_id, content_id, object_key, byte_size, mime_type,
               width, height, checksum_sha256, created_at
        FROM attachments
        WHERE content_id = $1
        ORDER BY id
        "#,
        content_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// `DELETE /uploads/{id}`: yalnızca sahibi.
///
/// **Sıra:** önce veritabanı satırı siliniyor, sonra depolamadaki nesne.
/// [`create_attachment`]'ın tersi ve aynı gerekçeyle: aradaki bir hatada
/// kalan şey sahipsiz bir nesne olsun (görünmez, temizlenebilir), hiçbir
/// nesnesi olmayan bir kayıt değil (kırık görsel gösterirdi).
///
/// # Errors
/// Ek yoksa [`Error::NotFound`]; çağıranın değilse [`Error::Forbidden`];
/// veritabanı hatası [`Error::Database`].
pub async fn delete_attachment(
    pool: &PgPool,
    storage: &Storage,
    id: i64,
    actor_id: i64,
) -> Result<()> {
    let kayit = sqlx::query!(
        r#"SELECT actor_id, object_key FROM attachments WHERE id = $1"#,
        id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("attachment"))?;

    if kayit.actor_id != actor_id {
        return Err(Error::Forbidden);
    }

    sqlx::query!(r#"DELETE FROM attachments WHERE id = $1"#, id)
        .execute(pool)
        .await?;

    nesneleri_sil(storage, &kayit.object_key).await;

    Ok(())
}

/// Bir ekin asıl dosyasını ve önizlemesini depolamadan siler.
///
/// Hata **yutuluyor, loglanıyor**: veritabanı satırı zaten gitti, çağıranın
/// işlemi başarılı sayılmalı. Kalan nesne görünmez bir çöp ve S3'te var
/// olmayan bir anahtarı silmek zaten başarılı sayıldığı için tekrar denemek
/// de güvenli.
async fn nesneleri_sil(storage: &Storage, object_key: &str) {
    if let Err(err) = storage.delete_object(object_key).await {
        tracing::warn!(object_key = %object_key, error = %err, "nesne silinemedi");
    }
    let thumb = format!("{object_key}{THUMBNAIL_SUFFIX}");
    if let Err(err) = storage.delete_object(&thumb).await {
        tracing::warn!(object_key = %thumb, error = %err, "önizleme silinemedi");
    }
}

/// [`ORPHAN_MAX_AGE_HOURS`] saatten uzun süredir hiçbir içeriğe bağlanmamış
/// yüklemeleri siler; silinen sayıyı döner.
///
/// `crate::tag::cleanup_unused` ve `crate::feed::recompute_hot_scores` ile
/// aynı advisory lock deseni, ayrı anahtarla.
///
/// `idx_attachments_orphaned` (kısmi index, `WHERE content_id IS NULL`)
/// tam bu sorgu için var — bkz. `migrations/0008_attachments.up.sql`.
///
/// **⚠️ Avatar olarak kullanılan ekler bilerek dışlanıyor.** Bir avatar
/// hiçbir zaman bir içeriğe bağlanmaz — `crate::actor::update_profile` onu
/// yalnızca `actors.avatar_object_key`'e yazar, `attachments.content_id`
/// hep `NULL` kalır (bkz. [`resolve_as_avatar`]). Bu filtre olmadan, bir
/// actor avatarını ayarladıktan [`ORPHAN_MAX_AGE_HOURS`] saat sonra bu iş
/// onu "hiçbir içeriğe bağlanmamış yükleme" sanıp **hem depolamadan hem
/// veritabanından siler** — avatar sessizce kırık bir bağlantıya döner, ne
/// istemciye ne loga bir hata düşer (silme başarıyla tamamlanır, sadece
/// yanlış satırı hedef alır). Dışlama `NOT EXISTS` ile: `attachments.
/// object_key`'i `actors.avatar_object_key`'e eşit olan hiçbir satır
/// (yaşı ne olursa olsun) bu iş tarafından adaya alınmaz.
///
/// Performans notu: `actors` üzerinde `avatar_object_key`'e bir index yok
/// (bu görev bir migration eklemedi); Postgres `NOT EXISTS`'i tipik olarak
/// tek bir anti-join'e çeviriyor (satır başına ayrı bir tarama değil), yani
/// bugünkü ölçekte sorun değil — `actors` tablosu büyüdükçe `avatar_object_key
/// IS NOT NULL` üzerinde kısmi bir index eklemek gerekebilir.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn cleanup_orphaned(pool: &PgPool, storage: &Storage) -> Result<u64> {
    let mut conn = pool.acquire().await?;

    let locked = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_lock($1) AS "locked!""#,
        CLEANUP_ADVISORY_LOCK_KEY,
    )
    .fetch_one(&mut *conn)
    .await?;

    if !locked {
        tracing::debug!("yetim yükleme temizliği başka bir instance'da çalışıyor, tur atlandı");
        return Ok(0);
    }

    let esik = Utc::now() - Duration::hours(ORPHAN_MAX_AGE_HOURS);

    // Satırlar `RETURNING` ile alınıyor: nesneleri de silmek için
    // anahtarlara ihtiyaç var. Veritabanı önce temizleniyor (bkz.
    // `delete_attachment`'taki aynı sıra gerekçesi).
    let silinenler = sqlx::query!(
        r#"
        DELETE FROM attachments
        WHERE id IN (
            SELECT attachments.id
            FROM attachments
            WHERE attachments.content_id IS NULL
              AND attachments.created_at < $1
              AND NOT EXISTS (
                  SELECT 1 FROM actors
                  WHERE actors.avatar_object_key = attachments.object_key
              )
            ORDER BY attachments.created_at
            LIMIT $2
        )
        RETURNING object_key
        "#,
        esik,
        CLEANUP_BATCH,
    )
    .fetch_all(&mut *conn)
    .await;

    if let Err(err) = sqlx::query_scalar!(
        r#"SELECT pg_advisory_unlock($1) AS "unlocked!""#,
        CLEANUP_ADVISORY_LOCK_KEY,
    )
    .fetch_one(&mut *conn)
    .await
    {
        tracing::warn!(error = %err, "yetim yükleme temizliği advisory lock'ı bırakılamadı");
    }

    let silinenler = silinenler?;

    for satir in &silinenler {
        nesneleri_sil(storage, &satir.object_key).await;
    }

    let sayi = u64::try_from(silinenler.len()).unwrap_or(u64::MAX);
    if sayi > 0 {
        tracing::info!(sayi, "bağlanmamış yüklemeler temizlendi");
    }

    Ok(sayi)
}

/// Eklerin herkese açık URL'leri.
///
/// Bucket public-read olduğu için (bkz. PLAN.md Faz 13; ileride private +
/// presigned URL) nesne anahtarından doğrudan URL üretiliyor, imzalama
/// adımı yok.
#[must_use]
pub fn public_urls(storage: &Storage, ekler: &[Attachment]) -> Vec<String> {
    ekler
        .iter()
        .map(|ek| storage.public_url(&ek.object_key))
        .collect()
}
