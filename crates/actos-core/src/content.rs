//! İçerik (post + yorum) domain katmanı: post oluşturma/okuma/düzenleme/
//! silme ve etiket ilişkilendirme.
//!
//! HTTP'yi bilmez — `actos-api/src/routes/posts.rs` bu modülün
//! fonksiyonlarını çağırıp sonucu HTTP'ye çevirir (bkz. `crate::actor`
//! modülündeki aynı katman ayrımı).
//!
//! **v1'de yalnızca post'lar burada.** Yorumlar (`parent_content_id` dolu
//! satırlar) Faz 9'un konusu — `path`/`depth`/`root_post_id`'nin ebeveyn
//! zincirinden türetilmesi, silinmiş ebeveyne yanıt yasağı ve
//! `comment_count` atomik güncellemesi gibi ek kurallar taşıyorlar ve
//! ayrı bir tur hak ediyorlar. Yine de [`Content`]/[`ContentType`]/
//! [`BodyFormat`] tipleri post'a özel değil — `contents` tablosunun
//! kendisi gibi geneller, Faz 9 bunları aynen yeniden kullanacak (bkz.
//! `actos_types::content` modül dokümantasyonu).
//!
//! ## `path`/`depth`/`root_post_id`'ye asla dokunulmuyor
//!
//! `migrations/0005_contents.up.sql` içindeki `trg_contents_set_path`
//! BEFORE INSERT trigger'ı bu üç sütunu **kendisi** hesaplıyor
//! (`NEW.id` IDENTITY varsayılanı trigger'dan önce atandığı için hazır
//! oluyor, bkz. o migration'daki yorum). Bu modülün INSERT'leri bu
//! sütunlara asla değer vermiyor.
//!
//! ## Etiket üst sınırı neden burada, `text.rs`'te değil
//!
//! `docs/db-conventions.md`'deki tabloya göre "post başına 10 etiket"
//! bir **ürün** kuralı, veri bütünlüğü kuralı değil — zamanla değişmesi
//! beklenir, şemada bir CHECK olarak yok. `text::validate_tag_name` yalnızca
//! *tek bir* etiketin format/uzunluk kuralını doğruluyor (şemadaki
//! `ck_tags_name_format`'la birebir); "kaç tane" sorusu içerik oluşturma
//! iş kuralı olduğu için [`MAX_TAGS_PER_POST`] burada.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{PgConnection, PgPool};

use crate::{
    auth::{ActorRecord, ActorType, AdminRole},
    error::{Error, Result},
    text,
};

/// Bir post'a eklenebilecek azami etiket sayısı (bkz. modül başındaki
/// gerekçe ve `migrations/0007_tags.up.sql` üzerindeki yorum).
pub const MAX_TAGS_PER_POST: usize = 10;

// --- Domain tipleri ----------------------------------------------------

/// `migrations/0005_contents.up.sql` → `content_type` Postgres enum'ının
/// Rust karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "content_type", rename_all = "snake_case")]
pub enum ContentType {
    Post,
    Comment,
}

/// `migrations/0005_contents.up.sql` → `body_format` Postgres enum'ının
/// Rust karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "body_format", rename_all = "snake_case")]
pub enum BodyFormat {
    Markdown,
    Plain,
}

/// `contents` tablosundan (yazar + etiketleriyle birlikte) okunan bir
/// satır.
///
/// **`author`/`author_deleted` ayrı alanlar, `author: Option<ActorRecord>`
/// DEĞİL:** yazar satırı soft-delete'te silinmiyor (bkz. `crate::actor`
/// dokümantasyonu — username impersonation riski yüzünden serbest
/// bırakılmıyor), yani `ActorRecord` her zaman doldurulabilir; yalnızca
/// *gösterilip gösterilemeyeceği* değişiyor. Bu ayrımın kendisi bir
/// maskeleme kararı değil, ham bir gerçek — maskelemenin nasıl yapılacağı
/// (hangi alanların gizleneceği) bilerek burada değil, HTTP çeviri
/// katmanında (`actos-api/src/routes/posts.rs`) veriliyor; bkz. o
/// dosyadaki `masked_actor_summary` üzerindeki gerekçe.
#[derive(Debug, Clone)]
pub struct Content {
    pub id: i64,
    pub content_type: ContentType,
    pub author: ActorRecord,
    pub author_deleted: bool,
    pub title: Option<String>,
    pub body: String,
    pub body_format: BodyFormat,
    pub tags: Vec<String>,
    pub score: i32,
    pub upvotes: i32,
    pub downvotes: i32,
    pub comment_count: i32,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    /// `Some` ise bu içerik soft-delete edilmiş. HTTP katmanı `get_post`
    /// için bunu hiç görmez (bkz. [`get_post`] — `410` erken döner), ama
    /// [`Content`] tek başına genel bir tip olduğu için (bkz. modül
    /// dokümantasyonu) burada taşınıyor: ileride bir liste bağlamında
    /// (Faz 9 yorum ağacı, Faz 12 feed) silinmiş bir öğeyi satır içinde
    /// `[silindi]` olarak göstermek isteyen bir çağıran buna ihtiyaç
    /// duyacak.
    pub deleted_at: Option<DateTime<Utc>>,
}

struct ContentRow {
    id: i64,
    content_type: ContentType,
    title: Option<String>,
    body: String,
    body_format: BodyFormat,
    score: i32,
    upvotes: i32,
    downvotes: i32,
    comment_count: i32,
    created_at: DateTime<Utc>,
    edited_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
    author_id: i64,
    author_username: String,
    author_actor_type: ActorType,
    author_display_name: Option<String>,
    author_bio: Option<String>,
    author_created_at: DateTime<Utc>,
    author_deleted_at: Option<DateTime<Utc>>,
    tags: Vec<String>,
}

impl From<ContentRow> for Content {
    fn from(row: ContentRow) -> Self {
        Self {
            id: row.id,
            content_type: row.content_type,
            author: ActorRecord {
                id: row.author_id,
                username: row.author_username,
                actor_type: row.author_actor_type,
                display_name: row.author_display_name,
                bio: row.author_bio,
                created_at: row.author_created_at,
            },
            author_deleted: row.author_deleted_at.is_some(),
            title: row.title,
            body: row.body,
            body_format: row.body_format,
            tags: row.tags,
            score: row.score,
            upvotes: row.upvotes,
            downvotes: row.downvotes,
            comment_count: row.comment_count,
            created_at: row.created_at,
            edited_at: row.edited_at,
            deleted_at: row.deleted_at,
        }
    }
}

// --- Girdi doğrulama -----------------------------------------------------

/// Ham etiket listesini doğrular: sayı sınırı, format (`text::
/// validate_tag_name` üzerinden) ve **tekilleştirme** — aynı etiketin iki
/// kez gönderilmesi (`"rust", "Rust"`) tek bir satıra çökmeli.
/// `BTreeSet` hem tekilleştiriyor hem de sonucu deterministik (alfabetik)
/// sıraya sokuyor; çıktı sırasının test edilebilir/öngörülebilir olması
/// için bilinçli bir seçim, `HashSet` de aynı işi görürdü ama sırasız.
///
/// # Errors
/// Etiket sayısı [`MAX_TAGS_PER_POST`]'u aşarsa veya herhangi bir etiket
/// formatı geçersizse [`Error::Validation`].
fn normalize_tags(raw: &[String]) -> Result<Vec<String>> {
    if raw.len() > MAX_TAGS_PER_POST {
        return Err(Error::Validation(format!(
            "en fazla {MAX_TAGS_PER_POST} etiket eklenebilir (gönderilen: {})",
            raw.len()
        )));
    }

    let mut normalized = BTreeSet::new();
    for tag in raw {
        let tag = text::validate_tag_name(tag).map_err(|e| Error::Validation(e.to_string()))?;
        normalized.insert(tag);
    }

    Ok(normalized.into_iter().collect())
}

/// `metadata` alanını doğrular: `ck_contents_metadata_object` (bkz.
/// `migrations/0005_contents.up.sql`) yalnızca JSON *nesnesine* izin
/// veriyor — burada aynı kuralı, veritabanına gitmeden, doğrulama
/// hatasıyla erken karşılıyoruz. Verilmemişse boş nesne varsayılan.
///
/// # Errors
/// Değer verilmiş ama bir JSON nesnesi değilse [`Error::Validation`].
fn normalize_metadata(raw: Option<JsonValue>) -> Result<JsonValue> {
    match raw {
        None => Ok(JsonValue::Object(serde_json::Map::new())),
        Some(JsonValue::Object(map)) => Ok(JsonValue::Object(map)),
        Some(_) => Err(Error::Validation(
            "metadata bir JSON nesnesi olmalı".to_owned(),
        )),
    }
}

// --- Etiket ilişkilendirme -------------------------------------------------

/// Verilen (zaten normalize edilmiş) etiket adlarını `tags` tablosunda
/// var olmayanları oluşturarak `content_id`'ye bağlar — hepsi tek
/// transaction'ın parçası olarak çağıranın bağlantısı üzerinden.
///
/// **N+1 yok:** etiket sayısı zaten [`MAX_TAGS_PER_POST`] ile sınırlı
/// (azami 10) olsa da, üç sorguya (`INSERT ... SELECT unnest`, tek bir
/// `SELECT ... WHERE name = ANY`, `INSERT ... SELECT unnest`) toplanıyor —
/// etiket başına ayrı bir round-trip atılmıyor.
///
/// `INSERT INTO tags ... ON CONFLICT (name) DO NOTHING` var olan bir
/// etiketle çakışmayı sessizce yutar (bkz. PLAN.md Faz 8 görev tanımı);
/// hangi satırların yeni/eski olduğunu ayırt etmemize gerek yok, sonraki
/// `SELECT ... WHERE name = ANY` her iki durumda da id'leri getirir.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
async fn attach_tags(conn: &mut PgConnection, content_id: i64, tags: &[String]) -> Result<()> {
    if tags.is_empty() {
        return Ok(());
    }

    sqlx::query!(
        r#"
        INSERT INTO tags (name)
        SELECT * FROM UNNEST($1::text[])
        ON CONFLICT (name) DO NOTHING
        "#,
        tags,
    )
    .execute(&mut *conn)
    .await?;

    let tag_ids: Vec<i64> =
        sqlx::query_scalar!(r#"SELECT id FROM tags WHERE name = ANY($1::text[])"#, tags,)
            .fetch_all(&mut *conn)
            .await?;

    sqlx::query!(
        r#"
        INSERT INTO content_tags (content_id, tag_id)
        SELECT $1, * FROM UNNEST($2::bigint[])
        ON CONFLICT DO NOTHING
        "#,
        content_id,
        &tag_ids,
    )
    .execute(&mut *conn)
    .await?;

    Ok(())
}

// --- Post oluşturma --------------------------------------------------------

/// `POST /posts`: yeni bir post oluşturur (+ eksik etiketler, aynı
/// transaction'da).
///
/// `title`/`body` [`text::validate_title`]/[`text::validate_body`]'den
/// geçer; ayrıca burada, o genel doğrulayıcıların izin verdiği ama bir
/// post için anlamsız olan **boş başlığı** ayrıca reddediyoruz (yorumlarda
/// zaten `title` yok, bu kural post'a özel olduğu için `text.rs`'e değil
/// buraya ait).
///
/// `path`/`depth`/`root_post_id`'ye dokunulmuyor (bkz. modül
/// dokümantasyonu) — `INSERT`, `trg_contents_set_path` trigger'ının
/// bunları hesaplamasına bırakılıyor.
///
/// # Errors
/// `title`/`body`/`tags`/`metadata` doğrulamadan geçmezse
/// [`Error::Validation`]; veritabanı hatası [`Error::Database`].
pub async fn create_post(
    pool: &PgPool,
    author: &ActorRecord,
    title: &str,
    body: &str,
    tags: &[String],
    metadata: Option<JsonValue>,
) -> Result<Content> {
    let title = text::validate_title(title).map_err(|e| Error::Validation(e.to_string()))?;
    if title.is_empty() {
        return Err(Error::Validation("post başlığı boş olamaz".to_owned()));
    }
    let body = text::validate_body(body).map_err(|e| Error::Validation(e.to_string()))?;
    let tags = normalize_tags(tags)?;
    let metadata = normalize_metadata(metadata)?;

    let mut tx = pool.begin().await?;

    let row = sqlx::query!(
        r#"
        INSERT INTO contents (actor_id, content_type, title, body, body_format, metadata)
        VALUES ($1, 'post'::content_type, $2, $3, 'markdown'::body_format, $4)
        RETURNING id, created_at, score, upvotes, downvotes, comment_count
        "#,
        author.id,
        title,
        body,
        metadata,
    )
    .fetch_one(&mut *tx)
    .await?;

    attach_tags(&mut tx, row.id, &tags).await?;

    tx.commit().await?;

    Ok(Content {
        id: row.id,
        content_type: ContentType::Post,
        author: author.clone(),
        author_deleted: false,
        title: Some(title),
        body,
        body_format: BodyFormat::Markdown,
        tags,
        score: row.score,
        upvotes: row.upvotes,
        downvotes: row.downvotes,
        comment_count: row.comment_count,
        created_at: row.created_at,
        edited_at: None,
        deleted_at: None,
    })
}

// --- Post okuma ------------------------------------------------------------

/// `GET /posts/{id}`: tek bir post + yazar + etiketler.
///
/// Tek bir SQL sorgusuyla (yazar `JOIN`, etiketler `LEFT JOIN` +
/// `array_agg`) getirilir. Post başına ek bir etiket sorgusu atılmıyor
/// (bkz. modül dokümantasyonundaki N+1 notu — burada tek satır çekildiği
/// için zaten "post başına" diye bir tekrar da yok). Sorgu metni bilerek
/// bir `const &str` değil, doğrudan makro çağrısının içinde: `sqlx::
/// query_as!` derleme zamanında sorguyu veritabanına karşı doğrulamak için
/// bir string **literal** bekliyor, bir sabite yapılan referansı kabul
/// etmiyor.
///
/// **Silinmiş post `404` değil `410` döner** — `crate::actor::get_profile`
/// ile aynı gerekçe: satır hâlâ var (`path`'teki yerini koruyor, alt
/// yorumlar yetim kalmasın diye, bkz. `migrations/0005_contents.up.sql`
/// `deleted_at` yorumu), sadece gizli. `title`/`body` gerçek değerleri bu
/// durumda yanıta **hiç girmiyor** — `Content` hiç oluşturulmadan erken
/// dönüyoruz, HTTP katmanının maskelemeyi unutmasına bağlı bir savunma
/// değil, veri hiç oraya ulaşmıyor.
///
/// # Errors
/// Post yoksa (ya da `id` bir yoruma aitse — `content_type = 'post'`
/// filtresi bunu da `NotFound` sayar) [`Error::NotFound`]; silinmişse
/// [`Error::Gone`]; veritabanı hatası [`Error::Database`].
pub async fn get_post(pool: &PgPool, id: i64) -> Result<Content> {
    let row = sqlx::query_as!(
        ContentRow,
        r#"
        SELECT
            contents.id,
            contents.content_type AS "content_type: ContentType",
            contents.title,
            contents.body,
            contents.body_format AS "body_format: BodyFormat",
            contents.score,
            contents.upvotes,
            contents.downvotes,
            contents.comment_count,
            contents.created_at,
            contents.edited_at,
            contents.deleted_at,
            actors.id AS author_id,
            actors.username AS author_username,
            actors.actor_type AS "author_actor_type: ActorType",
            actors.display_name AS author_display_name,
            actors.bio AS author_bio,
            actors.created_at AS author_created_at,
            actors.deleted_at AS author_deleted_at,
            COALESCE(
                array_agg(tags.name::text) FILTER (WHERE tags.id IS NOT NULL),
                '{}'
            ) AS "tags!: Vec<String>"
        FROM contents
        JOIN actors ON actors.id = contents.actor_id
        LEFT JOIN content_tags ON content_tags.content_id = contents.id
        LEFT JOIN tags ON tags.id = content_tags.tag_id
        WHERE contents.id = $1 AND contents.content_type = 'post'::content_type
        GROUP BY contents.id, actors.id
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?;

    let row = row.ok_or(Error::NotFound("post"))?;

    if row.deleted_at.is_some() {
        return Err(Error::Gone("post"));
    }

    Ok(row.into())
}

// --- Post güncelleme ---------------------------------------------------

/// `PATCH /posts/{id}`: yalnızca sahibi, kısmi güncelleme (`title`/`body`).
///
/// Üç adım **tek transaction'da**: (1) satır `FOR UPDATE` ile kilitlenip
/// sahiplik/silinmişlik kontrol edilir; (2) **eski hâli** `edit_history`'ye
/// yazılır; (3) yeni değerler uygulanır, `edited_at = now()`. `FOR UPDATE`
/// olmadan iki eşzamanlı `PATCH` arasında bir yarış olurdu (ikisi de aynı
/// "eski" satırı okuyup `edit_history`'ye yazabilir, biri kaybolurdu).
///
/// `title`/`body` **`Option<String>`** — `Option<Option<String>>` değil,
/// çünkü hiçbiri temizlenebilir (`NULL`) bir alan değil (bkz.
/// `actos_types::content::UpdatePostRequest` dokümanı). İkisi de
/// verilmemişse bu bir no-op `PATCH`'tir ve reddedilir — sessizce "hiçbir
/// şey değişmedi" 200 dönmek yerine, çağıranın muhtemelen bir hata
/// yaptığını (boş gövdeli `PATCH`) açıkça bildirmek daha iyi.
///
/// Commit sonrası [`get_post`] ile aynı satırı yeniden okuyoruz: kodu
/// tekrarlamamak (post + yazar + etiket birleştirme sorgusu tek yerde,
/// `get_post` içinde) için bilinçli bir taviz — ekstra bir sorgu pahasına,
/// iki farklı yerde senkron tutulması gereken iki ayrı sorgu olmasın.
///
/// # Errors
/// Post yoksa [`Error::NotFound`]; silinmişse [`Error::Gone`]; çağıran
/// sahibi değilse [`Error::Forbidden`]; `title`/`body` doğrulamadan
/// geçmezse ya da ikisi de verilmemişse [`Error::Validation`]; veritabanı
/// hatası [`Error::Database`].
pub async fn update_post(
    pool: &PgPool,
    id: i64,
    actor_id: i64,
    title: Option<String>,
    body: Option<String>,
) -> Result<Content> {
    if title.is_none() && body.is_none() {
        return Err(Error::Validation(
            "güncellemek için title veya body alanlarından en az biri gönderilmeli".to_owned(),
        ));
    }

    let title = title
        .map(|t| text::validate_title(&t).map_err(|e| Error::Validation(e.to_string())))
        .transpose()?;
    if let Some(t) = &title
        && t.is_empty()
    {
        return Err(Error::Validation("post başlığı boş olamaz".to_owned()));
    }
    let body = body
        .map(|b| text::validate_body(&b).map_err(|e| Error::Validation(e.to_string())))
        .transpose()?;

    let mut tx = pool.begin().await?;

    let current = sqlx::query!(
        r#"
        SELECT actor_id, title, body, deleted_at
        FROM contents
        WHERE id = $1 AND content_type = 'post'::content_type
        FOR UPDATE
        "#,
        id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let current = current.ok_or(Error::NotFound("post"))?;

    if current.deleted_at.is_some() {
        return Err(Error::Gone("post"));
    }
    if current.actor_id != actor_id {
        return Err(Error::Forbidden);
    }

    sqlx::query!(
        r#"
        INSERT INTO edit_history (content_id, previous_title, previous_body)
        VALUES ($1, $2, $3)
        "#,
        id,
        current.title,
        current.body,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        UPDATE contents
        SET title = COALESCE($2, title), body = COALESCE($3, body), edited_at = now()
        WHERE id = $1
        "#,
        id,
        title,
        body,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    get_post(pool, id).await
}

// --- Post silme --------------------------------------------------------

/// `DELETE /posts/{id}`: sahibi **veya** moderatör/admin. Soft-delete
/// (`deleted_at`).
///
/// Sıra bilinçli: önce varlık (`404`), sonra zaten silinmiş mi (`410`),
/// sonra yetki (`403`) — bir moderatörün olmayan bir post'u silmeye
/// çalışması `403` değil `404` almalı (yetkisi olsa da olmasa da post
/// yok), ama var olan başkasının post'unu silmeye çalışan sıradan bir
/// actor `403` almalı. `roles` boşsa (sıradan actor) yalnızca sahiplik
/// kontrol edilir.
///
/// # Errors
/// Post yoksa [`Error::NotFound`]; zaten silinmişse [`Error::Gone`];
/// çağıran ne sahibi ne moderatör/admin ise [`Error::Forbidden`];
/// veritabanı hatası [`Error::Database`].
pub async fn delete_post(pool: &PgPool, id: i64, actor_id: i64, roles: &[AdminRole]) -> Result<()> {
    let mut tx = pool.begin().await?;

    let current = sqlx::query!(
        r#"
        SELECT actor_id, deleted_at
        FROM contents
        WHERE id = $1 AND content_type = 'post'::content_type
        FOR UPDATE
        "#,
        id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let current = current.ok_or(Error::NotFound("post"))?;

    if current.deleted_at.is_some() {
        return Err(Error::Gone("post"));
    }

    let is_owner = current.actor_id == actor_id;
    let is_moderator = roles
        .iter()
        .any(|r| matches!(r, AdminRole::Admin | AdminRole::Moderator));

    if !is_owner && !is_moderator {
        return Err(Error::Forbidden);
    }

    sqlx::query!(
        r#"UPDATE contents SET deleted_at = now() WHERE id = $1"#,
        id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}
