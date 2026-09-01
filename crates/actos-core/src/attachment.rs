//! Yüklenen dosyalar: depolamaya yazma, içeriğe bağlama, silme ve
//! bağlanmamış yüklemeleri toplayan periyodik iş.
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

/// `POST /uploads`: ham baytları doğrula, normalize et, depolamaya yaz,
/// kaydı oluştur.
///
/// **Sıra bilinçli:** önce depolamaya yazılıyor, sonra veritabanı satırı
/// ekleniyor. Ters sırada, veritabanı satırı yazılıp depolama yüklemesi
/// başarısız olursa hiçbir nesnesi olmayan bir kayıt kalırdı ve o kayda
/// bağlanan bir post kırık bir görsel gösterirdi. Bu sırada ise en kötü
/// ihtimalle sahipsiz bir nesne kalıyor — görünmez, zararsız ve
/// [`cleanup_orphaned`]'in tarayabileceği bir çöp.
///
/// # Errors
/// Dosya doğrulamadan geçmezse [`Error::Validation`] /
/// [`Error::UnsupportedMedia`] (bkz. [`crate::media::process_image`]);
/// depolama erişilemezse [`Error::Internal`]; veritabanı hatası
/// [`Error::Database`].
pub async fn create_attachment(
    pool: &PgPool,
    storage: &Storage,
    id_codec: &IdCodec,
    actor_id: i64,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<Attachment> {
    let islenmis: ProcessedImage = media::process_image(bytes, max_bytes)?;

    let object_key = object_key_uret(id_codec, actor_id)?;
    let thumbnail_key = format!("{object_key}{THUMBNAIL_SUFFIX}");

    // Checksum **normalize edilmiş** çıktının, yüklenen ham dosyanın değil:
    // saklanan şey bu, bütünlük doğrulaması da bunun üzerinden anlamlı.
    let checksum = sha256_hex(&islenmis.data);
    let byte_size = i64::try_from(islenmis.data.len())
        .map_err(|_| Error::Internal("dosya boyutu i64'e sığmadı".to_owned()))?;

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
            "eklerden biri bulunamadı, sana ait değil ya da zaten bir içeriğe bağlı".to_owned(),
        ));
    }

    Ok(())
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
            SELECT id FROM attachments
            WHERE content_id IS NULL AND created_at < $1
            ORDER BY created_at
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
