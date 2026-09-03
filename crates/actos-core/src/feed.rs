//! Ana akış (feed) ve sıcaklık (hot score) hesabı.
//!
//! `GET /feed` amac.txt'teki `GET posts/mainpage` senaryosu; `GET
//! /feed/following` ise yalnızca takip edilenlerin postları.
//!
//! ## Hot score formülü
//!
//! ```text
//! sign(score) * log10(max(|score|, 1)) + epoch_seconds / 45000
//! ```
//!
//! Reddit'in klasik sıralaması. İki terim iki farklı işi yapıyor:
//!
//! - `sign(score) * log10(...)`: oyların etkisi **logaritmik**. 10 oy ile
//!   100 oy arasındaki fark, 1 oy ile 10 oy arasındakiyle aynı — böylece
//!   çok oy almış bir post listeyi sonsuza kadar kilitlemiyor.
//! - `epoch_seconds / 45000`: zaman terimi **koşulsuz** ekleniyor, yani
//!   45 000 saniyede (12.5 saat) bir tam puanlık avantaj. Yeni içerik
//!   kendiliğinden yukarıda başlıyor.
//!
//! **PLAN.md'deki ilk hâli hatalıydı** (`log10(...) + sign(score) *
//! epoch/45000`): `sign` yanlış terimi çarpıyordu ve `sign(0) = 0` zaman
//! terimini tamamen siliyordu — oy almamış her post `hot_score = 0` alıp
//! dibe düşerdi. Postların çoğunun 0 oyda olduğu yeni bir platformda hot
//! feed çalışmaz hâle gelirdi. Ölçüm ve düzeltme PLAN.md Faz 12'de kayıtlı.
//!
//! ## Formül neden iki yerde yazılı
//!
//! Aynı ifade hem burada ([`recompute_hot_scores`]) hem de
//! `crate::interaction::set_vote` içinde geçiyor. Tek bir Rust sabitinde
//! toplanamıyor: `sqlx::query!` derleme zamanı doğrulaması için sorgunun
//! bir string **literal** olmasını şart koşuyor, sabite yapılan referansı
//! kabul etmiyor. **Biri değişirse diğeri de değişmeli.**
//!
//! ## Ne zaman hesaplanıyor
//!
//! - **Anında:** her oyla birlikte, aynı transaction'da
//!   (`crate::interaction::set_vote`). Skor değiştiği anda sıralama doğru.
//! - **Periyodik:** [`recompute_hot_scores`] son 7 günün postlarını tazeler.
//!   Skor değişmese bile zaman terimi kaydığı için gerekli — ama yalnızca
//!   *göreli* sıra önemli olduğundan ve zaman terimi bütün satırlarda aynı
//!   hızda büyüdüğünden bu tazeleme aslında sıralamayı değiştirmiyor;
//!   değeri mutlak olarak doğru tutmak için var (ör. `EXPLAIN` ile plan
//!   incelerken ya da ileride farklı bir pencereyle kıyaslarken).

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;

use crate::{
    actor::{Page, paginate},
    auth::{ActorRecord, ActorType},
    content::{BodyFormat, Content, ContentType, PostSort},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
};

/// [`recompute_hot_scores`]'un tazelediği pencere.
///
/// 7 gün: bundan eski postlar hot feed'in üst sıralarına zaten
/// çıkamıyor (zaman terimi 14 puan geride kalıyor), tazelemenin bir
/// karşılığı yok.
const RECOMPUTE_WINDOW_DAYS: i64 = 7;

/// [`recompute_hot_scores`]'un PostgreSQL advisory lock anahtarı.
///
/// `crate::tag`'inkinden **farklı** olmak zorunda: aynı anahtar iki farklı
/// işi birbirini beklettirirdi. Advisory lock'lar veritabanı kapsamlı
/// (bkz. `crate::tag::cleanup_unused`).
const RECOMPUTE_ADVISORY_LOCK_KEY: i64 = 0x0AC7_0512;

/// Feed'in zaman penceresi (`?window=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedWindow {
    Day,
    Week,
    Month,
    /// Pencere yok; tüm zamanlar.
    All,
}

impl FeedWindow {
    /// `?window=` query parametresini ayrıştırır. Verilmemişse [`Self::All`].
    ///
    /// # Errors
    /// Tanınmayan bir değer [`Error::Validation`] üretir — sessizce
    /// varsayılana düşmek, yazım hatası yapan bir istemciye yanlış
    /// pencereden veriyi doğruymuş gibi verirdi.
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        match raw {
            None | Some("all") => Ok(Self::All),
            Some("day") => Ok(Self::Day),
            Some("week") => Ok(Self::Week),
            Some("month") => Ok(Self::Month),
            Some(other) => Err(Error::Validation(format!(
                "invalid window value: \"{other}\" (expected: day, week, month, all)"
            ))),
        }
    }

    /// Pencerenin alt sınırı; [`Self::All`] için `None`.
    ///
    /// Kesim noktası SQL'de `now() - interval` yerine burada hesaplanıp
    /// bağlanıyor: sorgu metnini sabit tutuyor (tek bir `query!` bütün
    /// pencereleri karşılıyor) ve testlerin zamanı kontrol etmesini
    /// kolaylaştırıyor.
    #[must_use]
    pub fn cutoff(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let gun = match self {
            Self::Day => 1,
            Self::Week => 7,
            Self::Month => 30,
            Self::All => return None,
        };
        Some(now - Duration::days(gun))
    }
}

/// `GET /feed` ve `GET /feed/following`.
///
/// **Tek fonksiyon, iki uç:** `follower` verilirse yalnızca o actor'ün
/// takip ettiklerinin postları döner, verilmezse bütün platform. Ayrı iki
/// fonksiyon yazmak, üç sıralamanın her biri için ikişer kopya (toplam altı
/// devasa `SELECT`) demekti; `$follower IS NULL OR ...` koşulu bunu üçe
/// indiriyor.
///
/// Yalnızca **post**'lar (yorumlar feed'e girmez) ve yalnızca canlı
/// içerik.
///
/// ## Performans: iki aşamalı sorgu (Faz 17)
///
/// Üç sıralamanın SQL'i de `WITH page AS (...)` CTE'siyle **iki aşamalı**:
/// önce `page` yalnızca eşleşen satırların `id`'sini `ORDER BY ... LIMIT`
/// ile sayfa boyutuna keser, sonra dıştaki sorgu bu az sayıdaki id için
/// `actors`/`content_tags`/`tags` `JOIN`'lerini ve `array_agg`'i yapar.
///
/// Tek aşamalı hâlde (etiket `JOIN`/`GROUP BY`'ı `ORDER BY`/`LIMIT`'ten
/// önce) planlayıcı `GROUP BY contents.id, actors.id` yüzünden `ORDER BY
/// ... LIMIT`'i aggregate'in altına itemiyordu: eşleşen bütün satırlar
/// (200 000 satırlık ölçüm veritabanında pratikte tüm tablo) önce
/// gruplanıp diske taşınıyor, sonra son `limit+1` tanesi seçiliyordu.
/// Ölçülmüş öncesi/sonrası süreler ve `EXPLAIN` çıktıları
/// `docs/query-plans.md`'de. Desen `crate::search::search_content`'ten
/// aynen kopyalandı (bkz. o modülün "Performans" bölümü) — **yeni bir
/// index gerekmiyor**, `idx_contents_new`/`_top`/`_hot` zaten doğru
/// index'ler, sorun onların kullanılamaması değil aggregate'in erken
/// çalışmasıydı.
///
/// `follower` filtresi (`$2::bigint IS NULL OR ...`) artık `page` CTE'sinin
/// **içinde** — genel feed'in `NULL` sabitiyle planlayıcının alt sorguyu
/// tamamen elediği davranış korundu (bkz. `docs/query-plans.md`).
///
/// ## `actor_type` filtresi (Faz 18.A, `NOTES.md` §8.1)
///
/// Aynı desen: `$6::actor_type IS NULL OR contents.actor_id IN (SELECT id
/// FROM actors WHERE actor_type = $6)`, `follower`'ınkiyle **aynı** `page`
/// CTE'sinin içinde, `ORDER BY ... LIMIT`'ten önce — iki aşamalı yapıyı
/// bozmuyor. `EXPLAIN (ANALYZE, BUFFERS)` ile `actos_explain`'de ölçüldü
/// (`docs/query-plans.md`): planlayıcı bunu da `follower` filtresiyle
/// birebir aynı şekle sokuyor — `idx_contents_hot/new/top` üzerinde tek bir
/// `Index Scan`, `actors` alt sorgusu (2 000 satır, ucuz bir `Seq Scan`)
/// **bir kez** hashlenip `Filter` olarak uygulanıyor, `LIMIT` hemen
/// ardından geliyor. **Yeni bir index gerekmedi** — `actors` tablosu o
/// kadar küçük ki (2 000 satır) hash'lenmesi ölçülemeyecek kadar ucuz;
/// `follower` filtresinin 1 999 satırlık `follows` alt sorgusuyla aynı
/// gerekçe.
///
/// **Doğrulanmıyor:** `actor_type` kayıt sırasında actor'ün kendi beyanı
/// (`POST /auth/register`), sunucu bunu bağımsız bir şekilde teyit etmiyor
/// (ör. bir insan `ai_agent` diye kaydolabilir). Bu filtre bu yüzden bir
/// *garanti* değil bir *kolaylık* — bkz. `docs/API.md` §3.8.
///
/// ## Güven kademesi ve `hot` filtresi (Faz 18.B, `NOTES.md` §9.3/§9.6)
///
/// **`hot` sıralaması yazarın `trust_level >= 1` olmasını şart koşuyor**
/// (`page` CTE'sinde `follower`/`actor_type` ile birebir aynı desende,
/// yukarıdaki SQL'e bkz.) — seviye 0 bir actor'ün post'u `hot`'ta hiç
/// görünmüyor. **`new` etkilenmiyor**: seviye 0 bir hesabın post'u orada
/// normal şekilde listeleniyor, yalnızca `hot`'un varsayılan keşif
/// yüzeyinden gizli.
///
/// **Neden ek bir kapı gerekiyor — `crate::interaction::set_vote`'daki oy
/// ağırlığı yetmiyor mu?** Oy ağırlığı zaten "100 sahte hesapla kendine oy
/// at" saldırısını kapatıyor (seviye 0 oyu skora `0` katkı yapıyor). Ama
/// `hot_score`'un formülü (bkz. yukarıdaki modül dokümantasyonu) skorun
/// yanına **koşulsuz bir zaman terimi** ekliyor — taze açılmış bir hesap
/// tek bir spam post attığı anda, hiç oy almadan bile, salt zaman
/// teriminden `hot`'un tepesine yakın bir yere yerleşebilir. Oy ağırlığı
/// bu saldırı yolunu kapatmıyor çünkü devreye girmesi için önce gerçek
/// kullanıcıların oy vermesi (ya da vermemesi) gerekiyor — `hot` ise
/// tam olarak "gerçek kullanıcıların henüz göremediği" o ilk pencerede
/// zarar veriyor. Yazar seviyesine bakan bir kapı bu pencereyi kapatan
/// tek şey: sybil halkasının içeriği, gerçek oylardan bağımsız olarak,
/// platformun ana keşif yüzeyine hiç çıkamıyor.
///
/// **`top` için AYNI kapı BİLEREK eklenmedi.** `top` salt `contents.score`a
/// göre sıralıyor ve `score` artık `sum(value * weight)` — seviye 0 bir
/// yazarın kendi kuklalarından aldığı oylar zaten `0` ağırlıklı, yani
/// `top`taki sybil saldırı yüzeyi oy ağırlığı mekanizmasıyla ZATEN
/// kapalı (yukarıdaki `hot` gerekçesindeki "koşulsuz zaman terimi" `top`ta
/// yok — `top`un tek girdisi `score`, ve o girdi başından beri ağırlıklı).
/// Seviye 0 bir yazarın `top`ta üst sıralarda görünmesi, ancak GERÇEK
/// (seviye ≥1) actor'lerin ağırlıklı oylarıyla oluyorsa mümkün — bu meşru
/// bir sinyal, bastırmak cezalandırıcı olurdu. Ayrıca `crate::actor::
/// recompute_trust_levels`'ın seviye 1 için BİLEREK karma şartı
/// taşımamasının gerekçesiyle (soğuk başlangıç, bkz. o fonksiyonun
/// dokümantasyonu) aynı mantık burada da geçerli: `hot` zaten kapalıyken
/// `top`u da kapatmak, yeni bir hesabın gerçekten iyi bir içerik
/// üretmesi durumunda bile hiçbir sıralı yüzeyde görünememesi demek
/// olurdu — `new` tek başına yeterli bir keşif yolu değil (kronolojik,
/// kaliteden bağımsız). `top` bu yüzden yalnızca `window`/cursor
/// filtrelerini taşıyor, `hot`ın yazar-seviyesi kapısını taşımıyor.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn list_feed(
    pool: &PgPool,
    follower: Option<i64>,
    sort: PostSort,
    window: FeedWindow,
    actor_type: Option<ActorType>,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Content>> {
    let cutoff = window.cutoff(Utc::now());

    let (cursor_created_at, cursor_score, cursor_hot, cursor_id) = match (sort, cursor) {
        (_, None) => (None, None, None, None),
        (
            PostSort::New,
            Some(Cursor {
                sort: SortKey::New { created_at },
                id,
            }),
        ) => (Some(created_at), None, None, Some(id)),
        (
            PostSort::Top,
            Some(Cursor {
                sort: SortKey::Top { score },
                id,
            }),
        ) => (None, Some(score), None, Some(id)),
        (
            PostSort::Hot,
            Some(Cursor {
                sort: SortKey::Hot { hot_score },
                id,
            }),
        ) => (None, None, Some(hot_score), Some(id)),
        _ => return Err(Error::InvalidCursor),
    };

    // İki aşamalı sorgu — bkz. modül dokümantasyonu "Performans" bölümü ve
    // `docs/query-plans.md`. `page` CTE'si yalnızca sayfanın `id`'lerini
    // `ORDER BY ... LIMIT` ile keser (etiket `JOIN`/`array_agg`'inden VE
    // `GROUP BY`'dan ÖNCE); dıştaki sorgu bu az sayıdaki id için etiketleri
    // toplar. Desen `crate::search::search_content`'ten aynen kopyalandı.
    // `contents.id` (primary key) `GROUP BY`'da olduğu için Postgres'in
    // fonksiyonel bağımlılık kuralı `contents`'in diğer sütunlarının (ör.
    // `created_at`) `ORDER BY`'da agregat dışı kullanılmasına izin veriyor
    // — `page`'den ayrı bir sıralama anahtarı taşımaya gerek yok, tıpkı
    // eski (tek aşamalı) sorgunun zaten aynı GROUP BY ile SELECT'te
    // `contents.created_at`'i agregat dışı kullanmasında olduğu gibi.
    let rows = match sort {
        PostSort::New => {
            sqlx::query_as!(
                FeedRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    WHERE contents.content_type = 'post'::content_type
                      AND contents.deleted_at IS NULL
                      AND ($1::timestamptz IS NULL OR contents.created_at >= $1::timestamptz)
                      AND (
                          $2::bigint IS NULL
                          OR contents.actor_id IN (
                              SELECT followed_actor_id FROM follows
                              WHERE follower_actor_id = $2::bigint
                          )
                      )
                      AND (
                          $6::actor_type IS NULL
                          OR contents.actor_id IN (
                              SELECT id FROM actors WHERE actor_type = $6::actor_type
                          )
                      )
                      AND (
                          $3::timestamptz IS NULL
                          OR (contents.created_at, contents.id) < ($3::timestamptz, $4::bigint)
                      )
                    ORDER BY contents.created_at DESC, contents.id DESC
                    LIMIT $5
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
                    actors.trust_level AS author_trust_level,
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
                cutoff,
                follower,
                cursor_created_at,
                cursor_id,
                limit + 1,
                actor_type as Option<ActorType>,
            )
            .fetch_all(pool)
            .await?
        }
        PostSort::Top => {
            sqlx::query_as!(
                FeedRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    WHERE contents.content_type = 'post'::content_type
                      AND contents.deleted_at IS NULL
                      AND ($1::timestamptz IS NULL OR contents.created_at >= $1::timestamptz)
                      AND (
                          $2::bigint IS NULL
                          OR contents.actor_id IN (
                              SELECT followed_actor_id FROM follows
                              WHERE follower_actor_id = $2::bigint
                          )
                      )
                      AND (
                          $6::actor_type IS NULL
                          OR contents.actor_id IN (
                              SELECT id FROM actors WHERE actor_type = $6::actor_type
                          )
                      )
                      AND (
                          $3::int IS NULL
                          OR (contents.score, contents.id) < ($3::int, $4::bigint)
                      )
                    ORDER BY contents.score DESC, contents.id DESC
                    LIMIT $5
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
                    actors.trust_level AS author_trust_level,
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
                cutoff,
                follower,
                cursor_score,
                cursor_id,
                limit + 1,
                actor_type as Option<ActorType>,
            )
            .fetch_all(pool)
            .await?
        }
        PostSort::Hot => {
            sqlx::query_as!(
                FeedRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    WHERE contents.content_type = 'post'::content_type
                      AND contents.deleted_at IS NULL
                      AND ($1::timestamptz IS NULL OR contents.created_at >= $1::timestamptz)
                      AND (
                          $2::bigint IS NULL
                          OR contents.actor_id IN (
                              SELECT followed_actor_id FROM follows
                              WHERE follower_actor_id = $2::bigint
                          )
                      )
                      AND (
                          $6::actor_type IS NULL
                          OR contents.actor_id IN (
                              SELECT id FROM actors WHERE actor_type = $6::actor_type
                          )
                      )
                      -- Faz 18.B, NOTES.md §9.3/§9.6: seviye 0 (taze/doğrulanmamış)
                      -- yazarların içeriği `hot`ta GÖSTERİLMEZ — bkz. modül
                      -- dokümantasyonu "Güven kademesi ve hot filtresi".
                      -- `follower`/`actor_type` filtreleriyle BİREBİR aynı desen
                      -- (üyelik testi, `actors`e alt sorguyla), aynı `page`
                      -- CTE'sinin içinde, `ORDER BY ... LIMIT`'ten önce — iki
                      -- aşamalı yapıyı bozmuyor.
                      AND contents.actor_id IN (
                          SELECT id FROM actors WHERE trust_level >= 1
                      )
                      AND (
                          $3::double precision IS NULL
                          OR (contents.hot_score, contents.id) < ($3::double precision, $4::bigint)
                      )
                    ORDER BY contents.hot_score DESC, contents.id DESC
                    LIMIT $5
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
                    actors.trust_level AS author_trust_level,
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
                cutoff,
                follower,
                cursor_hot,
                cursor_id,
                limit + 1,
                actor_type as Option<ActorType>,
            )
            .fetch_all(pool)
            .await?
        }
    };

    Ok(paginate(
        rows,
        limit,
        |row: &FeedRow| row.id,
        |row: &FeedRow| match sort {
            PostSort::New => SortKey::New {
                created_at: row.created_at,
            },
            PostSort::Top => SortKey::Top { score: row.score },
            PostSort::Hot => SortKey::Hot {
                hot_score: row.hot_score,
            },
        },
        Content::from_feed_row,
    ))
}

/// Feed sorgularının satır tipi.
///
/// `crate::content::ContentRow`'un alan alan aynısı ama o tip
/// `content.rs`'e özel (`pub(crate)` bile değil); `sqlx::query_as!` zaten
/// her sorguda sütun listesini tekrar yazmayı gerektirdiği için ortak bir
/// tip paylaşmanın kazandıracağı bir şey yok — bkz. `content.rs`'teki aynı
/// desen.
pub(crate) struct FeedRow {
    pub(crate) id: i64,
    pub(crate) content_type: ContentType,
    pub(crate) title: Option<String>,
    pub(crate) body: String,
    pub(crate) body_format: BodyFormat,
    pub(crate) metadata: serde_json::Value,
    pub(crate) score: i32,
    pub(crate) upvotes: i32,
    pub(crate) downvotes: i32,
    pub(crate) comment_count: i32,
    pub(crate) hot_score: f64,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) edited_at: Option<DateTime<Utc>>,
    pub(crate) deleted_at: Option<DateTime<Utc>>,
    pub(crate) author_id: i64,
    pub(crate) author_username: String,
    pub(crate) author_actor_type: ActorType,
    pub(crate) author_display_name: Option<String>,
    pub(crate) author_bio: Option<String>,
    pub(crate) author_created_at: DateTime<Utc>,
    pub(crate) author_trust_level: i16,
    pub(crate) author_deleted_at: Option<DateTime<Utc>>,
    pub(crate) tags: Vec<String>,
}

impl Content {
    /// [`FeedRow`] → [`Content`].
    ///
    /// `From` yerine adlandırılmış bir fonksiyon: `Content` için zaten iki
    /// `From` uygulaması var (`content::ContentRow`, `comment::CommentRow`)
    /// ve üçüncüsü hangisinin çağrıldığını okurken belirsizleştiriyordu.
    pub(crate) fn from_feed_row(row: FeedRow) -> Self {
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
                trust_level: row.author_trust_level,
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

/// Son [`RECOMPUTE_WINDOW_DAYS`] günün postlarının `hot_score`'unu yeniden
/// hesaplar; güncellenen satır sayısını döner.
///
/// `crate::tag::cleanup_unused` ile aynı advisory lock deseni (ayrı
/// anahtarla): kilit beklemiyor, başkası tutuyorsa tur atlanıyor ve `Ok(0)`
/// dönüyor.
///
/// **Formül `crate::interaction::set_vote`'takiyle aynı olmak zorunda** —
/// ikisi de aynı sütunu yazıyor. Neden tek bir sabitte toplanamadığı modül
/// dokümantasyonunda.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn recompute_hot_scores(pool: &PgPool) -> Result<u64> {
    let mut conn = pool.acquire().await?;

    let locked = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_lock($1) AS "locked!""#,
        RECOMPUTE_ADVISORY_LOCK_KEY,
    )
    .fetch_one(&mut *conn)
    .await?;

    if !locked {
        tracing::debug!("hot score tazelemesi başka bir instance'da çalışıyor, bu tur atlandı");
        return Ok(0);
    }

    let cutoff = Utc::now() - Duration::days(RECOMPUTE_WINDOW_DAYS);

    let result = sqlx::query!(
        r#"
        UPDATE contents
        SET hot_score = (
            sign(score) * log(greatest(abs(score), 1)::numeric)
            + extract(epoch FROM created_at) / 45000.0
        )::double precision
        WHERE content_type = 'post'::content_type
          AND deleted_at IS NULL
          AND created_at >= $1
        "#,
        cutoff,
    )
    .execute(&mut *conn)
    .await;

    // Kilit her durumda bırakılmalı — `UPDATE` hata verse bile (bkz.
    // `crate::tag::cleanup_unused`'daki aynı gerekçe).
    if let Err(err) = sqlx::query_scalar!(
        r#"SELECT pg_advisory_unlock($1) AS "unlocked!""#,
        RECOMPUTE_ADVISORY_LOCK_KEY,
    )
    .fetch_one(&mut *conn)
    .await
    {
        tracing::warn!(error = %err, "hot score tazelemesi advisory lock'ı bırakılamadı");
    }

    let guncellenen = result?.rows_affected();

    if guncellenen > 0 {
        tracing::info!(guncellenen, "hot score'lar tazelendi");
    }

    Ok(guncellenen)
}
