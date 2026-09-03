//! Yorumlar: `contents` ağacının `content_type = 'comment'` olan düğümleri.
//!
//! **Neden `crate::content`'ten ayrı bir modül:** yorumlar da `contents`
//! tablosunda yaşıyor ve [`crate::content::Content`] tipini aynen
//! kullanıyorlar — ayrı olmalarının sebebi veri modeli değil, `content.rs`'in
//! zaten post CRUD + etiketler + idempotent oluşturma ile dolu olması. Ağaç
//! yürüme (`ltree`), ata sayaç güncellemesi ve iç içe listeleme kendi başına
//! yeterince büyük bir konu; tek dosyaya sıkıştırmak ikisini de okunmaz
//! yapardı. Paylaşılan tipler (`Content`, `ContentType`, `BodyFormat`) ve
//! sayfalama (`crate::actor::paginate`) yeniden kullanılıyor, kopyalanmıyor.
//!
//! ## `path` / `depth` / `root_post_id` uygulama tarafından YAZILMAZ
//!
//! `migrations/0005_contents.up.sql`'deki `trg_contents_set_path` trigger'ı
//! bu üç sütunu BEFORE INSERT'te kendisi hesaplar (yorum için
//! `parent.path || 'c<yeni id>'`), ayrıca silinmiş bir ebeveyne ya da
//! silinmiş bir kök post'un ağacına yanıt verilmesini ve 32 derinlik
//! sınırının aşılmasını reddeder. Bu modül INSERT'te o sütunlara
//! dokunmuyor.
//!
//! Buna rağmen [`create_comment`] aynı kontrolleri (ebeveyn canlı mı, kök
//! post canlı mı, derinlik sınırı) **transaction içinde önden** yapıyor.
//! Sebep: trigger'ın `RAISE EXCEPTION`'ı istemciye 500 olarak yansırdı;
//! önden kontrol `404`/`410`/`400` gibi doğru ve anlaşılır bir yanıt
//! üretiyor. Trigger böylece "istemciye hata anlatan" değil, "veritabanı
//! değişmezini koruyan" son savunma hattı olarak kalıyor — iki katman
//! birbirinin yedeği.
//!
//! ## `comment_count` neden silmede azaltılmıyor
//!
//! [`create_comment`] yeni yorumun **tüm atalarında** (kök post dahil)
//! `comment_count`'u atomik olarak artırır. [`delete_comment`] ise
//! azaltmaz. Bu bilinçli: silinen bir yorum ağaçtan kaldırılmıyor, çocukları
//! yetim kalmasın diye yerinde duruyor ve istemciye `[deleted]` gövdesiyle
//! görünmeye devam ediyor (bkz. `actos_types::content` modül
//! dokümantasyonu). Sayaç "bu ağaçta kaç düğüm var" sorusunu yanıtlıyor;
//! azaltsaydık istemcinin çizdiği ağaçtaki düğüm sayısı ile başlıktaki sayı
//! birbirini tutmazdı.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::{
    actor::{Page, paginate, resolve_live_actor_id, split_new_cursor},
    auth::{ActorRecord, ActorType, AdminRole},
    content::{BodyFormat, Content, ContentType},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    notification::{self, NotificationKind},
    text,
};

/// Şemanın (`ck_contents_depth`) izin verdiği azami derinlik. Buradaki
/// kopya, trigger'a gitmeden önce anlaşılır bir hata üretebilmek için var
/// (bkz. modül dokümantasyonu) — şemadaki değer değişirse burası da
/// değişmeli.
pub const MAX_COMMENT_DEPTH: i32 = 32;

/// `?depth=` verilmediğinde bir yorum ağacında kaç seviye yanıt döndürülür.
///
/// Küçük tutuluyor: derin ağaçların tamamını her istekte göndermek hem
/// yanıtı şişirir hem de istemcinin çoğunlukla okumadığı veriyi taşır.
/// Daha derine inmek isteyen istemci `?parent=<id>` ile o alt ağacı ayrıca
/// çeker ("daha fazla yanıt yükle").
pub const DEFAULT_TREE_DEPTH: i32 = 5;

/// Bir yorum ağacında `?depth=` ile istenebilecek azami seviye.
pub const MAX_TREE_DEPTH: i32 = MAX_COMMENT_DEPTH;

/// Yorum listelerinin sıralaması (`?sort=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentSort {
    /// En yeni önce (`created_at DESC, id DESC`).
    New,
    /// En yüksek skor önce (`score DESC, id DESC`).
    Top,
}

impl CommentSort {
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
            Some(other) => Err(Error::Validation(format!(
                "invalid sort value: \"{other}\" (expected: new, top)"
            ))),
        }
    }
}

/// İç içe bir yorum düğümü: içeriğin kendisi + doğrudan yanıtları.
///
/// Ağaç `actos-api` tarafında değil burada kuruluyor: `ltree` satırlarını
/// ebeveyn/çocuk ilişkisine çevirmek veri modelinin bir parçası, HTTP
/// temsilinin değil.
#[derive(Debug, Clone)]
pub struct CommentNode {
    pub content: Content,
    pub replies: Vec<CommentNode>,
}

/// [`Content`]'in yorum sorgularında kullanılan satır karşılığı.
///
/// `crate::content::ContentRow`'un kopyası değil, iki farkı var.
///
/// **Fazladan `parent_content_id`:** ağacı uygulama tarafında kurmak için
/// gerekli tek bağ bu. `depth` ise bilerek seçilmiyor — derinlik sınırı
/// tamamen SQL tarafında (`contents.depth <= $n`) uygulandığı için Rust'ta
/// okunmayan bir sütunu taşımanın anlamı yok.
///
/// **Etiket `JOIN`'i yok:** etiketler yalnızca post'lara bağlanıyor
/// (`crate::content::attach_tags` sadece `create_post`'tan çağrılıyor),
/// dolayısıyla her yorum satırında `content_tags` üzerinden geçmek
/// karşılıksız bir birleştirme maliyeti olurdu — [`Content::tags`] sabit
/// boş dizi olarak dolduruluyor.
struct CommentRow {
    id: i64,
    parent_content_id: Option<i64>,
    body: String,
    body_format: BodyFormat,
    metadata: serde_json::Value,
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
    author_trust_level: i16,
    author_deleted_at: Option<DateTime<Utc>>,
}

impl From<CommentRow> for Content {
    fn from(row: CommentRow) -> Self {
        Self {
            id: row.id,
            content_type: ContentType::Comment,
            author: ActorRecord {
                id: row.author_id,
                username: row.author_username,
                actor_type: row.author_actor_type,
                display_name: row.author_display_name,
                bio: row.author_bio,
                created_at: row.author_created_at,
                trust_level: row.author_trust_level,
            },
            author_deleted: row.author_deleted_at.is_some(),
            title: None,
            body: row.body,
            body_format: row.body_format,
            metadata: row.metadata,
            tags: Vec::new(),
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

/// [`CommentRow`]'dan cursor'ın sıralama anahtarını türetir.
fn row_sort_key(sort: CommentSort, row: &CommentRow) -> SortKey {
    match sort {
        CommentSort::New => SortKey::New {
            created_at: row.created_at,
        },
        CommentSort::Top => SortKey::Top { score: row.score },
    }
}

/// Bir `Top` cursor'ını `(score, id)` bindable çiftine ayırır.
///
/// `crate::actor::split_new_cursor`'ın `Top` karşılığı. `None` ise her iki
/// değer de `None` döner ve SQL tarafındaki `$n::int IS NULL` koşulu ilk
/// sayfayı üretir.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse (ör. `?sort=top` ile
/// `New` cursor'ı gönderilmişse) [`Error::InvalidCursor`] — sessizce
/// yok saymak, istemciye sayfaların ortasında sessizce sıçrayan bir liste
/// verirdi.
fn split_top_cursor(cursor: Option<Cursor>) -> Result<(Option<i32>, Option<i64>)> {
    match cursor {
        None => Ok((None, None)),
        Some(Cursor {
            sort: SortKey::Top { score },
            id,
        }) => Ok((Some(score), Some(id))),
        Some(_) => Err(Error::InvalidCursor),
    }
}

/// [`split_new_cursor`]'ın bu modüldeki karşılığı; cursor türü listenin
/// sıralamasıyla uyuşmuyorsa hata döner (bkz. [`split_top_cursor`]).
fn split_new_cursor_checked(
    cursor: Option<Cursor>,
) -> Result<(Option<DateTime<Utc>>, Option<i64>)> {
    match cursor {
        None
        | Some(Cursor {
            sort: SortKey::New { .. },
            ..
        }) => Ok(split_new_cursor(cursor)),
        Some(_) => Err(Error::InvalidCursor),
    }
}

// --- Yorum oluşturma -------------------------------------------------------

/// Bir yorumun ebeveyni hakkında [`create_comment`]'in ihtiyaç duyduğu
/// bilgi: nereye takılacağı (id/depth) **ve** bildirim fan-out'u için kimin
/// yazarı olduğu.
///
/// `root_author_id`/`parent_author_id` burada bilerek birlikte taşınıyor
/// (ayrı bir sorguya çıkmak yerine): [`resolve_parent`] zaten hem kök
/// post'u hem (varsa) doğrudan ebeveyn yorumu `FOR UPDATE` ile okuyor, o
/// satırların `actor_id`'sini de aynı sorgudan almak bedelsiz — bildirim
/// fan-out'u için content.rs'e ikinci bir gidiş-dönüş gerekmiyor.
struct ParentInfo {
    id: i64,
    depth: i32,
    /// Kök post'un yazarı — `comment_on_post` bildiriminin alıcısı.
    root_author_id: i64,
    /// Doğrudan ebeveynin (post ya da yorum) yazarı — ebeveyn bir yorumsa
    /// `reply_to_comment` bildiriminin alıcısı; ebeveyn post'un kendisiyse
    /// `root_author_id` ile aynı değer.
    parent_author_id: i64,
    /// `true` ise doğrudan ebeveyn post'un kendisi (yorum post'a direkt
    /// yazıldı), `false` ise ebeveyn ağaçtaki başka bir yorum. [`create_comment`]
    /// bunu, ayrı bir `reply_to_comment` bildirimi üretip üretmeyeceğine
    /// karar vermek için kullanıyor (bkz. "fan-out sınırı" — post'a direkt
    /// yazılan bir yorum için `comment_on_post` zaten aynı olayı anlatıyor,
    /// ikinci bir bildirime gerek yok).
    is_direct_child_of_post: bool,
}

/// `POST /posts/{id}/comments`: bir post'a ya da mevcut bir yoruma yanıt.
///
/// `parent_id` verilmezse yorum post'un doğrudan çocuğu olur; verilirse o
/// yorumun çocuğu olur ve yorumun **aynı post'un ağacına** ait olması
/// zorunludur (`root_post_id` kontrolü) — başka bir thread'deki bir yoruma
/// bu uçtan yanıt verilememeli.
///
/// Yeni yorum eklendikten sonra **tüm atalarının** (kök post dahil)
/// `comment_count`'u aynı transaction'da atomik olarak artırılır. Ata
/// satırları `ORDER BY id ... FOR UPDATE` ile kilitleniyor: aynı ağaca
/// eşzamanlı iki yorum eklendiğinde iki transaction'ın ata satırlarını
/// farklı sıralarda kilitlemesi kilitlenmeye (deadlock) yol açabilirdi;
/// sabit bir sıra bunu imkânsız kılıyor.
///
/// # Errors
/// Post yoksa/ebeveyn yorum yoksa [`Error::NotFound`]; post ya da ebeveyn
/// silinmişse [`Error::Gone`]; gövde doğrulamadan geçmezse ya da derinlik
/// sınırı aşılacaksa [`Error::Validation`]; veritabanı hatası
/// [`Error::Database`].
pub async fn create_comment(
    pool: &PgPool,
    author: &ActorRecord,
    post_id: i64,
    parent_id: Option<i64>,
    body: &str,
    attachment_ids: &[i64],
) -> Result<Content> {
    let body = text::validate_body(body).map_err(|e| Error::Validation(e.to_string()))?;
    if body.is_empty() {
        return Err(Error::Validation("comment body cannot be empty".to_owned()));
    }

    let mut tx = pool.begin().await?;

    let parent = resolve_parent(&mut tx, post_id, parent_id).await?;

    if parent.depth + 1 > MAX_COMMENT_DEPTH {
        return Err(Error::Validation(format!(
            "exceeds the maximum comment depth ({MAX_COMMENT_DEPTH})"
        )));
    }

    let inserted = sqlx::query!(
        r#"
        INSERT INTO contents (actor_id, parent_content_id, content_type, body, body_format)
        VALUES ($1, $2, 'comment'::content_type, $3, 'markdown'::body_format)
        RETURNING id
        "#,
        author.id,
        parent.id,
        body,
    )
    .fetch_one(&mut *tx)
    .await?;

    increment_ancestor_counts(&mut tx, inserted.id).await?;

    // Ekler aynı transaction'da (bkz. `crate::content::create_post`).
    crate::attachment::attach_to_content(&mut tx, inserted.id, author.id, attachment_ids).await?;

    // --- Bildirim fan-out'u: kök yazarı + doğrudan ebeveyn, BAŞKASI DEĞİL
    // (bkz. `crate::notification` modül dokümantasyonu "Fan-out sınırı"
    // bölümü). `notify_once` aynı actor'e (root == parent yazarıysa) iki kez
    // yazılmasını, `create_notification`'ın kendisi de yeni yorumun yazarına
    // (kendine bildirim) yazılmasını engelliyor — ikisi de burada elle tekrar
    // kontrol edilmiyor, merkezi.
    let mut notified = std::collections::HashSet::new();
    notification::notify_once(
        &mut tx,
        &mut notified,
        parent.root_author_id,
        NotificationKind::CommentOnPost,
        Some(author.id),
        "content",
        inserted.id,
        serde_json::json!({}),
    )
    .await?;
    // Ebeveyn post'un kendisiyse yukarıdaki `comment_on_post` zaten aynı
    // olayı anlatıyor — ayrıca bir `reply_to_comment` bildirimi ÜRETİLMEZ
    // (bkz. fan-out sınırı).
    if !parent.is_direct_child_of_post {
        notification::notify_once(
            &mut tx,
            &mut notified,
            parent.parent_author_id,
            NotificationKind::ReplyToComment,
            Some(author.id),
            "content",
            inserted.id,
            serde_json::json!({}),
        )
        .await?;
    }

    tx.commit().await?;

    get_comment(pool, inserted.id).await
}

/// [`create_comment`]'in ebeveyn çözümü: `parent_id` verilmişse o yorum,
/// verilmemişse post'un kendisi.
///
/// Satır `FOR UPDATE` ile kilitleniyor — kontrol ile `INSERT` arasında
/// ebeveynin silinmesi mümkün olmasın diye.
async fn resolve_parent(
    tx: &mut PgConnection,
    post_id: i64,
    parent_id: Option<i64>,
) -> Result<ParentInfo> {
    // Kök post her durumda kontrol ediliyor: `parent_id` bir yorum olsa
    // bile, silinmiş bir post'un ağacına yeni yorum eklenmemeli (trigger
    // da aynı kuralı uyguluyor, bkz. modül dokümantasyonu).
    let post = sqlx::query!(
        r#"
        SELECT id, depth, deleted_at, actor_id
        FROM contents
        WHERE id = $1 AND content_type = 'post'::content_type
        FOR UPDATE
        "#,
        post_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("post"))?;

    if post.deleted_at.is_some() {
        return Err(Error::Gone("post"));
    }

    let Some(parent_id) = parent_id else {
        return Ok(ParentInfo {
            id: post.id,
            depth: post.depth,
            root_author_id: post.actor_id,
            parent_author_id: post.actor_id,
            is_direct_child_of_post: true,
        });
    };

    // `parent_id == post_id` gönderen istemciyi reddetmiyoruz: anlamı
    // "post'un doğrudan çocuğu" ile aynı, yukarıda zaten kilitlenmiş
    // satırı ikinci kez sorgulamaya gerek yok.
    if parent_id == post_id {
        return Ok(ParentInfo {
            id: post.id,
            depth: post.depth,
            root_author_id: post.actor_id,
            parent_author_id: post.actor_id,
            is_direct_child_of_post: true,
        });
    }

    let parent = sqlx::query!(
        r#"
        SELECT id, depth, deleted_at, root_post_id, actor_id
        FROM contents
        WHERE id = $1 AND content_type = 'comment'::content_type
        FOR UPDATE
        "#,
        parent_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("comment"))?;

    // Başka bir post'un ağacındaki bir yoruma bu uçtan yanıt verilemez.
    // `NotFound`: "bu post'un altında böyle bir yorum yok" — yorumun başka
    // bir yerde var olduğu bilgisini sızdırmıyoruz.
    if parent.root_post_id != Some(post_id) {
        return Err(Error::NotFound("comment"));
    }

    if parent.deleted_at.is_some() {
        return Err(Error::Gone("comment"));
    }

    Ok(ParentInfo {
        id: parent.id,
        depth: parent.depth,
        root_author_id: post.actor_id,
        parent_author_id: parent.actor_id,
        is_direct_child_of_post: false,
    })
}

/// Yeni eklenen `content_id`'nin tüm atalarında (kök post dahil)
/// `comment_count`'u bir artırır.
///
/// Ata kümesi `ltree`'nin `@>` ("ata mı") operatörüyle bulunuyor; ayrı bir
/// özyinelemeli sorgu ya da uygulama tarafında zincir yürüme gerekmiyor.
/// Kilit sırası hakkında bkz. [`create_comment`].
async fn increment_ancestor_counts(tx: &mut PgConnection, content_id: i64) -> Result<()> {
    sqlx::query!(
        r#"
        UPDATE contents
        SET comment_count = comment_count + 1
        WHERE id IN (
            SELECT ata.id
            FROM contents AS ata
            JOIN contents AS yeni ON yeni.id = $1
            WHERE ata.path @> yeni.path AND ata.id <> yeni.id
            ORDER BY ata.id
            FOR UPDATE OF ata
        )
        "#,
        content_id,
    )
    .execute(&mut *tx)
    .await?;

    Ok(())
}

// --- Tek yorum okuma -------------------------------------------------------

/// `GET /comments/{id}`: tek bir yorum.
///
/// [`crate::content::get_post`]'un aksine **silinmiş yorum `410` DÖNMEZ**,
/// `deleted_at` dolu olarak döner. Gerekçe: silinen bir yorumun çocukları
/// yaşamaya devam ediyor ve breadcrumb/ağaç bağlamlarında o düğümün
/// `[deleted]` olarak görünmesi gerekiyor (bkz. modül dokümantasyonu). Bu
/// fonksiyonu çağıran HTTP katmanı gövdeyi maskelemekten sorumlu —
/// `actos-api`'deki `content_summary` bunu `deleted_at`'e bakarak zaten
/// yapıyor, maskeleme çağıranın unutabileceği bir adım değil.
///
/// # Errors
/// Yorum yoksa [`Error::NotFound`]; veritabanı hatası [`Error::Database`].
pub async fn get_comment(pool: &PgPool, id: i64) -> Result<Content> {
    let row = sqlx::query_as!(
        CommentRow,
        r#"
        SELECT
            contents.id,
            contents.parent_content_id,
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
            actors.trust_level AS author_trust_level,
            actors.deleted_at AS author_deleted_at
        FROM contents
        JOIN actors ON actors.id = contents.actor_id
        WHERE contents.id = $1 AND contents.content_type = 'comment'::content_type
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("comment"))?;

    Ok(row.into())
}

/// `GET /comments/{id}`'in breadcrumb'ı: bir yorumun kökten kendisine kadar
/// olan ata zinciri (kök post dahil, yorumun kendisi hariç).
///
/// `ltree`'nin `@>` operatörü + `ORDER BY depth` ile **tek sorguda**
/// getiriliyor; ebeveyn zincirini adım adım yürüyen N sorgu yok.
///
/// Zincirdeki ata bir post olabileceği için satırlar
/// [`crate::content::Content`]'e `content_type` korunarak çevriliyor —
/// breadcrumb'ın ilk öğesi her zaman post'tur.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn ancestors_of(pool: &PgPool, id: i64) -> Result<Vec<Content>> {
    struct AncestorRow {
        id: i64,
        content_type: ContentType,
        title: Option<String>,
        body: String,
        body_format: BodyFormat,
        metadata: serde_json::Value,
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
        author_trust_level: i16,
        author_deleted_at: Option<DateTime<Utc>>,
    }

    let rows = sqlx::query_as!(
        AncestorRow,
        r#"
        SELECT
            ata.id,
            ata.content_type AS "content_type: ContentType",
            ata.title,
            ata.body,
            ata.body_format AS "body_format: BodyFormat",
            ata.metadata,
            ata.score,
            ata.upvotes,
            ata.downvotes,
            ata.comment_count,
            ata.hot_score,
            ata.created_at,
            ata.edited_at,
            ata.deleted_at,
            actors.id AS author_id,
            actors.username AS author_username,
            actors.actor_type AS "author_actor_type: ActorType",
            actors.display_name AS author_display_name,
            actors.bio AS author_bio,
            actors.created_at AS author_created_at,
            actors.trust_level AS author_trust_level,
            actors.deleted_at AS author_deleted_at
        FROM contents AS ata
        JOIN contents AS hedef ON hedef.id = $1
        JOIN actors ON actors.id = ata.actor_id
        WHERE ata.path @> hedef.path AND ata.id <> hedef.id
        ORDER BY ata.depth
        "#,
        id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| Content {
            id: row.id,
            content_type: row.content_type,
            author: ActorRecord {
                id: row.author_id,
                username: row.author_username,
                actor_type: row.author_actor_type,
                display_name: row.author_display_name,
                bio: row.author_bio,
                created_at: row.author_created_at,
                trust_level: row.author_trust_level,
            },
            author_deleted: row.author_deleted_at.is_some(),
            title: row.title,
            body: row.body,
            body_format: row.body_format,
            metadata: row.metadata,
            tags: Vec::new(),
            score: row.score,
            upvotes: row.upvotes,
            downvotes: row.downvotes,
            comment_count: row.comment_count,
            hot_score: row.hot_score,
            created_at: row.created_at,
            edited_at: row.edited_at,
            deleted_at: row.deleted_at,
        })
        .collect())
}

// --- Yorum ağacı listeleme -------------------------------------------------

/// Bir yorum ağacının hangi düğümünden başlanacağı.
struct TreeRoot {
    id: i64,
    depth: i32,
}

/// `GET /posts/{id}/comments`: bir post'un (ya da `?parent=` ile bir alt
/// ağacın) yorumları, iç içe.
///
/// **İki sorgu, ağaç uygulamada kuruluyor** (PLAN.md Faz 9'un tarif ettiği
/// desen): (1) kökün doğrudan çocukları cursor'lu olarak sayfalanır,
/// (2) o sayfadaki çocukların altındaki bütün torunlar `path <@ ANY(...)`
/// ile tek seferde çekilir. Sayfalamanın **yalnızca üst seviye yorumlara**
/// uygulanması bilinçli: bir yanıtın ortasından kesilmiş bir alt ağaç
/// istemciye anlamsız gelirdi, oysa "ilk 25 üst yorum, her biri altındaki
/// yanıtlarıyla" doğrudan çizilebilir bir birim.
///
/// `depth`, döndürülen üst seviye yorumların **altındaki** kaç seviye
/// yanıtın dahil edileceğini söyler; daha derini `?parent=<id>` ile ayrıca
/// çekilir ("daha fazla yanıt yükle").
///
/// **Silinmiş yorumlar listeden düşürülmez**, `deleted_at` dolu olarak
/// döner ve HTTP katmanında `[deleted]` olarak maskelenir — çocukları
/// yaşamaya devam ettiği için düğümü ağaçtan çıkarmak alt ağacı yetim
/// bırakırdı.
///
/// # Errors
/// Post yoksa [`Error::NotFound`]; post silinmişse [`Error::Gone`];
/// `parent` bu post'un ağacına ait değilse [`Error::NotFound`]; cursor
/// listenin sıralamasına ait değilse [`Error::InvalidCursor`]; veritabanı
/// hatası [`Error::Database`].
pub async fn list_comment_tree(
    pool: &PgPool,
    post_id: i64,
    parent: Option<i64>,
    sort: CommentSort,
    depth: i32,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<CommentNode>> {
    let root = resolve_tree_root(pool, post_id, parent).await?;
    let max_depth = root.depth + 1 + depth.clamp(0, MAX_TREE_DEPTH);

    let children = fetch_children_page(pool, root.id, sort, cursor, limit).await?;

    let child_ids: Vec<i64> = children.items.iter().map(|row| row.id).collect();
    let descendants = fetch_descendants(pool, &child_ids, sort, max_depth).await?;

    let items = build_forest(children.items, descendants);

    Ok(Page {
        items,
        next_cursor: children.next_cursor,
    })
}

/// [`list_comment_tree`]'nin kök çözümü. `parent` verilmişse o yorum,
/// verilmemişse post.
///
/// Silinmiş bir `parent` kabul ediliyor: yorum silinse de çocukları
/// yaşıyor, "daha fazla yanıt yükle" o alt ağaçta da çalışmalı. Silinmiş
/// **post** ise reddediliyor — orada gösterilecek bir thread yok.
async fn resolve_tree_root(pool: &PgPool, post_id: i64, parent: Option<i64>) -> Result<TreeRoot> {
    let post = sqlx::query!(
        r#"
        SELECT id, depth, deleted_at
        FROM contents
        WHERE id = $1 AND content_type = 'post'::content_type
        "#,
        post_id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("post"))?;

    if post.deleted_at.is_some() {
        return Err(Error::Gone("post"));
    }

    let Some(parent_id) = parent else {
        return Ok(TreeRoot {
            id: post.id,
            depth: post.depth,
        });
    };

    if parent_id == post_id {
        return Ok(TreeRoot {
            id: post.id,
            depth: post.depth,
        });
    }

    let parent = sqlx::query!(
        r#"
        SELECT id, depth, root_post_id
        FROM contents
        WHERE id = $1 AND content_type = 'comment'::content_type
        "#,
        parent_id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("comment"))?;

    if parent.root_post_id != Some(post_id) {
        return Err(Error::NotFound("comment"));
    }

    Ok(TreeRoot {
        id: parent.id,
        depth: parent.depth,
    })
}

/// Kökün doğrudan çocuklarının cursor'lu sayfası.
///
/// İki ayrı `query_as!` çağrısı var çünkü `sqlx` derleme zamanı doğrulaması
/// için sorgunun bir string **literal** olmasını şart koşuyor; `ORDER BY`
/// ve cursor koşulu sıralamaya göre değiştiği için tek bir metinle ifade
/// edilemiyor (dinamik string birleştirme bu doğrulamayı ve `sqlx prepare`
/// önbelleğini bozardı).
async fn fetch_children_page(
    pool: &PgPool,
    root_id: i64,
    sort: CommentSort,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<CommentRow>> {
    let rows = match sort {
        CommentSort::New => {
            let (cursor_created_at, cursor_id) = split_new_cursor_checked(cursor)?;
            sqlx::query_as!(
                CommentRow,
                r#"
                SELECT
                    contents.id,
                    contents.parent_content_id,
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
                    actors.trust_level AS author_trust_level,
                    actors.deleted_at AS author_deleted_at
                FROM contents
                JOIN actors ON actors.id = contents.actor_id
                WHERE contents.parent_content_id = $1
                  AND contents.content_type = 'comment'::content_type
                  AND (
                      $2::timestamptz IS NULL
                      OR (contents.created_at, contents.id) < ($2::timestamptz, $3::bigint)
                  )
                ORDER BY contents.created_at DESC, contents.id DESC
                LIMIT $4
                "#,
                root_id,
                cursor_created_at,
                cursor_id,
                limit + 1,
            )
            .fetch_all(pool)
            .await?
        }
        CommentSort::Top => {
            let (cursor_score, cursor_id) = split_top_cursor(cursor)?;
            sqlx::query_as!(
                CommentRow,
                r#"
                SELECT
                    contents.id,
                    contents.parent_content_id,
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
                    actors.trust_level AS author_trust_level,
                    actors.deleted_at AS author_deleted_at
                FROM contents
                JOIN actors ON actors.id = contents.actor_id
                WHERE contents.parent_content_id = $1
                  AND contents.content_type = 'comment'::content_type
                  AND (
                      $2::int IS NULL
                      OR (contents.score, contents.id) < ($2::int, $3::bigint)
                  )
                ORDER BY contents.score DESC, contents.id DESC
                LIMIT $4
                "#,
                root_id,
                cursor_score,
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
        |row: &CommentRow| row.id,
        |row: &CommentRow| row_sort_key(sort, row),
        |row| row,
    ))
}

/// Sayfadaki üst seviye yorumların altındaki bütün torunları **tek
/// sorguda** getirir.
///
/// Ata kümesi `ltree`'nin `<@` operatörüyle ifade ediliyor; `ARRAY(SELECT
/// ...)` alt sorgusu sayesinde `ltree` değerleri Rust tarafına hiç
/// çıkmıyor — yalnızca `bigint[]` bağlanıyor.
async fn fetch_descendants(
    pool: &PgPool,
    child_ids: &[i64],
    sort: CommentSort,
    max_depth: i32,
) -> Result<Vec<CommentRow>> {
    if child_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = match sort {
        CommentSort::New => {
            sqlx::query_as!(
                CommentRow,
                r#"
                SELECT
                    contents.id,
                    contents.parent_content_id,
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
                    actors.trust_level AS author_trust_level,
                    actors.deleted_at AS author_deleted_at
                FROM contents
                JOIN actors ON actors.id = contents.actor_id
                WHERE contents.path <@ ANY(
                          ARRAY(SELECT ust.path FROM contents AS ust WHERE ust.id = ANY($1::bigint[]))
                      )
                  AND contents.id <> ALL($1::bigint[])
                  AND contents.depth <= $2
                ORDER BY contents.created_at DESC, contents.id DESC
                "#,
                child_ids,
                max_depth,
            )
            .fetch_all(pool)
            .await?
        }
        CommentSort::Top => {
            sqlx::query_as!(
                CommentRow,
                r#"
                SELECT
                    contents.id,
                    contents.parent_content_id,
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
                    actors.trust_level AS author_trust_level,
                    actors.deleted_at AS author_deleted_at
                FROM contents
                JOIN actors ON actors.id = contents.actor_id
                WHERE contents.path <@ ANY(
                          ARRAY(SELECT ust.path FROM contents AS ust WHERE ust.id = ANY($1::bigint[]))
                      )
                  AND contents.id <> ALL($1::bigint[])
                  AND contents.depth <= $2
                ORDER BY contents.score DESC, contents.id DESC
                "#,
                child_ids,
                max_depth,
            )
            .fetch_all(pool)
            .await?
        }
    };

    Ok(rows)
}

/// Düz satır listelerinden iç içe ağacı kurar.
///
/// `roots` sıralamasını **korur** (SQL'den geldiği sırayla), torunlar da
/// aynı sırada eklenir — sıralama kararı SQL'de verildi, burada yeniden
/// sıralanmıyor.
///
/// Ebeveyni bu kümede olmayan bir torun (ör. derinlik sınırı yüzünden
/// arada bir seviye eksik kaldıysa) sessizce atılır: yarım bir zincirin
/// köküne tutturulmuş bir düğüm istemciye yanlış bir hiyerarşi gösterirdi.
fn build_forest(roots: Vec<CommentRow>, descendants: Vec<CommentRow>) -> Vec<CommentNode> {
    use std::collections::HashMap;

    // Ebeveyn id -> çocuk satırları (SQL sırası korunarak).
    let mut by_parent: HashMap<i64, Vec<CommentRow>> = HashMap::new();
    for row in descendants {
        if let Some(parent_id) = row.parent_content_id {
            by_parent.entry(parent_id).or_default().push(row);
        }
    }

    fn attach(row: CommentRow, by_parent: &mut HashMap<i64, Vec<CommentRow>>) -> CommentNode {
        let id = row.id;
        let children = by_parent.remove(&id).unwrap_or_default();
        CommentNode {
            content: row.into(),
            replies: children
                .into_iter()
                .map(|child| attach(child, by_parent))
                .collect(),
        }
    }

    roots
        .into_iter()
        .map(|row| attach(row, &mut by_parent))
        .collect()
}

// --- Yorum güncelleme / silme ----------------------------------------------

/// `PATCH /comments/{id}`: yalnızca sahibi, gövde güncellemesi.
///
/// [`crate::content::update_post`] ile aynı üç adım (kilitle → eski hâli
/// `edit_history`'ye yaz → güncelle), tek transaction. Yorumların `title`'ı
/// olmadığı için `previous_title` her zaman `NULL` yazılır.
///
/// # Errors
/// Yorum yoksa [`Error::NotFound`]; silinmişse [`Error::Gone`]; çağıran
/// sahibi değilse [`Error::Forbidden`]; gövde doğrulamadan geçmezse
/// [`Error::Validation`]; veritabanı hatası [`Error::Database`].
pub async fn update_comment(pool: &PgPool, id: i64, actor_id: i64, body: &str) -> Result<Content> {
    let body = text::validate_body(body).map_err(|e| Error::Validation(e.to_string()))?;
    if body.is_empty() {
        return Err(Error::Validation("comment body cannot be empty".to_owned()));
    }

    let mut tx = pool.begin().await?;

    let current = sqlx::query!(
        r#"
        SELECT actor_id, body, deleted_at
        FROM contents
        WHERE id = $1 AND content_type = 'comment'::content_type
        FOR UPDATE
        "#,
        id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("comment"))?;

    if current.deleted_at.is_some() {
        return Err(Error::Gone("comment"));
    }
    if current.actor_id != actor_id {
        return Err(Error::Forbidden);
    }

    sqlx::query!(
        r#"
        INSERT INTO edit_history (content_id, previous_title, previous_body)
        VALUES ($1, NULL, $2)
        "#,
        id,
        current.body,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"UPDATE contents SET body = $2, edited_at = now() WHERE id = $1"#,
        id,
        body,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    get_comment(pool, id).await
}

/// `DELETE /comments/{id}`: sahibi **veya** moderatör/admin. Soft-delete.
///
/// Satır ağaçta kalır (çocukları yetim kalmasın diye) ve `comment_count`
/// azaltılmaz — gerekçe modül dokümantasyonunda.
///
/// Yetki sırası [`crate::content::delete_post`] ile aynı: `404` → `410` →
/// `403`.
///
/// # Errors
/// Yorum yoksa [`Error::NotFound`]; zaten silinmişse [`Error::Gone`];
/// çağıran ne sahibi ne moderatör/admin ise [`Error::Forbidden`];
/// veritabanı hatası [`Error::Database`].
pub async fn delete_comment(
    pool: &PgPool,
    id: i64,
    actor_id: i64,
    roles: &[AdminRole],
) -> Result<()> {
    let mut tx = pool.begin().await?;

    let current = sqlx::query!(
        r#"
        SELECT actor_id, deleted_at
        FROM contents
        WHERE id = $1 AND content_type = 'comment'::content_type
        FOR UPDATE
        "#,
        id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("comment"))?;

    if current.deleted_at.is_some() {
        return Err(Error::Gone("comment"));
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

// --- Actor'e göre yorum listesi (Faz 7'den devir) --------------------------

/// `GET /actors/{username}/comments`: bir actor'ün yorumları, en yeni önce.
///
/// [`crate::content::list_posts_by_actor`] ile aynı desen ve aynı karar:
/// **silinmiş yorumlar bu listede görünmez.** Ağaç bağlamında silinmiş bir
/// düğüm `[deleted]` olarak duruyor çünkü çocuklarını taşıyor; bir actor'ün
/// "yazdıkları" listesinde ise taşıyacağı bir şey yok, orada silinmiş bir
/// satır yalnızca gürültü olurdu.
///
/// # Errors
/// `username` hiç yoksa [`Error::NotFound`]; actor silinmişse
/// [`Error::Gone`]; veritabanı hatası [`Error::Database`].
pub async fn list_comments_by_actor(
    pool: &PgPool,
    username: &str,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Content>> {
    let actor_id = resolve_live_actor_id(pool, username).await?;
    let (cursor_created_at, cursor_id) = split_new_cursor_checked(cursor)?;

    let rows = sqlx::query_as!(
        CommentRow,
        r#"
        SELECT
            contents.id,
            contents.parent_content_id,
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
            actors.trust_level AS author_trust_level,
            actors.deleted_at AS author_deleted_at
        FROM contents
        JOIN actors ON actors.id = contents.actor_id
        WHERE contents.actor_id = $1
          AND contents.content_type = 'comment'::content_type
          AND contents.deleted_at IS NULL
          AND (
              $2::timestamptz IS NULL
              OR (contents.created_at, contents.id) < ($2::timestamptz, $3::bigint)
          )
        ORDER BY contents.created_at DESC, contents.id DESC
        LIMIT $4
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
        |row: &CommentRow| row.id,
        |row: &CommentRow| SortKey::New {
            created_at: row.created_at,
        },
        Content::from,
    ))
}
