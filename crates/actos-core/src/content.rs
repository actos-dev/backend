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

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::{
    actor::{Page, paginate, resolve_live_actor_id, split_new_cursor},
    auth::{ActorRecord, ActorType, Grant, Permission},
    community::{CommunityRef, CommunityVisibility},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    id::IdCodec,
    storage::Storage,
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
    /// Faz 12'de periyodik olarak hesaplanacak "sıcaklık" değeri; o zamana
    /// kadar şema varsayılanı olan `0`. `PostSort::Hot` sıralamasının ve
    /// onun cursor'ının dayandığı alan (bkz. [`PostSort::Hot`]).
    pub hot_score: f64,
    /// Bu içeriğin ait olduğu topluluk; `None` bağımsız bir post demektir
    /// (COMMUNITY_PLAN.md §1 — topluluksuz post "aşağı" bir tür değil).
    ///
    /// Yorumlarda Faz 2'de her zaman `None` (yorumlar topluluklara ait
    /// değil); kolon yine de satırdan okunuyor ki ileride bir yorum
    /// topluluğa bağlanırsa bu alan kendiliğinden dolsun.
    pub community: Option<CommunityRef>,
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
    /// Bu içerik bir çapraz-gönderi ise kaynağın iç kimliği; `None` normal
    /// bir post. Çapraz-gönderi bir **kopya değil referanstır** (§8): satır
    /// yalnızca kaynağın id'sini taşır, kart okuma anında okuyucunun
    /// izinleriyle çözülür. Yorumlarda her zaman `None` (şemadaki
    /// `ck_contents_cross_post_is_post`).
    pub cross_post_source_id: Option<i64>,
    /// Kaynağın bu okuyucu için çözülmüş hâli — bkz. [`resolve_cross_posts`].
    ///
    /// `cross_post_source_id` dolu olup burası `None` ise kaynak erişilemez
    /// demektir: silinmiş ya da okuyucunun göremediği özel bir toplulukta —
    /// ikisi bilerek ayrılmaz, gerekçe açıklanmaz (§8). Liste/tekil okuma
    /// fonksiyonları doldurur; [`create_post`] yeni oluşturulan gönderi için
    /// hemen çözer.
    pub cross_post: Option<CrossPostPreview>,
}

/// Bir çapraz-gönderinin kaynağının, okuyucuya çözülmüş hâli.
///
/// **`deleted` diye bir alan yok:** silinmiş bir kaynak [`Content::cross_post`]'u
/// `None` yapar (mezar taşı); `Some` içinde asla "silinmiş" taşınmaz. Böylece
/// istemci tek bir `None` kontrolüyle aynı kartı çizer ve iki sebep (silinme
/// / görünmez özel topluluk) yapısal olarak ayırt edilemez (§8).
#[derive(Debug, Clone)]
pub struct CrossPostPreview {
    pub source_id: i64,
    pub title: Option<String>,
    pub author: ActorRecord,
    /// Kaynağın yazarı silinmişse `true`; maskeleme kararı [`Content`]'in
    /// `author_deleted` alanıyla birebir aynı gerekçeyle HTTP katmanında
    /// veriliyor (bkz. `actos-api/src/routes/posts.rs` → `masked_actor_summary`).
    pub author_deleted: bool,
    pub community: Option<CommunityRef>,
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
    community_id: Option<i64>,
    community_name: Option<String>,
    tags: Vec<String>,
    cross_post_source_id: Option<i64>,
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
            hot_score: row.hot_score,
            community: row.community_id.map(|id| CommunityRef {
                id,
                name: row.community_name.unwrap_or_default(),
            }),
            created_at: row.created_at,
            edited_at: row.edited_at,
            deleted_at: row.deleted_at,
            cross_post_source_id: row.cross_post_source_id,
            cross_post: None,
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
/// ## `cross_post_source` (COMMUNITY_PLAN.md §8, Faz 5)
///
/// `Some(source_id)` ise bu bir **çapraz-gönderi**dir: satır kaynağın
/// yalnızca id'sini taşır, `title = NULL`, `body = ''`, etiketsiz. Bu
/// yolda başlık/gövde/etiket doğrulaması **atlanır** (gönderi kendi
/// içeriğini taşımıyor). Kaynak yüklenirken sırasıyla:
///
/// 1. yaratıcının göremediği (`content_visible_to`, yaratıcının kendi
///    görünür topluluk kümesiyle) ya da hiç var olmayan kaynak →
///    [`Error::NotFound`];
/// 2. post olmayan ya da kendisi de bir çapraz-gönderi olan kaynak →
///    [`Error::Validation`] (derinlik sınırı **tek seviye**; zincir yok);
/// 3. özel bir topluluktaki kaynak → [`Error::Forbidden`], yaratıcı o
///    topluluğun üyesi olsa bile ("özel topluluktan hiçbir şey çıkmaz");
/// 4. silinmiş kaynak → [`Error::Gone`].
///
/// Yaratıcının görünür kümesi yalnızca çapraz-gönderi yolunda hesaplanır;
/// normal post sıfır ek sorgu öder.
///
/// `path`/`depth`/`root_post_id`'ye dokunulmuyor (bkz. modül
/// dokümantasyonu) — `INSERT`, `trg_contents_set_path` trigger'ının
/// bunları hesaplamasına bırakılıyor.
///
/// `files` (raw, not-yet-validated bytes — zero to
/// [`crate::attachment::MAX_ATTACHMENTS_PER_CONTENT`] of them) are validated,
/// normalized, quota-checked, and inserted as `attachments` rows **in the
/// same transaction** as the post itself (see
/// [`crate::attachment::create_for_content`]) — there is no separate
/// upload-then-attach step any more (REFACTOR.md §4). An empty `files` is
/// the common case (a plain text post) and costs nothing beyond the empty
/// check itself.
///
/// # Errors
/// `title`/`body`/`tags` doğrulamadan geçmezse ya da kaynak geçersizse
/// [`Error::Validation`]; hedef topluluk kuralları ihlâl edilirse
/// [`Error::Forbidden`]/[`Error::Banned`]; kaynak yok/görünmezse
/// [`Error::NotFound`]; kaynak silinmişse [`Error::Gone`]; a file fails
/// validation, exceeds the per-file limit, or the batch would exceed the
/// storage quota: [`Error::Validation`] / [`Error::UnsupportedMedia`] (see
/// [`crate::attachment::create_for_content`]); veritabanı hatası
/// [`Error::Database`].
#[allow(clippy::too_many_arguments)]
pub async fn create_post(
    pool: &PgPool,
    storage: &Storage,
    id_codec: &IdCodec,
    author: &ActorRecord,
    community: Option<&str>,
    title: &str,
    body: &str,
    tags: &[String],
    files: &[Vec<u8>],
    max_file_bytes: usize,
    quota_bytes: i64,
    cross_post_source: Option<i64>,
) -> Result<Content> {
    // Çapraz-gönderi yolu doğrulamayı atlar (yukarıdaki doküman): başlık
    // kaynaktan çözülür, gövde boş, etiket yok. Normal yol eski hâliyle
    // aynen işliyor.
    let (title, body, tags) = match cross_post_source {
        None => {
            let title =
                text::validate_title(title).map_err(|e| Error::Validation(e.to_string()))?;
            if title.is_empty() {
                return Err(Error::Validation("post title cannot be empty".to_owned()));
            }
            let body = text::validate_body(body).map_err(|e| Error::Validation(e.to_string()))?;
            (Some(title), body, normalize_tags(tags)?)
        }
        Some(_) => (None, String::new(), Vec::new()),
    };

    // Yaratıcının görünür toplulukları yalnızca kaynak kapısı için gerekli;
    // normal post bu sorguyu hiç atmıyor. `visible_community_ids` transaction
    // dışında çalışıyor (havuz istiyor) — üyelik ile INSERT arasındaki
    // teorik yarış, `resolve_community_id_in` ile aynı sınıfta ve kabul
    // edilebilir.
    let creator_communities = match cross_post_source {
        None => Vec::new(),
        Some(_) => crate::visibility::visible_community_ids(pool, Some(author.id)).await?,
    };

    let mut tx = pool.begin().await?;

    if let Some(source_id) = cross_post_source {
        // Kaynak sorgusu görünürlük kapısını içeriyor: görünmeyen bir satır
        // hiç dönmez, dolayısıyla ayrı bir "görünür mü" kontrolü yok (§9).
        let source = sqlx::query!(
            r#"
            SELECT
                kaynak.content_type AS "content_type: ContentType",
                kaynak.deleted_at,
                kaynak.cross_post_source_id,
                topluluk.visibility AS "community_visibility?: CommunityVisibility"
            FROM contents AS kaynak
            LEFT JOIN communities AS topluluk ON topluluk.id = kaynak.community_id
            WHERE kaynak.id = $1
              AND content_visible_to(kaynak.community_id, $2::bigint[])
            "#,
            source_id,
            &creator_communities,
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound("post"))?;

        // Derinlik sınırı: yorum ya da başka bir çapraz-gönderi kaynak
        // olamaz — yoksa zincir oluşur (§8).
        if source.content_type != ContentType::Post || source.cross_post_source_id.is_some() {
            return Err(Error::Validation(
                "the cross-post source must be a post that is not itself a cross-post".to_owned(),
            ));
        }

        // Özel topluluktan hiçbir şey çıkmaz (§8) — yaratıcı üye olsa bile.
        // Bu kontrol görünürlükten SONRA: üye olmayan bir yaratıcı zaten
        // yukarıda `NotFound` alıp topluluğun varlığını öğrenemiyor.
        if source.community_visibility == Some(CommunityVisibility::Private) {
            return Err(Error::Forbidden);
        }

        if source.deleted_at.is_some() {
            return Err(Error::Gone("post"));
        }
    }

    // Topluluk isteğe bağlıdır: verilmediyse post bağımsızdır. Verildiyse
    // topluluk **var olmalı** (yoksa 404) ve yazar **üye olmalı** (değilse
    // 403) — okuma için üyelik gerekmez ama yazmak için her zaman gerekir
    // (COMMUNITY_PLAN.md §3). Kontroller INSERT'ten önce, aynı transaction
    // içinde; yazma ile üyelik kontrolü arasında ayrılma yarışı olmasın.
    //
    // Yanıt DTO'su için saklanan (normalize edilmiş) ismi de okuyoruz: `name`
    // citext olduğu için büyük/küçük harf farkıyla da eşleşebilirdi, ama dış
    // referans DB'deki gerçek yazımı taşımalı.
    let community_ref: Option<(i64, String)> = match community {
        None => None,
        Some(name) => {
            let community_id = crate::community::resolve_community_id_in(&mut *tx, name).await?;
            // Topluluk ban'ı yazmayı engeller (COMMUNITY_PLAN.md §6). Üyelik
            // kontrolünden ÖNCE: ban üyeliği zaten sildiği için aksi hâlde
            // "üye değil" (403) dönüp gerçek sebebi gizlerdik.
            if crate::moderation::is_banned_from_community_in(&mut *tx, community_id, author.id)
                .await?
            {
                return Err(Error::Banned);
            }
            if !crate::community::is_member_in(&mut *tx, community_id, author.id).await? {
                return Err(Error::Forbidden);
            }
            let stored_name = sqlx::query_scalar!(
                r#"SELECT name::text AS "name!" FROM communities WHERE id = $1"#,
                community_id,
            )
            .fetch_one(&mut *tx)
            .await?;
            Some((community_id, stored_name))
        }
    };
    let community_id = community_ref.as_ref().map(|(id, _)| *id);

    let row = sqlx::query!(
        r#"
        INSERT INTO contents (actor_id, content_type, title, body, body_format, community_id, cross_post_source_id)
        VALUES ($1, 'post'::content_type, $2, $3, 'markdown'::body_format, $4, $5)
        RETURNING id, created_at, score, upvotes, downvotes, comment_count, hot_score
        "#,
        author.id,
        title,
        body,
        community_id,
        cross_post_source,
    )
    .fetch_one(&mut *tx)
    .await?;

    attach_tags(&mut tx, row.id, &tags).await?;

    // Attachments are created **in the same transaction**: if any file
    // fails validation or the batch would exceed the quota, the post must
    // not exist either — otherwise the client would end up with a
    // different post than the one it asked for (text without its images).
    crate::attachment::create_for_content(
        &mut tx,
        storage,
        id_codec,
        author.id,
        row.id,
        files,
        max_file_bytes,
        quota_bytes,
    )
    .await?;

    tx.commit().await?;

    let mut content = Content {
        id: row.id,
        content_type: ContentType::Post,
        author: author.clone(),
        author_deleted: false,
        title,
        body,
        body_format: BodyFormat::Markdown,
        tags,
        score: row.score,
        upvotes: row.upvotes,
        downvotes: row.downvotes,
        comment_count: row.comment_count,
        hot_score: row.hot_score,
        community: community_ref.map(|(id, name)| CommunityRef { id, name }),
        created_at: row.created_at,
        edited_at: None,
        deleted_at: None,
        cross_post_source_id: cross_post_source,
        cross_post: None,
    };

    // Yeni oluşturulan çapraz-gönderinin kaynağı, yukarıdaki kontrollerden
    // geçtiği için kesin görünür; kartı ilk `201` yanıtında da dolu vermek
    // için önizlemeyi hemen çözüyoruz (tek ek sorgu, yalnızca çapraz-gönderi
    // yolunda).
    if cross_post_source.is_some() {
        resolve_cross_posts(
            pool,
            &creator_communities,
            std::slice::from_mut(&mut content),
        )
        .await?;
    }

    Ok(content)
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
/// **Görünürlük sırası (Faz 4A):** varlık → görünürlük → silinmişlik.
/// Görünmeyen bir özel post [`Error::NotFound`] döner, `410` değil:
/// "silinmiş" demek postun var olduğunu söylerdi, oysa okuyucu onu hiç
/// görmemeli (COMMUNITY_PLAN.md §9). `content_visible_to` filtresi bu yüzden
/// silinmişlik kontrolünden **önce**, sorgunun kendisinde.
///
/// # Errors
/// Post yoksa (ya da `id` bir yoruma aitse — `content_type = 'post'`
/// filtresi bunu da `NotFound` sayar) ya da okuyucuya görünmüyorsa
/// [`Error::NotFound`]; silinmişse [`Error::Gone`]; veritabanı hatası
/// [`Error::Database`].
pub async fn get_post(pool: &PgPool, id: i64, viewer_communities: &[i64]) -> Result<Content> {
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
            contents.hot_score,
            contents.created_at,
            contents.edited_at,
            contents.deleted_at,
            contents.cross_post_source_id,
            actors.id AS author_id,
            actors.username AS author_username,
            actors.actor_type AS "author_actor_type: ActorType",
            actors.display_name AS author_display_name,
            actors.bio AS author_bio,
            actors.created_at AS author_created_at,
            actors.deleted_at AS author_deleted_at,
            communities.id AS "community_id?",
            communities.name AS "community_name?",
            COALESCE(
                array_agg(tags.name::text) FILTER (WHERE tags.id IS NOT NULL),
                '{}'
            ) AS "tags!: Vec<String>"
        FROM contents
        JOIN actors ON actors.id = contents.actor_id
        LEFT JOIN communities ON communities.id = contents.community_id
        LEFT JOIN content_tags ON content_tags.content_id = contents.id
        LEFT JOIN tags ON tags.id = content_tags.tag_id
        WHERE contents.id = $1 AND contents.content_type = 'post'::content_type
          AND content_visible_to(contents.community_id, $2::bigint[])
        GROUP BY contents.id, actors.id, communities.id
        "#,
        id,
        viewer_communities,
    )
    .fetch_optional(pool)
    .await?;

    let row = row.ok_or(Error::NotFound("post"))?;

    if row.deleted_at.is_some() {
        return Err(Error::Gone("post"));
    }

    let mut content: Content = row.into();
    resolve_cross_posts(pool, viewer_communities, std::slice::from_mut(&mut content)).await?;
    Ok(content)
}

/// Bir sayfadaki çapraz-gönderilerin kaynaklarını **tek sorguda** çözer:
/// önce sayfadaki farklı `cross_post_source_id`'ler toplanır, kaynaklar
/// `id = ANY(...)` ile bir kez yüklenir, sonra her içeriğe kendi önizlemesi
/// yazılır. Kaynak başına ayrı sorgu (N+1) yok.
///
/// Erişilemeyen kaynak [`Content::cross_post`]'u `None` yapar — mezar taşı:
///
///   * kaynak soft-delete edilmişse,
///   * kaynak özel bir toplulukta ve o topluluk `viewer_communities`'te
///     yoksa,
///   * kaynak hiç yoksa (hard delete).
///
/// Üçü **bilerek ayırt edilmez**; ayrımı sızdırmak özel içeriğin varlığını
/// ele verirdi (COMMUNITY_PLAN.md §8).
///
/// `viewer_communities` çağıranın zaten geçirdiği görünür topluluk kümesi.
/// Public yüzeyler `'{}'` geçtiği için (§9) orada özel kaynaklar koşulsuz
/// mezar taşıdır; okuyucunun kendi listeleri gerçek kümeyi geçirir.
///
/// Bu fonksiyon yalnızca `cross_post_source_id` dolu içerikleri değiştirir;
/// diğerlerine dokunmaz.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn resolve_cross_posts(
    pool: &PgPool,
    viewer_communities: &[i64],
    contents: &mut [Content],
) -> Result<()> {
    // Farklı kaynak id'leri: `BTreeSet` hem tekilleştiriyor hem
    // deterministik sıra veriyor (parametre sırası test edilebilir olsun
    // diye; semantik önemi yok).
    let source_ids: Vec<i64> = contents
        .iter()
        .filter_map(|content| content.cross_post_source_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if source_ids.is_empty() {
        return Ok(());
    }

    struct SourceRow {
        id: i64,
        title: Option<String>,
        deleted_at: Option<DateTime<Utc>>,
        author_id: i64,
        author_username: String,
        author_actor_type: ActorType,
        author_display_name: Option<String>,
        author_bio: Option<String>,
        author_created_at: DateTime<Utc>,
        author_deleted_at: Option<DateTime<Utc>>,
        community_id: Option<i64>,
        community_name: Option<String>,
        community_visibility: Option<CommunityVisibility>,
    }

    let rows = sqlx::query_as!(
        SourceRow,
        r#"
        SELECT
            kaynak.id,
            kaynak.title,
            kaynak.deleted_at,
            actors.id AS author_id,
            actors.username AS author_username,
            actors.actor_type AS "author_actor_type: ActorType",
            actors.display_name AS author_display_name,
            actors.bio AS author_bio,
            actors.created_at AS author_created_at,
            actors.deleted_at AS author_deleted_at,
            topluluk.id AS "community_id?",
            topluluk.name AS "community_name?",
            topluluk.visibility AS "community_visibility?: CommunityVisibility"
        FROM contents AS kaynak
        JOIN actors ON actors.id = kaynak.actor_id
        LEFT JOIN communities AS topluluk ON topluluk.id = kaynak.community_id
        WHERE kaynak.id = ANY($1::bigint[])
        "#,
        &source_ids,
    )
    .fetch_all(pool)
    .await?;

    let sources: HashMap<i64, SourceRow> = rows.into_iter().map(|row| (row.id, row)).collect();

    for content in contents.iter_mut() {
        let Some(source_id) = content.cross_post_source_id else {
            continue;
        };

        let Some(source) = sources.get(&source_id) else {
            // Kaynak hiç yok — mezar taşı.
            content.cross_post = None;
            continue;
        };

        // Kaynak görünürlüğü: silinmemiş VE (bağımsız ya da public ya da
        // okuyucunun gördüğü bir toplulukta). Public yüzeyler boş küme
        // geçtiği için özel kaynaklar burada elenir.
        let reachable = source.deleted_at.is_none()
            && match source.community_visibility {
                Some(CommunityVisibility::Private) => source
                    .community_id
                    .is_some_and(|id| viewer_communities.contains(&id)),
                Some(CommunityVisibility::Public) | None => true,
            };

        content.cross_post = reachable.then(|| CrossPostPreview {
            source_id: source.id,
            title: source.title.clone(),
            author: ActorRecord {
                id: source.author_id,
                username: source.author_username.clone(),
                actor_type: source.author_actor_type,
                display_name: source.author_display_name.clone(),
                bio: source.author_bio.clone(),
                created_at: source.author_created_at,
            },
            author_deleted: source.author_deleted_at.is_some(),
            community: source.community_id.map(|id| CommunityRef {
                id,
                name: source.community_name.clone().unwrap_or_default(),
            }),
        });
    }

    Ok(())
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
    viewer_communities: &[i64],
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
          AND content_visible_to(contents.community_id, $2::bigint[])
        FOR UPDATE
        "#,
        id,
        viewer_communities,
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

    get_post(pool, id, viewer_communities).await
}

// --- Post silme --------------------------------------------------------

/// `DELETE /posts/{id}`: sahibi **veya** moderatör/admin. Soft-delete
/// (`deleted_at`).
///
/// Sıra bilinçli: önce varlık (`404`), sonra zaten silinmiş mi (`410`),
/// sonra yetki (`403`) — bir moderatörün olmayan bir post'u silmeye
/// çalışması `403` değil `404` almalı (yetkisi olsa da olmasa da post
/// yok), ama var olan başkasının post'unu silmeye çalışan sıradan bir
/// actor `403` almalı. `permissions` içinde global `content.delete` yoksa
/// yalnızca sahiplik kontrol edilir.
///
/// # Errors
/// Post yoksa [`Error::NotFound`]; zaten silinmişse [`Error::Gone`];
/// çağıran ne sahibi ne `content.delete` sahibi ise [`Error::Forbidden`];
/// veritabanı hatası [`Error::Database`].
pub async fn delete_post(
    pool: &PgPool,
    id: i64,
    actor_id: i64,
    permissions: &[Grant],
) -> Result<()> {
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
    let can_delete = crate::authz::has_global(permissions, Permission::ContentDelete);

    if !is_owner && !can_delete {
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
    viewer_communities: &[i64],
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
              AND content_visible_to(contents.community_id, $5::bigint[])
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
            contents.score,
            contents.upvotes,
            contents.downvotes,
            contents.comment_count,
            contents.hot_score,
            contents.created_at,
            contents.edited_at,
            contents.deleted_at,
            contents.cross_post_source_id,
            actors.id AS author_id,
            actors.username AS author_username,
            actors.actor_type AS "author_actor_type: ActorType",
            actors.display_name AS author_display_name,
            actors.bio AS author_bio,
            actors.created_at AS author_created_at,
            actors.deleted_at AS author_deleted_at,
            communities.id AS "community_id?",
            communities.name AS "community_name?",
            COALESCE(
                array_agg(tags.name::text) FILTER (WHERE tags.id IS NOT NULL),
                '{}'
            ) AS "tags!: Vec<String>"
        FROM page
        JOIN contents ON contents.id = page.id
        JOIN actors ON actors.id = contents.actor_id
        LEFT JOIN communities ON communities.id = contents.community_id
        LEFT JOIN content_tags ON content_tags.content_id = page.id
        LEFT JOIN tags ON tags.id = content_tags.tag_id
        GROUP BY contents.id, actors.id, communities.id
        ORDER BY contents.created_at DESC, contents.id DESC
        "#,
        actor_id,
        cursor_created_at,
        cursor_id,
        limit + 1,
        viewer_communities,
    )
    .fetch_all(pool)
    .await?;

    let mut page = paginate(
        rows,
        limit,
        |row: &ContentRow| row.id,
        |row: &ContentRow| SortKey::New {
            created_at: row.created_at,
        },
        Content::from,
    );
    resolve_cross_posts(pool, viewer_communities, &mut page.items).await?;
    Ok(page)
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
    viewer_communities: &[i64],
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
                      AND content_visible_to(contents.community_id, $5::bigint[])
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
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
                    contents.created_at,
                    contents.edited_at,
                    contents.deleted_at,
                    contents.cross_post_source_id,
                    actors.id AS author_id,
                    actors.username AS author_username,
                    actors.actor_type AS "author_actor_type: ActorType",
                    actors.display_name AS author_display_name,
                    actors.bio AS author_bio,
                    actors.created_at AS author_created_at,
                    actors.deleted_at AS author_deleted_at,
                    communities.id AS "community_id?",
                    communities.name AS "community_name?",
                    COALESCE(
                        array_agg(tags.name::text) FILTER (WHERE tags.id IS NOT NULL),
                        '{}'
                    ) AS "tags!: Vec<String>"
                FROM page
                JOIN contents ON contents.id = page.id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN communities ON communities.id = contents.community_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id, communities.id
                ORDER BY contents.created_at DESC, contents.id DESC
                "#,
                tag_id,
                cursor_created_at,
                cursor_id,
                limit + 1,
                viewer_communities,
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
                      AND content_visible_to(contents.community_id, $5::bigint[])
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
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
                    contents.created_at,
                    contents.edited_at,
                    contents.deleted_at,
                    contents.cross_post_source_id,
                    actors.id AS author_id,
                    actors.username AS author_username,
                    actors.actor_type AS "author_actor_type: ActorType",
                    actors.display_name AS author_display_name,
                    actors.bio AS author_bio,
                    actors.created_at AS author_created_at,
                    actors.deleted_at AS author_deleted_at,
                    communities.id AS "community_id?",
                    communities.name AS "community_name?",
                    COALESCE(
                        array_agg(tags.name::text) FILTER (WHERE tags.id IS NOT NULL),
                        '{}'
                    ) AS "tags!: Vec<String>"
                FROM page
                JOIN contents ON contents.id = page.id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN communities ON communities.id = contents.community_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id, communities.id
                ORDER BY contents.score DESC, contents.id DESC
                "#,
                tag_id,
                cursor_score,
                cursor_id,
                limit + 1,
                viewer_communities,
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
                      AND content_visible_to(contents.community_id, $5::bigint[])
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
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
                    contents.created_at,
                    contents.edited_at,
                    contents.deleted_at,
                    contents.cross_post_source_id,
                    actors.id AS author_id,
                    actors.username AS author_username,
                    actors.actor_type AS "author_actor_type: ActorType",
                    actors.display_name AS author_display_name,
                    actors.bio AS author_bio,
                    actors.created_at AS author_created_at,
                    actors.deleted_at AS author_deleted_at,
                    communities.id AS "community_id?",
                    communities.name AS "community_name?",
                    COALESCE(
                        array_agg(tags.name::text) FILTER (WHERE tags.id IS NOT NULL),
                        '{}'
                    ) AS "tags!: Vec<String>"
                FROM page
                JOIN contents ON contents.id = page.id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN communities ON communities.id = contents.community_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id, communities.id
                ORDER BY contents.hot_score DESC, contents.id DESC
                "#,
                tag_id,
                cursor_hot,
                cursor_id,
                limit + 1,
                viewer_communities,
            )
            .fetch_all(pool)
            .await?
        }
    };

    let mut page = paginate(
        rows,
        limit,
        |row: &ContentRow| row.id,
        |row: &ContentRow| post_sort_key(sort, row),
        Content::from,
    );
    resolve_cross_posts(pool, viewer_communities, &mut page.items).await?;
    Ok(page)
}
