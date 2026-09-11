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
    actor::{Page, paginate, resolve_live_actor_id, split_new_cursor},
    auth::{ActorRecord, ActorType, AdminRole},
    cursor::{Cursor, SortKey},
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
    /// Serbest biçimli ek veri (bkz. `migrations/0005_contents.up.sql` →
    /// `contents.metadata` COMMENT'i). `create_post` bunu her zaman
    /// [`normalize_metadata`]'dan geçmiş, geçerli bir JSON *nesnesi* olarak
    /// yazar (`ck_contents_metadata_object` de bunu şema seviyesinde
    /// zorluyor) — bu yüzden burada da her zaman `JsonValue::Object`,
    /// hiçbir zaman başka bir JSON türü değil.
    pub metadata: JsonValue,
    pub tags: Vec<String>,
    pub score: i32,
    pub upvotes: i32,
    pub downvotes: i32,
    pub comment_count: i32,
    /// Faz 12'de periyodik olarak hesaplanacak "sıcaklık" değeri; o zamana
    /// kadar şema varsayılanı olan `0`. `PostSort::Hot` sıralamasının ve
    /// onun cursor'ının dayandığı alan (bkz. [`PostSort::Hot`]).
    pub hot_score: f64,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    /// `Some` ise bu içerik soft-delete edilmiş. HTTP katmanı `get_post`
    /// için bunu hiç görmez (bkz. [`get_post`] — `410` erken döner), ama
    /// [`Content`] tek başına genel bir tip olduğu için (bkz. modül
    /// dokümantasyonu) burada taşınıyor: ileride bir liste bağlamında
    /// (Faz 9 yorum ağacı, Faz 12 feed) silinmiş bir öğeyi satır içinde
    /// `[deleted]` olarak göstermek isteyen bir çağıran buna ihtiyaç
    /// duyacak.
    pub deleted_at: Option<DateTime<Utc>>,
}

struct ContentRow {
    id: i64,
    content_type: ContentType,
    title: Option<String>,
    body: String,
    body_format: BodyFormat,
    metadata: JsonValue,
    score: i32,
    upvotes: i32,
    downvotes: i32,
    comment_count: i32,
    hot_score: f64,
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
            metadata: row.metadata,
            tags: row.tags,
            score: row.score,
            upvotes: row.upvotes,
            downvotes: row.downvotes,
            comment_count: row.comment_count,
            hot_score: row.hot_score,
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
            "at most {MAX_TAGS_PER_POST} tags can be added (received: {})",
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
            "metadata must be a JSON object".to_owned(),
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
/// `attachment_ids` verilirse o yüklemeler **aynı transaction'da** bu
/// post'a bağlanıyor (bkz. [`crate::attachment::attach_to_content`]);
/// yalnızca çağıranın kendi, henüz bağlanmamış yüklemeleri kabul edilir.
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
    attachment_ids: &[i64],
) -> Result<Content> {
    let title = text::validate_title(title).map_err(|e| Error::Validation(e.to_string()))?;
    if title.is_empty() {
        return Err(Error::Validation("post title cannot be empty".to_owned()));
    }
    let body = text::validate_body(body).map_err(|e| Error::Validation(e.to_string()))?;
    let tags = normalize_tags(tags)?;
    let metadata = normalize_metadata(metadata)?;

    let mut tx = pool.begin().await?;

    let row = sqlx::query!(
        r#"
        INSERT INTO contents (actor_id, content_type, title, body, body_format, metadata)
        VALUES ($1, 'post'::content_type, $2, $3, 'markdown'::body_format, $4)
        RETURNING id, created_at, score, upvotes, downvotes, comment_count, hot_score
        "#,
        author.id,
        title,
        body,
        // Klonlanıyor: aşağıdaki `Ok(Content { metadata, .. })` orijinal
        // değeri (DB'ye ekstra bir `SELECT` atmadan) geri döndürmek için
        // hâlâ ihtiyaç duyuyor — `query!` bağladığı argümanın sahipliğini
        // alıyor.
        metadata.clone(),
    )
    .fetch_one(&mut *tx)
    .await?;

    attach_tags(&mut tx, row.id, &tags).await?;

    // Ekler **aynı transaction'da** bağlanıyor: ek bağlama başarısız
    // olursa (ör. başkasının yüklemesi istendi) post da oluşmamalı, yoksa
    // istemcinin gönderdiğinden farklı bir post yaratmış olurduk.
    crate::attachment::attach_to_content(&mut tx, row.id, author.id, attachment_ids).await?;

    tx.commit().await?;

    Ok(Content {
        id: row.id,
        content_type: ContentType::Post,
        author: author.clone(),
        author_deleted: false,
        title: Some(title),
        body,
        body_format: BodyFormat::Markdown,
        metadata,
        tags,
        score: row.score,
        upvotes: row.upvotes,
        downvotes: row.downvotes,
        comment_count: row.comment_count,
        hot_score: row.hot_score,
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
            contents.metadata,
            contents.score,
            contents.upvotes,
            contents.downvotes,
            contents.comment_count,
            contents.hot_score,
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
            "at least one of title or body must be provided to update".to_owned(),
        ));
    }

    let title = title
        .map(|t| text::validate_title(&t).map_err(|e| Error::Validation(e.to_string())))
        .transpose()?;
    if let Some(t) = &title
        && t.is_empty()
    {
        return Err(Error::Validation("post title cannot be empty".to_owned()));
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

// --- Actor'e göre post listesi (Faz 7'den devir) ----------------------

/// `GET /actors/{username}/posts`: bir actor'ün postları, en yeni önce.
///
/// **Silinmiş postlar listede hiç görünmez** — `get_post`'un tersine
/// (satırı `410` ile ama var olarak taşıyan tekil okuma), burada
/// `contents.deleted_at IS NULL` filtresi SQL seviyesinde uygulanıyor:
/// bu bir liste ucu, silinmiş bir öğeyi `[deleted]` olarak satır içinde
/// göstermek (bkz. `actos_types::content` modül dokümantasyonundaki Faz 9/12
/// senaryosu) bu görevin kapsamında değil — PLAN.md bu uç için yalnızca "cursor'lu,
/// ContentSummary döndürür" diyor, silinmiş postu maskeli göstermeyi değil.
/// İleride bir yorum/feed listesi silinmiş öğeleri satır içinde göstermek
/// isterse bu filtreyi kaldırıp [`Content::deleted_at`]'i kullanabilir; bu
/// fonksiyon o davranışı şimdiden taahhüt etmiyor.
///
/// Sayfalama deseni `crate::actor`'daki `paginate`/`Page`/`limit + 1`
/// deseninin birebir aynısı (bkz. o modülün dokümantasyonu) — burada
/// yeniden icat edilmiyor, `pub(crate)` yapılıp buradan çağrılıyor.
/// `resolve_live_actor_id` de aynı sebeple oradan alınıyor: "username var
/// mı, canlı mı" kontrolü ve `410` kararı `get_profile`/`list_followers`
/// ile birebir aynı kural, iki farklı yerde iki farklı kopyası olmamalı.
///
/// # Errors
/// `username` hiç yoksa [`Error::NotFound`]; actor silinmişse
/// [`Error::Gone`]; veritabanı hatası [`Error::Database`].
pub async fn list_posts_by_actor(
    pool: &PgPool,
    username: &str,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Content>> {
    let actor_id = resolve_live_actor_id(pool, username).await?;
    let (cursor_created_at, cursor_id) = split_new_cursor(cursor);

    // İki aşamalı sorgu — bkz. `crate::feed::list_feed` dokümantasyonu
    // "Performans" bölümü ve `docs/query-plans.md`. Bu fonksiyon `actor_id =
    // $1` eşitliğiyle filtrelediği için satır sayısı platformun tamamıyla
    // değil yalnızca o actor'ün post sayısıyla büyüyor (ölçüm veritabanında
    // actor başına ~100), yani pratikte diske taşan bir aggregate riski
    // düşük — ama sorgu şekli `list_feed`/`list_posts_by_tag` ile birebir
    // aynı kusuru taşıyordu (`GROUP BY`, `ORDER BY ... LIMIT`'ten önce
    // çalışıyordu, ölçüldü: `actos_explain`'de en çok post'lu actor için
    // `GroupAggregate rows=100` → `Limit`). Aynı deseni burada da uygulamak
    // bedelsiz (yeni index yok, davranış aynı) ve çok post'lu bir actor
    // (ör. bir bot hesap) için gelecekte aynı sınıf soruna düşmeyi baştan
    // engelliyor.
    let rows = sqlx::query_as!(
        ContentRow,
        r#"
        WITH page AS (
            SELECT contents.id
            FROM contents
            WHERE contents.actor_id = $1
              AND contents.content_type = 'post'::content_type
              AND contents.deleted_at IS NULL
              AND (
                  $2::timestamptz IS NULL
                  OR (contents.created_at, contents.id) < ($2::timestamptz, $3::bigint)
              )
            ORDER BY contents.created_at DESC, contents.id DESC
            LIMIT $4
        )
        SELECT
            contents.id,
            contents.content_type AS "content_type: ContentType",
            contents.title,
            contents.body,
            contents.body_format AS "body_format: BodyFormat",
            contents.metadata,
            contents.score,
            contents.upvotes,
            contents.downvotes,
            contents.comment_count,
            contents.hot_score,
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
        FROM page
        JOIN contents ON contents.id = page.id
        JOIN actors ON actors.id = contents.actor_id
        LEFT JOIN content_tags ON content_tags.content_id = page.id
        LEFT JOIN tags ON tags.id = content_tags.tag_id
        GROUP BY contents.id, actors.id
        ORDER BY contents.created_at DESC, contents.id DESC
        "#,
        actor_id,
        cursor_created_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row: &ContentRow| row.id,
        |row: &ContentRow| SortKey::New {
            created_at: row.created_at,
        },
        Content::from,
    ))
}

// --- Etikete göre post listesi (Faz 10) ------------------------------------

/// Post listelerinin sıralaması (`?sort=`).
///
/// `crate::comment::CommentSort`'un post karşılığı; ayrı bir tip çünkü
/// post'larda [`Self::Hot`] de anlamlı (yorumlarda bir "sıcaklık" kavramı
/// yok, `hot_score` yalnızca post'lar için hesaplanacak — bkz. PLAN.md
/// Faz 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostSort {
    /// En yeni önce (`created_at DESC, id DESC`).
    New,
    /// En yüksek skor önce (`score DESC, id DESC`).
    Top,
    /// En "sıcak" önce (`hot_score DESC, id DESC`).
    ///
    /// **Faz 12'ye kadar `hot_score` her satırda `0`** (şema varsayılanı),
    /// yani bu sıralama şimdilik pratikte `id DESC`'e düşüyor. Uç bugünden
    /// çalışıyor ve sözleşmesi doğru; sıralamayı anlamlı kılacak olan
    /// periyodik `hot_score` hesabı Faz 12'nin işi.
    Hot,
}

impl PostSort {
    /// `?sort=` query parametresini ayrıştırır. Verilmemişse [`Self::New`].
    ///
    /// # Errors
    /// Tanınmayan bir değer [`Error::Validation`] üretir — sessizce
    /// varsayılana düşmek, yazım hatası yapan bir istemciye yanlış sıralı
    /// veriyi doğruymuş gibi verirdi.
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        match raw {
            None | Some("new") => Ok(Self::New),
            Some("top") => Ok(Self::Top),
            Some("hot") => Ok(Self::Hot),
            Some(other) => Err(Error::Validation(format!(
                "invalid sort value: \"{other}\" (expected: new, top, hot)"
            ))),
        }
    }

    /// Bu sıralamanın cursor türü.
    #[must_use]
    pub const fn sort_kind(self) -> crate::cursor::SortKind {
        match self {
            Self::New => crate::cursor::SortKind::New,
            Self::Top => crate::cursor::SortKind::Top,
            Self::Hot => crate::cursor::SortKind::Hot,
        }
    }
}

/// Bir [`ContentRow`]'dan bu sıralamanın cursor anahtarını türetir.
fn post_sort_key(sort: PostSort, row: &ContentRow) -> SortKey {
    match sort {
        PostSort::New => SortKey::New {
            created_at: row.created_at,
        },
        PostSort::Top => SortKey::Top { score: row.score },
        PostSort::Hot => SortKey::Hot {
            hot_score: row.hot_score,
        },
    }
}

/// Cursor'ı sıralamaya göre bindable üçlüye ayırır: `(created_at, score,
/// hot_score, id)`. Yalnızca ilgili alan dolu olur, diğerleri `None`.
///
/// Tek bir sorgu metniyle üç sıralamayı ifade edemediğimiz için (bkz.
/// [`list_posts_by_tag`]) her dal kendi parametrelerini bağlıyor; bu
/// fonksiyon o dalların ortak ayrıştırma mantığını tek yerde tutuyor.
///
/// # Errors
/// Cursor listenin sıralamasına ait değilse [`Error::InvalidCursor`].
#[allow(clippy::type_complexity)]
fn split_post_cursor(
    sort: PostSort,
    cursor: Option<Cursor>,
) -> Result<(Option<DateTime<Utc>>, Option<i32>, Option<f64>, Option<i64>)> {
    match (sort, cursor) {
        (_, None) => Ok((None, None, None, None)),
        (
            PostSort::New,
            Some(Cursor {
                sort: SortKey::New { created_at },
                id,
            }),
        ) => Ok((Some(created_at), None, None, Some(id))),
        (
            PostSort::Top,
            Some(Cursor {
                sort: SortKey::Top { score },
                id,
            }),
        ) => Ok((None, Some(score), None, Some(id))),
        (
            PostSort::Hot,
            Some(Cursor {
                sort: SortKey::Hot { hot_score },
                id,
            }),
        ) => Ok((None, None, Some(hot_score), Some(id))),
        _ => Err(Error::InvalidCursor),
    }
}

/// `GET /tags/{name}/posts`: bir etiketteki post'lar (amac.txt'teki
/// `GET posts/nvidia` senaryosu).
///
/// Etiket adı [`text::validate_tag_name`]'den geçiriliyor; **hiç var
/// olmayan bir etiket `404`**, var olup hiç canlı post'u kalmamış bir
/// etiket ise boş liste döner. Ayrım bilinçli: "böyle bir etiket yok" ile
/// "bu etikette şu an içerik yok" istemci için farklı bilgiler.
///
/// Silinmiş post'lar listede görünmez ([`list_posts_by_actor`] ile aynı
/// karar).
///
/// **Üç ayrı `query_as!` çağrısı** var çünkü `sqlx` derleme zamanı
/// doğrulaması için sorgunun string **literal** olmasını şart koşuyor;
/// `ORDER BY` ve cursor koşulu sıralamaya göre değiştiğinden tek metinle
/// ifade edilemiyor. Dinamik string birleştirme bu doğrulamayı ve
/// `sqlx prepare` önbelleğini bozardı.
///
/// # Errors
/// Etiket adı geçersizse ya da böyle bir etiket yoksa [`Error::NotFound`];
/// cursor sıralamaya ait değilse [`Error::InvalidCursor`]; veritabanı
/// hatası [`Error::Database`].
pub async fn list_posts_by_tag(
    pool: &PgPool,
    tag_name: &str,
    sort: PostSort,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Content>> {
    let name = text::validate_tag_name(tag_name).map_err(|_| Error::NotFound("tag"))?;

    let tag_id = sqlx::query_scalar!(r#"SELECT id FROM tags WHERE name = $1"#, name)
        .fetch_optional(pool)
        .await?
        .ok_or(Error::NotFound("tag"))?;

    let (cursor_created_at, cursor_score, cursor_hot, cursor_id) = split_post_cursor(sort, cursor)?;

    // İki aşamalı sorgu — bkz. `crate::feed::list_feed` dokümantasyonu
    // "Performans" bölümü ve `docs/query-plans.md`. Etiket filtresi
    // (`filtre.tag_id = $1`) artık `page` CTE'sinin içinde: popüler bir
    // etiket (ölçüm veritabanında 100 000 post) için bütün eşleşen küme
    // önce gruplanıp diske taşınmadan, yalnızca sayfa boyutu kadar id
    // seçiliyor.
    let rows = match sort {
        PostSort::New => {
            sqlx::query_as!(
                ContentRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    JOIN content_tags AS filtre ON filtre.content_id = contents.id
                    WHERE filtre.tag_id = $1
                      AND contents.content_type = 'post'::content_type
                      AND contents.deleted_at IS NULL
                      AND (
                          $2::timestamptz IS NULL
                          OR (contents.created_at, contents.id) < ($2::timestamptz, $3::bigint)
                      )
                    ORDER BY contents.created_at DESC, contents.id DESC
                    LIMIT $4
                )
                SELECT
                    contents.id,
                    contents.content_type AS "content_type: ContentType",
                    contents.title,
                    contents.body,
                    contents.body_format AS "body_format: BodyFormat",
                    contents.metadata,
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
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
                FROM page
                JOIN contents ON contents.id = page.id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id
                ORDER BY contents.created_at DESC, contents.id DESC
                "#,
                tag_id,
                cursor_created_at,
                cursor_id,
                limit + 1,
            )
            .fetch_all(pool)
            .await?
        }
        PostSort::Top => {
            sqlx::query_as!(
                ContentRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    JOIN content_tags AS filtre ON filtre.content_id = contents.id
                    WHERE filtre.tag_id = $1
                      AND contents.content_type = 'post'::content_type
                      AND contents.deleted_at IS NULL
                      AND (
                          $2::int IS NULL
                          OR (contents.score, contents.id) < ($2::int, $3::bigint)
                      )
                    ORDER BY contents.score DESC, contents.id DESC
                    LIMIT $4
                )
                SELECT
                    contents.id,
                    contents.content_type AS "content_type: ContentType",
                    contents.title,
                    contents.body,
                    contents.body_format AS "body_format: BodyFormat",
                    contents.metadata,
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
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
                FROM page
                JOIN contents ON contents.id = page.id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id
                ORDER BY contents.score DESC, contents.id DESC
                "#,
                tag_id,
                cursor_score,
                cursor_id,
                limit + 1,
            )
            .fetch_all(pool)
            .await?
        }
        PostSort::Hot => {
            sqlx::query_as!(
                ContentRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    JOIN content_tags AS filtre ON filtre.content_id = contents.id
                    WHERE filtre.tag_id = $1
                      AND contents.content_type = 'post'::content_type
                      AND contents.deleted_at IS NULL
                      AND (
                          $2::double precision IS NULL
                          OR (contents.hot_score, contents.id) < ($2::double precision, $3::bigint)
                      )
                    ORDER BY contents.hot_score DESC, contents.id DESC
                    LIMIT $4
                )
                SELECT
                    contents.id,
                    contents.content_type AS "content_type: ContentType",
                    contents.title,
                    contents.body,
                    contents.body_format AS "body_format: BodyFormat",
                    contents.metadata,
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
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
                FROM page
                JOIN contents ON contents.id = page.id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id
                ORDER BY contents.hot_score DESC, contents.id DESC
                "#,
                tag_id,
                cursor_hot,
                cursor_id,
                limit + 1,
            )
            .fetch_all(pool)
            .await?
        }
    };

    Ok(paginate(
        rows,
        limit,
        |row: &ContentRow| row.id,
        |row: &ContentRow| post_sort_key(sort, row),
        Content::from,
    ))
}
