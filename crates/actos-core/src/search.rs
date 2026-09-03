//! Tam metin arama: içerik (post/yorum) ve actor.
//!
//! `GET /search?q=...&type=post|comment|actor` amac.txt'in de PLAN.md Faz
//! 15'in konusu — bu modül veritabanı tarafını sağlıyor, HTTP'yi bilmiyor
//! (bkz. crate kök dokümantasyonu).
//!
//! ## `unaccent` tuzağı ve çözümü — şema tarafında, burada değil
//!
//! Arama vektörlerinin nasıl üretildiği (`unaccent()`'in STABLE olması
//! yüzünden generated column'da doğrudan kullanılamaması, bunun yerine
//! `unaccent` sözlüğü gömülü özel bir `actos_simple` text search
//! configuration'ı kurulması) burada değil, `migrations/0019_search.up.sql`
//! içinde anlatılıyor. Bu modül yalnızca o konfigürasyonun adını (`
//! 'actos_simple'`) sorgularda kullanıyor — canlı PostgreSQL 18 üzerinde
//! doğrulanmış: `q="surucu"` "sürücü"yü, `q="CAFE"` "Café"yi buluyor.
//!
//! Sorgu ayrıştırmada `websearch_to_tsquery` kullanılıyor (`plainto_tsquery`
//! değil): tırnak, `OR`, `-` destekler ve kullanıcı/ajan girdisinde **asla
//! hata vermez** — bozuk bir sözdizimi sessizce en iyi çabayla yorumlanır,
//! oysa `to_tsquery` sözdizimi hatasında `ERROR` fırlatır ve bunu 400'e
//! çevirmek arama kutusuna yazan sıradan bir kullanıcı için gereksiz bir
//! sürtünme olurdu.
//!
//! ## Sıralama: alaka + tazelik + popülerlik — ÖLÇÜLMÜŞ sabitler
//!
//! [`search_content`]'in sıraladığı `rank` değeri şu formülle hesaplanıyor:
//!
//! ```text
//! rank = ts_rank(search_vector, query) * 10.0
//!      + extract(epoch FROM created_at) / 45000.0 / 1750.0
//!      + sign(score) * log10(greatest(abs(score), 1)) * 0.4
//! ```
//!
//! **Bu modülün ilk sürümü ölçmeden tahmin etmişti ve yanlıştı** — ilk
//! yorum "`ts_rank` tipik olarak 0.01-0.6 bandında" diyordu ve `hot_score`'u
//! (zaten `sign(score)*log10(...) + epoch/45000` olarak İKİ sinyali TEK bir
//! sayıda birleştiren, ayrıca yalnızca oy anında/periyodik işte tazelenen
//! denormalize bir kolon) tek bir sabite (`50000`) bölüyordu. Bu koddaki
//! iki gerçek hata canlı veri üzerinde ölçülerek bulundu:
//!
//! 1. **Ölçüm yanlıştı.** 5 satırlık gerçek bir korpusta (`nvidia` tek
//!    terim, `nvidia rust` iki terim, A=1.0/B=0.4 ağırlıklarıyla) ölçülen
//!    `ts_rank` değerleri: başlıkta tek terim eşleşmesi `0.6687`, başlıkta
//!    iki terim eşleşmesi `0.9991`, gövdede tek terim eşleşmesi `0.2432`.
//!    Yani gerçek bant `0.24`-`1.0`, varsayılan `0.01`-`0.6` değil —
//!    `*10.0` sonrası gerçek bant `2.4`-`10.0`.
//! 2. **`hot_score`'u bölmek iki sinyali birden bozuyordu.** `hot_score`
//!    içindeki zaman terimi (`epoch/45000`) bugün için `~39733`, oy terimi
//!    (`sign(score)*log10(...)`) ise `-3`..`3` mertebesinde — birbirinden
//!    **dört büyüklük mertebesi** uzak iki sinyal. Tek bir sabite (`50000`)
//!    bölmek ikisini birden anlamlı bir ölçeğe getiremiyordu: sonuç, zaman
//!    farkının (`1 yıl ≈ 700/50000 = 0.014`) ve oy farkının (`skor 1→1000 ≈
//!    3/50000 = 0.00006`) ikisinin de `ts_rank*10`'un gerçek bandına
//!    (`2.4`-`10.0`) kıyasla önemsiz kalmasıydı — sıralama pratikte saf
//!    `ts_rank`'e indirgeniyordu, karışım yalnızca kağıt üzerinde vardı.
//!
//! **Çözüm: `hot_score`'u hiç kullanma, iki sinyali ayrı ayrı ve doğrudan
//! `created_at`/`score`'dan hesapla, her birine kendi sabitini ver.**
//! `hot_score` denormalize bir kolon (yalnızca oy anında/periyodik job'da
//! tazeleniyor, bkz. `crate::feed` modül dokümantasyonu) — onu ranking'e
//! sokmak hem bayatlama riski taşıyor hem de içindeki iki bileşeni tek bir
//! sayıdan ayrıştırılamaz kılıyor. `created_at`/`score` ise her zaman
//! **canlı**: `PostSort::Top`'un doğrudan `score` kolonuna göre sıralaması
//! gibi, arama da aynı canlı kolonlara bakıyor.
//!
//! Sabitler **hedeflenen davranışa göre geriye doğru** hesaplandı:
//! "1 yıllık yaş farkı ve 10 katlık skor farkı, her biri ~0.3-0.5 rank
//! değerinde olsun, ama `ts_rank*10`'un gerçek bandını (2.4-10.0) ezmesin."
//!
//! - **Tazelik böleni `1750.0`:** 1 yıl = `31_536_000` saniye,
//!   `epoch/45000` cinsinden `700.8` birim. `700.8 / 1750 ≈ 0.40` —
//!   hedefin tam ortası. 1 günlük fark ise `86400/45000/1750 ≈ 0.0011` —
//!   önemsiz, beklenen (günlük tazelik farkının alakayı ezmesi istenmiyor).
//! - **Popülerlik çarpanı `0.4`:** `sign(score)*log10(max(|score|,1))`
//!   10 katlık bir skor farkında (ör. skor 10 → 100) tam `1.0` değişiyor
//!   (`log10(100)-log10(10)=1`). `1.0 * 0.4 = 0.4` — yine hedefin ortası.
//!
//! Sonuç: **alaka birincil sıralama sinyali** (tek terim ↔ iki terim farkı
//! `~3.3`, gövde ↔ başlık farkı `~4.2` — bkz. yukarıdaki ölçümler), **tazelik
//! ve popülerlik ise karşılaştırılabilir derecede alakalı sonuçlar arasında
//! ölçülebilir bir ikincil ayırt edici** (`~0.4` mertebesinde, alaka farkının
//! onda biri kadar — sırayı gerçekten değiştirebiliyor ama güçlü bir alaka
//! farkını ezmiyor). Bu, `crates/actos-core/tests/search.rs`'teki
//! `skor_farki_...` ve `tazelik_farki_...` testleriyle doğrulandı — ikisi de
//! **eski formülle kırmızıydı** (bkz. o testlerin üzerindeki yorum).
//!
//! Sabitler keyfi değil ama kesin bilim de değil; gerçek arama trafiğiyle
//! yeniden ölçülüp ayarlanabilir (dışa dönük bir sözleşme değil, yalnızca
//! bir sıralama detayı).
//!
//! **`rank` ifadesi SELECT'te (CTE içinde, bkz. aşağıdaki "Performans"
//! bölümü) ve cursor `WHERE` koşulunda İKİ YERDE yazılı** — `sqlx::query_as!`
//! derleme zamanı doğrulaması için sorgunun string **literal** olmasını
//! şart koşuyor, bir Rust sabitine/alt sorguya referans bunu bozar (bkz.
//! `crate::content::list_posts_by_tag` ve `crate::feed` modüllerindeki aynı
//! kısıt). **Biri değişirse diğeri de değişmeli** — tıpkı PLAN.md'nin
//! `hot_score` formülü için zaten not düştüğü aynı tuzak (bkz.
//! `crate::feed` modül dokümantasyonu "Formül neden iki yerde yazılı").
//!
//! ## Performans: neden `LIMIT`'ten önce bir CTE ile satır sayısını kesiyoruz
//!
//! `search_content`'in SELECT'i `contents`'i `actors`/`content_tags`/`tags`
//! ile `JOIN`layıp `array_agg` ile etiketleri topluyor — bu `GROUP BY
//! contents.id, actors.id` gerektiriyor. Eğer `rank`/`ORDER BY`/`LIMIT` bu
//! `GROUP BY`'ın olduğu sorgunun **kendisinde** olsaydı, planlayıcı
//! `ORDER BY ... LIMIT`'i aggregate'in altına itemez: yaygın bir terimle
//! (`rust`, `nvidia`) eşleşen **bütün** satırlar önce gruplanıp diske
//! taşınır (external merge), sonra yalnızca son `limit+1` tanesi alınır.
//! 200.000 eşleşen satırla ölçüldü: `GROUP BY`'ı önce yapan hâliyle
//! `~179ms` (plan: `GroupAggregate rows=200000` + `external merge Disk: 16
//! MB × 3 worker`), sayfayı önce seçen iki aşamalı hâliyle `~29ms` (plan:
//! `GroupAggregate rows=26`) — **~6× hızlanma**, disk yazması sıfıra iniyor.
//!
//! Çözüm: `page` CTE'si önce `rank`'i hesaplayıp `ORDER BY rank DESC, id
//! DESC LIMIT $n` ile **yalnızca istenen sayfanın id'lerini** seçiyor;
//! dıştaki sorgu bu az sayıdaki id için `JOIN`/`array_agg` yapıyor. Bu aynı
//! zamanda `rank`'i bir kez hesaplayıp CTE'nin dışında yeniden kullanma
//! avantajı da veriyor (`ORDER BY "rank!" DESC` — CTE'nin çıktısı).
//!
//! `search_actors`'da bu sorun **yok**: o sorguda hiç `JOIN`/`array_agg`/
//! `GROUP BY` yok (actor'ün etiketi/eki gibi çoğul bir ilişkisi toplanmıyor),
//! yani aggregate'in `LIMIT`'in altına inmemesi diye bir risk yok — teyit
//! edildi, dokunulmadı.
//!
//! **Kapsam notu (Faz 17'de kapandı):** `crate::feed::list_feed`,
//! `crate::content::list_posts_by_tag` ve `crate::content::list_posts_by_actor`
//! da **aynı** deseni taşıyordu (aggregate `ORDER BY`/`LIMIT`'ten önce).
//! Faz 15'te burada yalnızca arama düzeltilmiş, diğerleri Faz 17'ye
//! bırakılmıştı; **Faz 17 üçünü de aynı CTE desenine çevirdi**. Ölçümler ve
//! öncesi/sonrası `EXPLAIN` çıktıları `docs/query-plans.md`'de. Yani bu desen
//! artık kod tabanında dört yerde: sayfalanan ve yanında çoğul bir ilişki
//! (etiket/ek) toplayan **yeni** bir liste ucu yazan da aynı şekli
//! kullanmalı.
//!
//! ## Cursor: mekanizma aynı, anlamı `q`'ya bağlı
//!
//! Cursor, [`crate::cursor::SortKey::Hot`] varyantı **yeniden kullanılarak**
//! taşınıyor — `tag.rs::search`'ün popülerlik listesi için `SortKey::Top`'u
//! (orada "skor" olan alan aslında post sayısı) yeniden kullanmasıyla
//! birebir aynı desen: `cursor.rs`'in imza mekanizması taşınan `f64`'ün
//! *anlamını* bilmiyor, yalnızca sıralamanın türünü (`Hot`) sabitliyor. Bu
//! yüzden yeni bir `SortKey` varyantı eklemeye gerek yok; `hot_score` alanı
//! burada gerçek `hot_score` değil, yukarıdaki `rank` karışımı.
//!
//! **Bir cursor yalnızca üretildiği `q` ile birlikte anlamlıdır.** Cursor
//! kendi başına `q`'yu taşımıyor (imzalı gövdesi yalnızca sürüm + tür +
//! değer + id, bkz. `cursor.rs`) — yani istemci sayfa 2'yi farklı bir `q`
//! ile isterse bu **güvenlik açığı değil**: `rank` değeri her zaman
//! sunucu tarafında yeniden hesaplanıyor (satır id'si dışında hiçbir şey
//! istemciden gelmiyor), olası tek sonuç `WHERE (rank, id) < (eski_rank,
//! eski_id)` koşulunun yeni `q`'nun ürettiği tamamen farklı bir `rank`
//! ölçeğiyle karşılaştırılması — pratikte tuhaf (eksik/atlanmış görünen)
//! ama zararsız bir sayfa. İstemcinin aynı `q` ile sayfalaması **beklenen
//! kullanım**, farklı `q` ile sayfalaması tanımsız davranış.
//!
//! ## Actor araması: önek + trigram + tam metin
//!
//! [`search_actors`] `tag.rs::search`'ün Faz 10'da öğrendiği dersi
//! tekrarlıyor: **kısa sorgularda trigram benzerliği tek başına yetmiyor**
//! (`similarity('nvidia','nv')` = `0.25`, `pg_trgm`'in varsayılan eşiği
//! `0.3` — bkz. `tag::SIMILARITY_THRESHOLD` üzerindeki gerekçe). Bu yüzden
//! üç sinyal birlikte kullanılıyor:
//!
//! 1. **Kullanıcı adı öneki** (`username LIKE 'q%'`) — otomatik tamamlamanın
//!    asıl yolu, sabit büyük bir bonus (`1000.0`) alıyor ki her zaman diğer
//!    iki sinyalin toplamını ezsin (bir önek eşleşmesi asla bir önek
//!    olmayan eşleşmenin altına düşmemeli).
//! 2. **Tam metin** (`username`/`display_name` A, `bio` B ağırlıklı,
//!    `ts_rank`) — "bio'sunda 'rust' geçen kullanıcılar" gibi aramalar için.
//! 3. **Trigram benzerliği** (`similarity(username, q)`) — yazım
//!    hatalarını toparlar (`tag.rs::search` ile aynı gerekçe).
//!
//! Actor'lerde `hot_score` diye bir kavram yok (bu alan yalnızca
//! `contents` tablosunda) — yani actor sıralamasında "hotness" karışımı
//! anlamsız; `rank` yalnızca yukarıdaki üç sinyalin toplamı. Cursor
//! mekanizması yine de aynı (`SortKey::Hot`) çünkü mekanizma sıralamanın
//! *anlamından* bağımsız, yalnızca "azalan bir `f64` + id" taşıyor.
//!
//! ## Boş/anlamsız `q`
//!
//! `tag.rs::search` ile **tutarlı**: `q` verilmemişse ya da normalize
//! edildikten sonra boşsa hata değil, **boş liste** dönüyor. Gerekçe aynı:
//! bir arama kutusu kullanıcı/ajan henüz yazarken (ya da hiç yazmadan) her
//! tuş vuruşunda/istekte bir `400` göstermemeli — boş sorgu "sonuç yok"
//! anlamına gelir, "geçersiz istek" değil. `websearch_to_tsquery` zaten boş
//! girdide boş bir tsquery üretip hiçbir şeyle eşleşmez; burada erken
//! dönüş yalnızca gereksiz bir veritabanı turunu atlıyor.
//!
//! ## Silinmiş içerik/actor hiç görünmez
//!
//! `contents.deleted_at IS NULL` ve `actors.deleted_at IS NULL` filtreleri
//! her iki fonksiyonda da var — `crate::content::list_posts_by_actor` ile
//! aynı karar (bkz. o fonksiyonun dokümantasyonu): bu bir *liste* ucu,
//! silinmiş bir öğeyi satır içinde `[deleted]` göstermek bu görevin
//! kapsamında değil, PLAN.md yalnızca "arama sonuçlarında silinmiş içerik
//! çıkmasın" diyor.

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::PgPool;

use crate::{
    actor::{Page, paginate},
    auth::{ActorRecord, ActorType},
    content::{BodyFormat, Content, ContentType},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    text,
};

/// `tag::SIMILARITY_THRESHOLD` ile aynı gerekçeyle aynı gevşek eşik: kısa
/// kullanıcı adlarında `pg_trgm`'in varsayılanı (`0.3`) çok sıkı kalıyor.
/// Actor araması için ayrı bir sabit — `tag`'inkiyle *aynı değeri*
/// taşıması bir tesadüf değil (ikisi de kısa, tek "kelime"lik tanımlayıcı
/// aramaları hedefliyor) ama modüller birbirine bağımlı olmasın diye
/// paylaşılmıyor.
const ACTOR_SIMILARITY_THRESHOLD: f32 = 0.2;

/// `GET /search`'ün `?type=` parametresi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTarget {
    Post,
    Comment,
    Actor,
}

impl SearchTarget {
    /// `?type=` query parametresini ayrıştırır.
    ///
    /// **`None` de reddedilir** — `content::PostSort::parse`'ın aksine
    /// burada sessiz bir varsayılan yok: post/yorum/actor üç farklı DTO
    /// şekli üretiyor (`ContentSummary` vs. `ActorSummary`), "hiçbiri
    /// belirtilmezse hepsini birden ara" diye anlamlı, tek bir sayfalanmış
    /// yanıt şekli yok. İstemcinin türü açıkça seçmesi gerekiyor.
    ///
    /// # Errors
    /// `raw` `None`'sa ya da tanınan üç değerden biri değilse
    /// [`Error::Validation`].
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        match raw {
            Some("post") => Ok(Self::Post),
            Some("comment") => Ok(Self::Comment),
            Some("actor") => Ok(Self::Actor),
            None => Err(Error::Validation(
                "the type parameter is required (expected: post, comment, actor)".to_owned(),
            )),
            Some(other) => Err(Error::Validation(format!(
                "invalid type value: \"{other}\" (expected: post, comment, actor)"
            ))),
        }
    }
}

/// Bir `Hot` cursor'ını `(rank, id)` bindable çiftine ayırır.
///
/// Bkz. modül dokümantasyonu "Cursor" bölümü: burada taşınan `f64`, gerçek
/// bir `hot_score` değil, [`search_content`]/[`search_actors`]'ın kendi
/// `rank` karışımı — `tag::split_count_cursor`'ın `Top`'u post sayısı için
/// yeniden kullanmasıyla birebir aynı desen.
///
/// # Errors
/// Cursor başka bir sıralamaya aitse [`Error::InvalidCursor`].
fn split_rank_cursor(cursor: Option<Cursor>) -> Result<(Option<f64>, Option<i64>)> {
    match cursor {
        None => Ok((None, None)),
        Some(Cursor {
            sort: SortKey::Hot { hot_score: rank },
            id,
        }) => Ok((Some(rank), Some(id))),
        Some(_) => Err(Error::InvalidCursor),
    }
}

// --- İçerik araması (post/yorum) -------------------------------------------

struct ContentSearchRow {
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
    author_trust_level: i16,
    author_deleted_at: Option<DateTime<Utc>>,
    tags: Vec<String>,
    /// Bkz. modül dokümantasyonu "Sıralama" bölümü — alaka + tazelik +
    /// popülerlik karışımı, `hot_score` kolonundan değil doğrudan
    /// `created_at`/`score`'dan hesaplanıyor. `Content`'in bir alanı değil,
    /// yalnızca sıralama/cursor için taşınıyor (bkz. [`paginate`] çağrısı).
    rank: f64,
}

impl From<ContentSearchRow> for Content {
    fn from(row: ContentSearchRow) -> Self {
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

/// `GET /search?type=post` / `?type=comment`: içerik araması.
///
/// Yalnızca canlı içerik (`deleted_at IS NULL`) ve yalnızca `content_type`
/// eşleşen satırlar. Sıralama ve cursor için bkz. modül dokümantasyonu.
///
/// `query` boş/yalnızca boşluktan ibaretse (normalize edildikten sonra)
/// veritabanına hiç gitmeden boş bir sayfa döner (bkz. modül
/// dokümantasyonu "Boş/anlamsız `q`" bölümü).
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn search_content(
    pool: &PgPool,
    query: &str,
    content_type: ContentType,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Content>> {
    let normalized = text::normalize_text(query);
    if normalized.is_empty() {
        return Ok(Page {
            items: Vec::new(),
            next_cursor: None,
        });
    }

    let (cursor_rank, cursor_id) = split_rank_cursor(cursor)?;

    // `rank` ifadesi burada VE aşağıdaki cursor `WHERE` koşulunda tekrar
    // ediyor — `sqlx::query_as!` sorgunun string literal olmasını şart
    // koşuyor, bir alt sorguya/Rust sabitine çekilemez (bkz. modül
    // dokümantasyonu "Sıralama" bölümü, `crate::feed`'in `hot_score`
    // formülü için düştüğü aynı not). **Biri değişirse diğeri de
    // değişmeli.**
    //
    // `page` CTE'si `rank`'i hesaplayıp `ORDER BY ... LIMIT` ile sayfayı
    // ETİKET JOIN'İNDEN/`GROUP BY`'DAN ÖNCE kesiyor (bkz. modül
    // dokümantasyonu "Performans" bölümü) — yaygın bir terimle yüz binlerce
    // satır eşleşse bile yalnızca `limit+1` tanesi `array_agg` aggregate'ine
    // giriyor.
    let rows = sqlx::query_as!(
        ContentSearchRow,
        r#"
        WITH page AS (
            SELECT
                contents.id,
                (
                    ts_rank(contents.search_vector, websearch_to_tsquery('actos_simple', $1)) * 10.0
                    + extract(epoch FROM contents.created_at) / 45000.0 / 1750.0
                    + sign(contents.score::double precision)
                      * log10(greatest(abs(contents.score), 1)::double precision) * 0.4
                ) AS rank
            FROM contents
            WHERE contents.search_vector @@ websearch_to_tsquery('actos_simple', $1)
              AND contents.content_type = $2::content_type
              AND contents.deleted_at IS NULL
              AND (
                  $3::double precision IS NULL
                  OR (
                      ts_rank(contents.search_vector, websearch_to_tsquery('actos_simple', $1)) * 10.0
                      + extract(epoch FROM contents.created_at) / 45000.0 / 1750.0
                      + sign(contents.score::double precision)
                        * log10(greatest(abs(contents.score), 1)::double precision) * 0.4,
                      contents.id
                  ) < ($3::double precision, $4::bigint)
              )
            ORDER BY rank DESC, contents.id DESC
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
            ) AS "tags!: Vec<String>",
            page.rank AS "rank!"
        FROM page
        JOIN contents ON contents.id = page.id
        JOIN actors ON actors.id = contents.actor_id
        LEFT JOIN content_tags ON content_tags.content_id = contents.id
        LEFT JOIN tags ON tags.id = content_tags.tag_id
        GROUP BY contents.id, actors.id, page.rank
        ORDER BY "rank!" DESC, contents.id DESC
        "#,
        normalized,
        content_type as ContentType,
        cursor_rank,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row: &ContentSearchRow| row.id,
        |row: &ContentSearchRow| SortKey::Hot {
            hot_score: row.rank,
        },
        Content::from,
    ))
}

// --- Actor araması -----------------------------------------------------

struct ActorSearchRow {
    id: i64,
    username: String,
    actor_type: ActorType,
    display_name: Option<String>,
    bio: Option<String>,
    created_at: DateTime<Utc>,
    trust_level: i16,
    /// Bkz. modül dokümantasyonu "Actor araması" bölümü.
    rank: f64,
}

impl From<ActorSearchRow> for ActorRecord {
    fn from(row: ActorSearchRow) -> Self {
        Self {
            id: row.id,
            username: row.username,
            actor_type: row.actor_type,
            display_name: row.display_name,
            bio: row.bio,
            created_at: row.created_at,
            trust_level: row.trust_level,
        }
    }
}

/// `GET /search?type=actor`: actor araması.
///
/// Yalnızca canlı hesaplar (`deleted_at IS NULL`) — bkz. modül
/// dokümantasyonu "Silinmiş içerik/actor hiç görünmez" bölümü. Sıralama
/// için bkz. "Actor araması" bölümü (önek + tam metin + trigram).
///
/// `query` boş/yalnızca boşluktan ibaretse boş bir sayfa döner (aynı karar,
/// bkz. modül dokümantasyonu).
///
/// **Kullanıcı adı karşılaştırmaları küçük harfe çevrilmiş sorguyla
/// yapılıyor:** `actors.username` şema seviyesinde her zaman küçük harf
/// saklanıyor (`ck_actors_username_format`, bkz.
/// `migrations/0002_actors.up.sql`), ama sorgu bunu garanti etmiyor
/// (`?q=NVIDIA`). Tam metin tarafı (`ts_rank`/`websearch_to_tsquery`) buna
/// ihtiyaç duymuyor — `to_tsvector`'ın `simple` sözlüğü zaten büyük/küçük
/// harften bağımsız lexeme üretiyor (canlı veritabanında doğrulandı: `q=
/// "CAFE"` "Café"yi buluyor) — ama düz `LIKE`/`similarity()` karşılaştırması
/// büyük/küçük harf duyarlı, bu yüzden yalnızca o ikisi için ayrı, küçültülmüş
/// bir sorgu string'i kullanılıyor.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn search_actors(
    pool: &PgPool,
    query: &str,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<ActorRecord>> {
    let normalized = text::normalize_text(query);
    if normalized.is_empty() {
        return Ok(Page {
            items: Vec::new(),
            next_cursor: None,
        });
    }
    let username_query = normalized.to_lowercase();

    let (cursor_rank, cursor_id) = split_rank_cursor(cursor)?;

    let rows = sqlx::query_as!(
        ActorSearchRow,
        r#"
        SELECT
            actors.id,
            actors.username::text AS "username!",
            actors.actor_type AS "actor_type: ActorType",
            actors.display_name,
            actors.bio,
            actors.created_at,
            actors.trust_level,
            (
                CASE WHEN actors.username::text LIKE $2 || '%' THEN 1000.0 ELSE 0.0 END
                + ts_rank(actors.search_vector, websearch_to_tsquery('actos_simple', $1)) * 10.0
                + similarity(actors.username::text, $2)
            ) AS "rank!"
        FROM actors
        WHERE actors.deleted_at IS NULL
          AND (
              actors.search_vector @@ websearch_to_tsquery('actos_simple', $1)
              OR actors.username::text LIKE $2 || '%'
              OR similarity(actors.username::text, $2) >= $3
          )
          AND (
              $4::double precision IS NULL
              OR (
                  CASE WHEN actors.username::text LIKE $2 || '%' THEN 1000.0 ELSE 0.0 END
                  + ts_rank(actors.search_vector, websearch_to_tsquery('actos_simple', $1)) * 10.0
                  + similarity(actors.username::text, $2),
                  actors.id
              ) < ($4::double precision, $5::bigint)
          )
        ORDER BY "rank!" DESC, actors.id DESC
        LIMIT $6
        "#,
        normalized,
        username_query,
        ACTOR_SIMILARITY_THRESHOLD,
        cursor_rank,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row: &ActorSearchRow| row.id,
        |row: &ActorSearchRow| SortKey::Hot {
            hot_score: row.rank,
        },
        ActorRecord::from,
    ))
}
