//! Attachments: images that travel with a post or comment, created in the
//! same transaction as the content they belong to.
//!
//! **There is no standalone upload any more** (see REFACTOR.md §4: "this is
//! not an image host"). Before this unit, `POST /uploads` created a row with
//! `content_id IS NULL` and a later `POST /posts`/`POST /posts/{id}/comments`
//! call attached it via a separate `attach_to_content` step — which meant an
//! attachment could sit unattached forever, hence the orphan concept
//! (`cleanup_orphaned`, its advisory lock, its age threshold) that used to
//! live here. Both are gone: [`create_for_content`] is the only way an
//! attachment row is ever created, it always receives the `content_id` of
//! the content it belongs to at INSERT time, and it is always called from
//! inside the same database transaction as that content's own INSERT (see
//! `crate::content::create_post` / `crate::comment::create_comment`). A row
//! in this table with a NULL `content_id` is no longer a representable
//! state — `migrations/0027_attachments_content_id_not_null.up.sql` makes
//! the schema say so.
//!
//! Validating and normalizing the uploaded bytes is not this module's job —
//! that's [`crate::media`]; this module takes its output and drives storage
//! plus the database row together.
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

/// Maximum number of image files a single post or comment may carry (see
/// REFACTOR.md §4, "Remaining caps to set").
///
/// Chosen together with the 34 MiB `DefaultBodyLimit` override on
/// `POST /posts`/`POST /posts/{id}/comments` (`crate::routes::posts`/
/// `crate::routes::comments` in `actos-api`): 4 files at the default 8 MiB
/// `max_upload_bytes` each is already 32 MiB, and the remaining headroom is
/// there for the JSON `payload` part plus multipart boundary/header
/// overhead — not for a fifth file.
pub const MAX_ATTACHMENTS_PER_CONTENT: usize = 4;

/// Bir yükleme kaydı.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub id: i64,
    pub actor_id: i64,
    /// The content this attachment belongs to. **Never `NULL`** — see the
    /// module documentation; every row is born already pointing at its
    /// content.
    pub content_id: i64,
    pub object_key: String,
    pub byte_size: i64,
    pub mime_type: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub checksum_sha256: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Attachment {
    /// Küçük önizlemenin nesne anahtarı.
    #[must_use]
    pub fn thumbnail_key(&self) -> String {
        format!("{}{THUMBNAIL_SUFFIX}", self.object_key)
    }
}

/// Creates every attachment of a freshly-created post or comment, **inside
/// the caller's transaction** — called from `crate::content::create_post`
/// and `crate::comment::create_comment`, never on its own.
///
/// **All-or-nothing.** Every file is validated and normalized (format,
/// dimensions, decompression-bomb ceiling — see [`media::process_image`])
/// and the whole batch is quota-checked *before any of them touches
/// storage*. So if file 3 of 4 fails validation, files 1 and 2 were never
/// uploaded — there is nothing to clean up, and returning the error here
/// lets the caller's transaction roll back the content row too, so nothing
/// is written at all. The one thing this can't fully protect against is a
/// `storage.put_object` failure partway through the upload loop itself
/// (network hiccup, bucket unreachable): by then one or more EARLIER files
/// in the batch have already reached S3, and since the caller's transaction
/// still rolls back (the database side is fully undone), those objects are
/// left behind as invisible garbage. That is not a new risk this function
/// introduces — it is the exact same "upload before insert, worst case an
/// unreferenced object" trade-off the old single-file `create_attachment`
/// already made, just now reachable from a multi-file batch instead of one
/// call at a time.
///
/// **Storage quota** (flat per-account cap, [`crate::config::
/// StorageQuotaConfig`]) is checked ONCE for the whole batch: `current
/// usage (a live `SUM`, see `total_storage_bytes`) + the combined size of
/// every file in this request`, against `quota_bytes`. Checking per file
/// would mean partially accepting a multi-image post — this request is one
/// unit, so the check is too. This is still the same live-`SUM`-before-write
/// race this module has always accepted (two concurrent requests can both
/// pass the check and jointly overshoot the quota) — unchanged by this
/// move, only relocated from a dedicated `POST /uploads` handler into the
/// content-creation path.
///
/// `files.len() > `[`MAX_ATTACHMENTS_PER_CONTENT`]` is rejected outright,
/// before any file is even opened.
///
/// An empty `files` is the common case (a plain text post/comment) and is
/// not an error — it returns `Ok(vec![])` immediately, without touching the
/// quota or storage at all.
///
/// # Errors
/// More than [`MAX_ATTACHMENTS_PER_CONTENT`] files, a file that fails
/// validation, or a batch that would exceed the quota:
/// [`Error::Validation`] / [`Error::UnsupportedMedia`] (see
/// [`media::process_image`]); storage unreachable: [`Error::Internal`];
/// database error: [`Error::Database`].
#[allow(clippy::too_many_arguments)]
pub async fn create_for_content(
    tx: &mut PgConnection,
    storage: &Storage,
    id_codec: &IdCodec,
    actor_id: i64,
    content_id: i64,
    files: &[Vec<u8>],
    max_file_bytes: usize,
    quota_bytes: i64,
) -> Result<Vec<Attachment>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    if files.len() > MAX_ATTACHMENTS_PER_CONTENT {
        return Err(Error::Validation(format!(
            "too many files: {} given, at most {MAX_ATTACHMENTS_PER_CONTENT} allowed per post \
             or comment",
            files.len()
        )));
    }

    // Validate + normalize every file up front — see "All-or-nothing" above.
    // Nothing has touched storage or the database yet at this point.
    let processed: Vec<ProcessedImage> = files
        .iter()
        .map(|bytes| media::process_image(bytes, max_file_bytes))
        .collect::<Result<Vec<_>>>()?;

    let batch_bytes: i64 = processed
        .iter()
        .map(|islenmis| i64::try_from(islenmis.data.len()).unwrap_or(i64::MAX))
        .fold(0i64, i64::saturating_add);

    let mevcut_kullanim = total_storage_bytes_tx(&mut *tx, actor_id).await?;
    // Taşma savunması: bkz. eski `create_attachment` üzerindeki aynı
    // gerekçe — `saturating_add` en kötü ihtimalle kotayı "kesin aşılmış"
    // sayar, asla sessizce izin vermez.
    if mevcut_kullanim.saturating_add(batch_bytes) > quota_bytes {
        return Err(Error::Validation(format!(
            "storage quota exceeded: you are currently using {mevcut_kullanim} bytes, your \
             quota limit is {quota_bytes} bytes, and this request would add {batch_bytes} more \
             bytes across {} file(s) — you may need to delete some content first",
            processed.len()
        )));
    }

    let mut ekler = Vec::with_capacity(processed.len());
    for islenmis in processed {
        let object_key = object_key_uret(id_codec, actor_id)?;
        let thumbnail_key = format!("{object_key}{THUMBNAIL_SUFFIX}");

        // Checksum **normalize edilmiş** çıktının, yüklenen ham dosyanın
        // değil: saklanan şey bu, bütünlük doğrulaması da bunun üzerinden
        // anlamlı.
        let checksum = sha256_hex(&islenmis.data);
        let byte_size = i64::try_from(islenmis.data.len())
            .map_err(|_| Error::Internal("file size does not fit in i64".to_owned()))?;

        storage
            .put_object(&object_key, islenmis.data, ProcessedImage::mime_type())
            .await?;

        // Önizlemenin başarısız olması yüklemeyi düşürmüyor: asıl dosya
        // zaten yazıldı ve kayıt onun üzerinden anlamlı. Önizleme bir
        // kolaylık.
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
                (actor_id, content_id, object_key, byte_size, mime_type, width, height, checksum_sha256)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING id, created_at
            "#,
            actor_id,
            content_id,
            object_key,
            byte_size,
            ProcessedImage::mime_type(),
            width,
            height,
            checksum,
        )
        .fetch_one(&mut *tx)
        .await?;

        ekler.push(Attachment {
            id: row.id,
            actor_id,
            content_id,
            object_key,
            byte_size,
            mime_type: ProcessedImage::mime_type().to_owned(),
            width,
            height,
            checksum_sha256: checksum,
            created_at: row.created_at,
        });
    }

    Ok(ekler)
}

/// Bir actor'ün **tüm** eklerinin toplam bayt kullanımı — [`create_for_content`]'in
/// kota kontrolünün tek doğruluk kaynağı.
///
/// **Silinen ekler otomatik düşer:** ayrı bir "azalt" adımı yok çünkü hiç
/// gerekmiyor — `SUM(byte_size)`, sorgu her çalıştığında `attachments`
/// tablosunun **o anki** satırlarını topluyor.
///
/// `COALESCE(..., 0)`: actor'ün hiç eki yoksa `SUM` SQL'de `NULL` döner
/// (boş küme üzerinde toplam tanımsız) — `0`'a çeviriyoruz ki çağıranın
/// `Option` ile uğraşmasına gerek kalmasın. `::bigint` cast'i sqlx'in `SUM`
/// çıktısını (Postgres'te `numeric`, `byte_size` `bigint` olsa bile)
/// `i64`'e güvenle eşlemesi için.
///
/// This pool-based variant is used by tests that want to observe usage
/// outside of a transaction; [`create_for_content`] itself uses the
/// transaction-scoped `total_storage_bytes_tx` below, so it sees the same
/// connection's uncommitted state.
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

/// [`total_storage_bytes`]'in transaction içinde çalışan hâli — aynı sorgu,
/// yalnızca yürütücü (`executor`) farklı. sqlx'in `PgPool`/`&mut
/// PgConnection` için ayrı `Executor` implementasyonları olduğu için tek bir
/// generic yerine burada bilerek küçük bir kopya tutuluyor (bkz.
/// [`create_for_content`] — tek çağıran).
async fn total_storage_bytes_tx(tx: &mut PgConnection, actor_id: i64) -> Result<i64> {
    let toplam = sqlx::query_scalar!(
        r#"
        SELECT COALESCE(SUM(byte_size), 0)::bigint AS "toplam!"
        FROM attachments
        WHERE actor_id = $1
        "#,
        actor_id,
    )
    .fetch_one(tx)
    .await?;

    Ok(toplam)
}

/// `<actor dış id>/<uuidv7>.webp`.
///
/// UUIDv7 zaman sıralı: aynı actor'ün yüklemeleri bucket listelemesinde
/// kronolojik görünüyor, ve rastgele bir v4'ün aksine tahmin edilebilir bir
/// sıra vermiyor (zaman damgası dışındaki bitler rastgele).
///
/// `pub(crate)`: `crate::avatar` da aynı isim biçimini kullanıyor — bir
/// avatar da bu bucket'ta yaşayan bir nesne, yalnızca `attachments`
/// tablosunda bir satırı yok (bkz. `crate::avatar` modül dokümanı).
pub(crate) fn object_key_uret(id_codec: &IdCodec, actor_id: i64) -> Result<String> {
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
