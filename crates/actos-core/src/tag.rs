//! Etiketler: popülerlik listesi, otomatik tamamlama araması ve
//! kullanılmayan etiketleri toplayan periyodik iş.
//!
//! Etiketlerin **oluşturulması** burada değil,
//! [`crate::content::create_post`] içinde (`attach_tags`): bir etiket
//! kendi başına yaratılmıyor, yalnızca bir post'a eklendiğinde var oluyor.
//! Bu modül var olanları okuyup temizliyor.
//!
//! Etiket adı doğrulaması da burada değil, [`crate::text::validate_tag_name`]
//! içinde — kural (`[a-z0-9][a-z0-9-]{0,31}`, küçük harf, kırpılmış)
//! `migrations/0007_tags.up.sql`'deki `ck_tags_name_format` ile birebir
//! aynı; iki yerde iki farklı kural olmaması için tek kaynak orası.
//!
//! ## Popülerlik neden sorgu anında sayılıyor
//!
//! [`list_popular`] etiket başına post sayısını her istekte `GROUP BY` ile
//! hesaplıyor; `tags` tablosunda tutulan bir sayaç sütunu yok. Gerekçe:
//! böyle bir sayaç post silme/geri alma, etiket ekleme/çıkarma ve içerik
//! moderasyonu yollarının hepsinde senkron tutulmak zorunda kalırdı ve
//! kaçırılan tek bir yol sayacı sessizce yanlışlar. v1 ölçeğinde `GROUP BY`
//! yeterli; ölçüm gerekirse (bkz. PLAN.md Faz 17) materialized view ya da
//! sayaç sütunu sonradan eklenebilir — dışa dönük sözleşme değişmez.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    actor::{Page, paginate},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    text,
};

/// `GET /tags/search` için azami sonuç sayısı.
///
/// Otomatik tamamlama listesi kısa olmalı: istemci bunu bir açılır menüde
/// gösteriyor, yüzlerce satır ne kullanıcıya ne ajana yarar.
pub const SEARCH_LIMIT: i64 = 20;

/// Trigram benzerlik eşiği (`similarity()` için).
///
/// PostgreSQL'in `%` operatörü `pg_trgm.similarity_threshold` (varsayılan
/// 0.3) kullanıyor ve bu kısa sorgularda işe yaramıyor: `similarity('nvidia',
/// 'nv')` yalnızca 0.25 — yani planın kendi örneği olan `?q=nv` saf trigram
/// eşleşmesiyle **hiçbir sonuç döndürmezdi**. Bu yüzden [`search`] önek
/// eşleşmesini trigram'a ekliyor ve eşiği burada kendi belirlediğimiz daha
/// gevşek bir değerde tutuyoruz; trigram'ın işi kısa önekleri yakalamak
/// değil, yazım hatalarını (`nvdia` → `nvidia`) toparlamak.
///
/// `f32` çünkü PostgreSQL'in `similarity()` fonksiyonu `real` döndürüyor.
const SIMILARITY_THRESHOLD: f32 = 0.2;

/// Kullanılmayan etiket toplayıcısının PostgreSQL advisory lock anahtarı.
///
/// Sabit ve bu işe özel: birden fazla API instance'ı aynı anda çalışsa bile
/// temizlik aynı anda yalnızca birinde koşsun diye. Değer keyfi ama
/// **değiştirilmemeli** — çalışan eski bir instance ile yeni bir instance
/// farklı anahtar kullanırsa kilit amacını yitirir.
const CLEANUP_ADVISORY_LOCK_KEY: i64 = 0x0AC7_0510;

/// Bir etiket + kaç canlı post'ta kullanıldığı.
#[derive(Debug, Clone)]
pub struct TagSummary {
    pub id: i64,
    pub name: String,
    pub post_count: i32,
    pub created_at: DateTime<Utc>,
}

/// Otomatik tamamlama sonucu.
///
/// [`TagSummary`]'den ayrı bir tip: arama sorgusu post sayısını
/// hesaplamıyor (autocomplete'in her tuş vuruşunda `GROUP BY` yapması
/// karşılığını vermez), dolayısıyla `TagSummary` döndürüp `post_count`'u
/// `0` ile doldurmak yanlış bir değeri doğruymuş gibi taşımak olurdu.
#[derive(Debug, Clone)]
pub struct TagMatch {
    pub id: i64,
    pub name: String,
}

struct TagRow {
    id: i64,
    name: String,
    post_count: i32,
    created_at: DateTime<Utc>,
}

struct TagMatchRow {
    id: i64,
    name: String,
}

impl From<TagMatchRow> for TagMatch {
    fn from(row: TagMatchRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
        }
    }
}

impl From<TagRow> for TagSummary {
    fn from(row: TagRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            post_count: row.post_count,
            created_at: row.created_at,
        }
    }
}

/// Bir `Top` cursor'ını `(post_count, id)` bindable çiftine ayırır.
///
/// Popülerlik sıralaması [`SortKey::Top`] üzerinden ifade ediliyor:
/// `Top` "en yüksek sayısal değer önce" demek, burada o değer skor değil
/// post sayısı. `cursor.rs`'in imza mekanizması taşınan sayının ne anlama
/// geldiğini bilmiyor, yalnızca sıralamanın türünü sabitliyor — bu yüzden
/// yeni bir `SortKey` varyantı eklemeye gerek yok.
///
/// # Errors
/// Cursor başka bir sıralamaya aitse [`Error::InvalidCursor`].
fn split_count_cursor(cursor: Option<Cursor>) -> Result<(Option<i32>, Option<i64>)> {
    match cursor {
        None => Ok((None, None)),
        Some(Cursor {
            sort: SortKey::Top { score },
            id,
        }) => Ok((Some(score), Some(id))),
        Some(_) => Err(Error::InvalidCursor),
    }
}

/// `GET /tags`: en çok kullanılan etiketler önce, cursor'lu.
///
/// **Yalnızca canlı post'lar sayılıyor** (`deleted_at IS NULL`,
/// `content_type = 'post'`) ve `HAVING` ile sayısı sıfıra düşen etiketler
/// listeden çıkarılıyor: bütün post'ları silinmiş bir etiket "popüler
/// etiketler" listesinde bir gürültü satırından ibaret olurdu. Böyle
/// etiketler `content_tags` satırlarını hâlâ taşıdığı için
/// [`cleanup_unused`] tarafından da silinmez — listeden düşmeleri bu
/// `HAVING`'in işi.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn list_popular(
    pool: &PgPool,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<TagSummary>> {
    let (cursor_count, cursor_id) = split_count_cursor(cursor)?;

    let rows = sqlx::query_as!(
        TagRow,
        r#"
        SELECT
            tags.id,
            tags.name::text AS "name!",
            COUNT(contents.id) FILTER (
                WHERE contents.deleted_at IS NULL
                  AND contents.content_type = 'post'::content_type
            )::int AS "post_count!",
            tags.created_at
        FROM tags
        JOIN content_tags ON content_tags.tag_id = tags.id
        JOIN contents ON contents.id = content_tags.content_id
        GROUP BY tags.id
        HAVING COUNT(contents.id) FILTER (
                   WHERE contents.deleted_at IS NULL
                     AND contents.content_type = 'post'::content_type
               ) > 0
           AND (
               $1::int IS NULL
               OR (
                   COUNT(contents.id) FILTER (
                       WHERE contents.deleted_at IS NULL
                         AND contents.content_type = 'post'::content_type
                   )::int,
                   tags.id
               ) < ($1::int, $2::bigint)
           )
        ORDER BY "post_count!" DESC, tags.id DESC
        LIMIT $3
        "#,
        cursor_count,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row: &TagRow| row.id,
        |row: &TagRow| SortKey::Top {
            score: row.post_count,
        },
        TagSummary::from,
    ))
}

/// `GET /tags/search?q=`: otomatik tamamlama.
///
/// **İki eşleşme yolu birlikte** (bkz. [`SIMILARITY_THRESHOLD`]):
/// 1. **Önek** (`name LIKE 'q%'`) — asıl otomatik tamamlama yolu. `?q=nv`
///    → `nvidia`. Trigram tek başına bunu yakalayamıyor.
/// 2. **Trigram benzerliği** — yazım hatalarını toparlıyor (`nvdia` →
///    `nvidia`).
///
/// Sıralama önce önek eşleşmelerini veriyor: `nv` yazan biri `nvidia`'yı
/// `envanter`den önce görmeli, ikisi de eşleşse bile.
///
/// Sorgu **önce küçük harfe çevriliyor**, sonra saklanan etiketlerle aynı
/// kurallardan geçiriliyor ([`text::validate_tag_name`]). Küçültme şart:
/// `validate_tag_name` büyük harfi düzeltmez, *reddeder* (etiketler her
/// zaman küçük harf saklandığı için depolama tarafında doğru davranış bu) —
/// oysa arama tarafında `?q=NV` yazan birine boş liste döndürmek yanlış
/// olurdu. Kurallara hiç uymayan bir sorgu (ör. `?q=@@@`) hiçbir etiketle
/// eşleşemeyeceği için hata yerine **boş liste** dönüyor: otomatik
/// tamamlamada kullanıcı henüz yazarken hata göstermek yanlış olurdu.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn search(pool: &PgPool, query: &str) -> Result<Vec<TagMatch>> {
    let Ok(normalized) = text::validate_tag_name(&query.to_lowercase()) else {
        return Ok(Vec::new());
    };

    let rows = sqlx::query_as!(
        TagMatchRow,
        r#"
        SELECT
            tags.id,
            tags.name::text AS "name!"
        FROM tags
        WHERE tags.name::text LIKE $1 || '%'
           OR similarity(tags.name::text, $1) >= $2
        ORDER BY
            (tags.name::text LIKE $1 || '%') DESC,
            similarity(tags.name::text, $1) DESC,
            tags.name::text
        LIMIT $3
        "#,
        normalized,
        SIMILARITY_THRESHOLD,
        SEARCH_LIMIT,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(TagMatch::from).collect())
}

/// Hiçbir içeriğe bağlı olmayan etiketleri siler; silinen satır sayısını
/// döner.
///
/// **Advisory lock ile korunuyor:** birden fazla API instance'ı çalışırken
/// hepsinin aynı anda aynı `DELETE`'i denemesi gereksiz kilit çekişmesi
/// üretirdi. `pg_try_advisory_lock` **beklemez** — kilidi başkası tutuyorsa
/// bu tur atlanır ve `Ok(0)` döner; periyodik bir iş için doğru davranış
/// budur, sıraya girip birikmek değil.
///
/// Kilit PostgreSQL'de **veritabanı kapsamlıdır** (`pg_locks.database`),
/// yani aynı sunucudaki başka bir veritabanına bağlı bir süreç bu kilitten
/// etkilenmez — tam olarak istediğimiz kapsam: "bu Actos veritabanında aynı
/// anda tek temizlik".
///
/// Kilit oturum kapsamlı olduğu için tüm işlem **tek bir bağlantı**
/// üzerinde yürütülüyor; havuzdan her adımda farklı bir bağlantı alınsaydı
/// kilit alan oturumla `DELETE` yapan oturum farklı olurdu.
///
/// `content_tags` satırı olan ama tüm post'ları silinmiş bir etiket burada
/// **silinmez** — soft-delete edilen bir post geri alınabilir ve etiketleri
/// yerinde durmalı. Böyle etiketler yalnızca [`list_popular`]'ın
/// `HAVING`'iyle listeden düşer.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn cleanup_unused(pool: &PgPool) -> Result<u64> {
    let mut conn = pool.acquire().await?;

    let locked = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_lock($1) AS "locked!""#,
        CLEANUP_ADVISORY_LOCK_KEY,
    )
    .fetch_one(&mut *conn)
    .await?;

    if !locked {
        tracing::debug!("etiket temizliği başka bir instance'da çalışıyor, bu tur atlandı");
        return Ok(0);
    }

    let result = sqlx::query!(
        r#"
        DELETE FROM tags
        WHERE NOT EXISTS (
            SELECT 1 FROM content_tags WHERE content_tags.tag_id = tags.id
        )
        "#,
    )
    .execute(&mut *conn)
    .await;

    // Kilit her durumda bırakılmalı — `DELETE` hata verse bile. `?` ile
    // erken dönseydik kilit, bağlantı havuza geri dönüp yeniden
    // kullanılana kadar tutulmuş olurdu.
    let unlock = sqlx::query_scalar!(
        r#"SELECT pg_advisory_unlock($1) AS "unlocked!""#,
        CLEANUP_ADVISORY_LOCK_KEY,
    )
    .fetch_one(&mut *conn)
    .await;

    if let Err(err) = unlock {
        tracing::warn!(error = %err, "etiket temizliği advisory lock'ı bırakılamadı");
    }

    let deleted = result?.rows_affected();

    if deleted > 0 {
        tracing::info!(deleted, "kullanılmayan etiketler temizlendi");
    }

    Ok(deleted)
}
