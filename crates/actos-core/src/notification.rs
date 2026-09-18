//! Bildirimler: `notifications` tablosu, yazma yolu ve `GET /me/inbox`.
//!
//! Gerekçe (neden bu var, neden şimdi) NOTES.md §1'de: bir ajan/insan
//! "postuma yanıt geldi mi?" sorusunu N yoklama isteği yerine **1 istekle**
//! yanıtlayabilsin. Şema `migrations/0021_notifications.up.sql`'de, kolon
//! kolon gerekçeli.
//!
//! ## Yazma yolu: her zaman çağıranın transaction'ında
//!
//! [`create_notification`] `&mut PgConnection` alır, kendi `pool.begin()`
//! **yapmaz** — `crate::moderation::log_action`'la aynı desen ve aynı
//! gerekçe: bildirim satırı, onu üreten eylemle (yorum oluşturma, takip,
//! moderasyon eylemi) **aynı transaction'da**, commit ile birlikte kalıcı
//! olmalı. Ayrı bir transaction'da yazılsaydı, eylem başarılı olup bildirim
//! yazımı (ör. bağlantı kopması) başarısız olduğunda "yorum var ama kimse
//! haberdar edilmedi" gibi sessiz bir tutarsızlık ortaya çıkardı.
//!
//! ## Fan-out sınırı: kök yazarı + doğrudan ebeveyn, TÜM ATALAR DEĞİL
//!
//! `crate::comment::create_comment`, yeni bir yorum eklendiğinde en fazla
//! iki bildirim üretir: (a) kök postun yazarına `CommentOnPost`, (b) —
//! yalnızca yanıt bir posta değil bir yoruma verildiyse — o yorumun
//! **doğrudan** yazarına `ReplyToComment`. Ağaçtaki diğer atalar (ör. 32
//! seviyelik bir dalın ortasındaki yorumlar) bildirim ALMAZ. Bu bilinçli:
//! `crate::comment::increment_ancestor_counts` sayaç için tüm atalara
//! dokunuyor çünkü sayaç "bu ağaçta kaç düğüm var" sorusuna cevap veriyor,
//! ama bildirim "sana bir şey oldu mu" sorusuna cevap veriyor — 32
//! seviyelik bir dalda yaprak bir yoruma gelen TEK bir yanıt, kökten yaprağa
//! kadar 32 kişiyi bilgilendirseydi ("post yazarının torunun torununun...
//! yorumuna yanıt geldi" gibi anlamsız bir zincirleme), hem gürültü
//! yaratırdı hem de yazma yolunu O(derinlik) yapardı — sayaç güncellemesi
//! zaten bunu yapıyor ama o denormalize bir sayı, bildirim tekil, kalıcı bir
//! satır ve OKUNAN bir gelen kutusu; ikisinin maliyet/fayda dengesi farklı.
//!
//! ## Kendi eylemin sana bildirim üretmez
//!
//! [`create_notification`], `actor_id == Some(recipient_actor_id)` ise
//! **hiçbir satır eklemeden** başarıyla döner. Tek merkezi kontrol noktası
//! burası — her çağıran tarafın (yorum, takip, moderasyon) kendi başına
//! "acaba kendime mi bildirim gönderiyorum" kontrolü yazmasına gerek yok,
//! dolayısıyla unutulamaz. Kendi postuna kendi yorumu, kendi kendini takip
//! zaten başka katmanlarda engelli (bkz. `crate::interaction::follow`),
//! ama bu kontrol yine de burada: savunma tek bir yerde, sebebi ne olursa
//! olsun (bugün engelli bir eylem yarın izin verilir hâle gelirse bile).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::{
    actor::{Page, paginate},
    auth::{ActorRecord, ActorType},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
};

/// Bildirim türü (`migrations/0021_notifications.up.sql` → `notification_kind`).
///
/// `direct_message` bilerek burada YOK — DM v1 kapsamı dışında (bkz. modül
/// üstündeki migration yorumu ve NOTES.md §5). Eklenmesi gerektiğinde pg
/// enum'a `ALTER TYPE ... ADD VALUE` ile eklenir, bu enum'a yeni bir varyant
/// eklemek yeterli olur — bu dosyada başka hiçbir şeyin değişmesi gerekmez.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(type_name = "notification_kind", rename_all = "snake_case")]
pub enum NotificationKind {
    CommentOnPost,
    ReplyToComment,
    NewFollower,
    ModerationAction,
}

impl NotificationKind {
    /// HTTP yanıtında kullanılan sabit metin gösterimi.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommentOnPost => "comment_on_post",
            Self::ReplyToComment => "reply_to_comment",
            Self::NewFollower => "new_follower",
            Self::ModerationAction => "moderation_action",
        }
    }
}

/// Bir bildirim satırı, `notifications` tablosunun domain karşılığı.
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: i64,
    pub kind: NotificationKind,
    /// Bildirimi tetikleyen actor. `None` yalnızca sistem kaynaklı olaylarda
    /// (bkz. `notifications.actor_id` sütun yorumu) — bugün üreten hiçbir
    /// yol yok, ama tip bunu ifade edebiliyor.
    pub actor: Option<ActorRecord>,
    /// `"content"` ya da `"actor"` — bkz. `notifications.target_type` sütun
    /// yorumu. Bilerek serbest metin, kapalı bir enum değil (gerekçe aynı
    /// yerde).
    pub target_type: String,
    pub target_id: i64,
    /// Tür başına opsiyonel ek veri; boşsa `{}` (bkz.
    /// `notifications.payload` sütun yorumu — bilerek zorunlu bir `preview`
    /// alanı YOK).
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

// --- Yazma: bildirim üretme --------------------------------------------

/// Bir bildirim satırı ekler — **çağıranın transaction'ında**, tetikleyen
/// eylemle atomik olsun diye (bkz. modül dokümantasyonu).
///
/// `actor_id == Some(recipient_actor_id)` ise (kendi eylemin sana bildirim
/// üretmez, bkz. modül dokümantasyonu) sessizce hiçbir şey yapmadan `Ok(())`
/// döner — bu bir hata değil, beklenen bir kısayol.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn create_notification(
    tx: &mut PgConnection,
    recipient_actor_id: i64,
    kind: NotificationKind,
    actor_id: Option<i64>,
    target_type: &str,
    target_id: i64,
    payload: serde_json::Value,
) -> Result<()> {
    if actor_id == Some(recipient_actor_id) {
        return Ok(());
    }

    sqlx::query!(
        r#"
        INSERT INTO notifications (recipient_actor_id, kind, actor_id, target_type, target_id, payload)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        recipient_actor_id,
        kind as NotificationKind,
        actor_id,
        target_type,
        target_id,
        payload,
    )
    .execute(&mut *tx)
    .await?;

    Ok(())
}

/// [`create_notification`]'ı, aynı **fiziksel eylemden** kaynaklanan birden
/// fazla olası alıcı arasında **tekilleştirerek** çağırır.
///
/// `crate::comment::create_comment`'in fan-out'u için var: kök post yazarı
/// ile doğrudan ebeveyn yorumun yazarı aynı actor olabilir (ör. post
/// sahibinin kendi postuna açtığı bir yorum zincirinde birine yanıt
/// verilmesi) — bu durumda aynı actor'e, aynı yeni yorum için iki ayrı satır
/// (`comment_on_post` + `reply_to_comment`) yazmak, tek bir olayı iki kez
/// bildirmek olurdu. `seen`, bu fonksiyonu çağıran taraf boyunca paylaşılan
/// bir `HashSet` — bir actor bir kez bildirildiyse `seen` onu tutar, sonraki
/// çağrılar o actor için sessizce atlanır.
///
/// `pub(crate)`: yalnızca bu crate içindeki yazma yollarının (comment/
/// interaction/moderation) kullanacağı bir yardımcı, HTTP katmanının bilmesi
/// gereken bir şey değil.
///
/// # Errors
/// [`create_notification`] ile aynı.
///
/// `#[allow(clippy::too_many_arguments)]`: parametreleri bir struct'a
/// toplamak burada gerçek bir okunabilirlik kazancı sağlamıyor —
/// [`create_notification`]'ı bire bir sarmalıyor (tek fark: `seen` ve
/// tekilleştirme), aynı yedi parametre + `seen` zaten çağıranların
/// (`crate::comment::create_comment`) alan adlarıyla eşleşiyor; ayrı bir
/// struct yalnızca çağrı yerinde bir dolgu inşası ekler.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn notify_once(
    tx: &mut PgConnection,
    seen: &mut HashSet<i64>,
    recipient_actor_id: i64,
    kind: NotificationKind,
    actor_id: Option<i64>,
    target_type: &str,
    target_id: i64,
    payload: serde_json::Value,
) -> Result<()> {
    if !seen.insert(recipient_actor_id) {
        return Ok(());
    }
    create_notification(
        tx,
        recipient_actor_id,
        kind,
        actor_id,
        target_type,
        target_id,
        payload,
    )
    .await
}

// --- Okuma: `GET /me/inbox` ---------------------------------------------

/// [`Notification`]'ın sorgu satırı karşılığı. Tetikleyen actor `LEFT JOIN`
/// ile geliyor (`actor_id` nullable olduğu için) — bu yüzden `actor_*`
/// alanların hepsi `Option`, `actor_id` `Some` olduğunda diğerlerinin de
/// `Some` olması FK bütünlüğüyle garanti (bkz. [`row_into_notification`]).
struct NotificationRow {
    id: i64,
    kind: NotificationKind,
    actor_id: Option<i64>,
    actor_username: Option<String>,
    actor_actor_type: Option<ActorType>,
    actor_display_name: Option<String>,
    actor_bio: Option<String>,
    actor_created_at: Option<DateTime<Utc>>,
    target_type: String,
    target_id: i64,
    payload: serde_json::Value,
    created_at: DateTime<Utc>,
    read_at: Option<DateTime<Utc>>,
}

/// [`NotificationRow`]'u [`Notification`]'a çevirir.
///
/// `actor_id` doluyken diğer `actor_*` alanlarından biri `NULL` gelmesi,
/// `notifications.actor_id`'nin `actors(id)`'ye FK olması sayesinde asla
/// olmaması gereken bir durum — yine de bir `unwrap`/`expect` yerine, bu
/// tutarsızlığı sunucunun kendi hatası sayıp [`Error::Internal`] dönüyoruz
/// (bkz. `crate::actor::update_profile`'daki aynı desen: "olmamalı ama
/// olursa panik değil, teşhis edilebilir bir iç hata").
fn row_into_notification(row: NotificationRow) -> Result<Notification> {
    let actor = match row.actor_id {
        None => None,
        Some(id) => Some(ActorRecord {
            id,
            username: row.actor_username.ok_or_else(|| {
                Error::Internal(format!(
                    "notifications: actor_id={id} is set but actor_username is NULL"
                ))
            })?,
            actor_type: row.actor_actor_type.ok_or_else(|| {
                Error::Internal(format!(
                    "notifications: actor_id={id} is set but actor_type is NULL"
                ))
            })?,
            display_name: row.actor_display_name,
            bio: row.actor_bio,
            created_at: row.actor_created_at.ok_or_else(|| {
                Error::Internal(format!(
                    "notifications: actor_id={id} is set but actor_created_at is NULL"
                ))
            })?,
        }),
    };

    Ok(Notification {
        id: row.id,
        kind: row.kind,
        actor,
        target_type: row.target_type,
        target_id: row.target_id,
        payload: row.payload,
        created_at: row.created_at,
        read_at: row.read_at,
    })
}

/// `GET /me/inbox`: çağıranın bildirimleri, en yeni önce, keyset cursor'lu.
///
/// **Yeni bir sayfalama icat edilmiyor** — `crate::cursor` aynen kullanılıyor
/// (bkz. NOTES.md §1). Sıralama her zaman [`SortKey::New`]
/// (`created_at DESC, id DESC`), `crate::actor`/`crate::moderation`'daki
/// listelerle aynı desen.
///
/// `unread_only`: `true` ise yalnızca `read_at IS NULL` satırlar. Tek bir
/// statik sorguda `(NOT $2::boolean OR read_at IS NULL)` koşuluyla ifade
/// ediliyor — `crate::comment::fetch_children_page`'in `sort`'a göre iki
/// ayrı sorgu bloğu yazmasının aksine, burada `ORDER BY` sabit kaldığı için
/// (yalnızca bir `WHERE` koşulu değişiyor) tek bir sorgu yeterli ve tercih
/// edilen yol.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// beklenmeyen bir veri tutarsızlığı [`Error::Internal`] (bkz.
/// [`row_into_notification`]); veritabanı hatası [`Error::Database`].
pub async fn list_inbox(
    pool: &PgPool,
    actor_id: i64,
    unread_only: bool,
    cursor: Option<Cursor>,
    limit: i64,
    viewer_communities: &[i64],
) -> Result<Page<Notification>> {
    let (cursor_created_at, cursor_id) = match cursor {
        None => (None, None),
        Some(Cursor {
            sort: SortKey::New { created_at },
            id,
        }) => (Some(created_at), Some(id)),
        Some(_) => return Err(Error::InvalidCursor),
    };

    // Bir bildirim `target_type = 'content'` ise yalnızca **işaret ettiği
    // içerik görünürse** listelenir (Faz 4A): özel bir topluluktan çıkınca o
    // topluluktaki bir yoruma gelen bildirim gelen kutusunda kalmamalı.
    // `actor` hedefli bildirimler (yeni takipçi) ve ileride topluluk hedefli
    // olanlar bu kapıya takılmaz — hedefleri bir topluluk içeriği değil.
    let rows = sqlx::query_as!(
        NotificationRow,
        r#"
        SELECT
            n.id,
            n.kind AS "kind: NotificationKind",
            n.actor_id,
            actors.username AS actor_username,
            actors.actor_type AS "actor_actor_type: ActorType",
            actors.display_name AS actor_display_name,
            actors.bio AS actor_bio,
            actors.created_at AS actor_created_at,
            n.target_type,
            n.target_id,
            n.payload,
            n.created_at,
            n.read_at
        FROM notifications AS n
        LEFT JOIN actors ON actors.id = n.actor_id
        LEFT JOIN contents AS c
               ON n.target_type = 'content' AND c.id = n.target_id
        WHERE n.recipient_actor_id = $1
          AND (NOT $2::boolean OR n.read_at IS NULL)
          AND (n.target_type <> 'content'
               OR content_visible_to(c.community_id, $6::bigint[]))
          AND (
              $3::timestamptz IS NULL
              OR (n.created_at, n.id) < ($3::timestamptz, $4::bigint)
          )
        ORDER BY n.created_at DESC, n.id DESC
        LIMIT $5
        "#,
        actor_id,
        unread_only,
        cursor_created_at,
        cursor_id,
        limit + 1,
        viewer_communities,
    )
    .fetch_all(pool)
    .await?;

    // `paginate` satır tipinden id/sıralama anahtarı türetmek için ham satırı
    // istiyor, ama dönüşüm burada fallible (`row_into_notification` `Result`
    // döner) — `paginate`'in `into_item` kapanışı fallible değil (bkz.
    // `crate::actor::paginate` imzası). Bu yüzden önce TÜM satırları
    // dönüştürüp `Vec<Notification>` üretiyoruz, `paginate`'i o üzerinden
    // (kimlik dönüşümüyle) çağırıyoruz.
    let items = rows
        .into_iter()
        .map(row_into_notification)
        .collect::<Result<Vec<_>>>()?;

    Ok(paginate(
        items,
        limit,
        |n: &Notification| n.id,
        |n: &Notification| SortKey::New {
            created_at: n.created_at,
        },
        |n| n,
    ))
}

/// `GET /me/inbox` yanıtındaki `unread_count`: çağıranın okunmamış bildirim
/// sayısı. `idx_notifications_recipient_unread` kısmi index'i sayesinde bu
/// sayım, tabloyu değil yalnızca okunmamış satırları tarar (bkz. migration
/// yorumu).
///
/// **Aynı görünürlük filtresi** ([`list_inbox`] ile birebir): sayaç,
/// listeden düşürülen bir bildirimi saymaya devam ederse `unread_count`
/// gizli içeriğin varlığını sızdırırdı (Faz 4A).
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn count_unread(pool: &PgPool, actor_id: i64, viewer_communities: &[i64]) -> Result<i64> {
    let count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM notifications AS n
        LEFT JOIN contents AS c
               ON n.target_type = 'content' AND c.id = n.target_id
        WHERE n.recipient_actor_id = $1
          AND n.read_at IS NULL
          AND (n.target_type <> 'content'
               OR content_visible_to(c.community_id, $2::bigint[]))
        "#,
        actor_id,
        viewer_communities,
    )
    .fetch_one(pool)
    .await?;

    Ok(count)
}

// --- Okundu işaretleme ---------------------------------------------------

/// Tek bir bildirimi okundu işaretler. **İdempotent:** zaten okunmuş bir
/// satıra tekrar uygulanırsa `read_at` İLERİ ATILMAZ (`COALESCE`), yine
/// başarı döner.
///
/// Sahiplik kontrolü `WHERE ... AND recipient_actor_id = $2` ile sorgunun
/// içinde: başka bir actor'ün bildirimini işaretlemeye çalışmak, "var ama
/// senin değil" ile "hiç yok" ayrımını sızdırmadan aynı [`Error::NotFound`]
/// ile sonuçlanır (`crate::comment::resolve_parent`'teki "başka bir postun
/// yorumuna yanıt" ile aynı gerekçe: mevcudiyet bilgisi sızdırılmıyor).
///
/// # Errors
/// Bildirim yoksa ya da çağırana ait değilse [`Error::NotFound`]; veritabanı
/// hatası [`Error::Database`].
pub async fn mark_read(pool: &PgPool, actor_id: i64, notification_id: i64) -> Result<()> {
    let updated = sqlx::query!(
        r#"
        UPDATE notifications
        SET read_at = COALESCE(read_at, now())
        WHERE id = $1 AND recipient_actor_id = $2
        RETURNING id
        "#,
        notification_id,
        actor_id,
    )
    .fetch_optional(pool)
    .await?;

    updated.map(|_| ()).ok_or(Error::NotFound("notification"))
}

/// Toplu okundu işaretleme: "şu cursor'a kadar hepsi". `cursor` `None` ise
/// çağıranın **tüm** okunmamış bildirimleri okundu işaretlenir.
///
/// `cursor`, `GET /me/inbox`'ın döndürdüğü bir `next_cursor` — yani "şu ana
/// kadar gördüğüm sayfaların sınırı". Liste en yeniden eskiye sıralandığı
/// (`DESC`) için, cursor'ın işaret ettiği satır ve ondan **daha yeni** olan
/// her şey (`(created_at, id) >= cursor`) istemcinin zaten görmüş olduğu
/// aralık — o yüzden "kadar" burada `>=` ile ifade ediliyor, `<=` değil: bir
/// sonraki sayfanın (henüz görülmemiş, daha eski) satırlarına dokunulmaması
/// gerekiyor.
///
/// **Tek `UPDATE`, tek istek** — 200 bildirimi tek tek `mark_read` ile
/// işaretlemek (200 istek) yerine. **İdempotent:** `read_at IS NULL` filtresi
/// ve `COALESCE` sayesinde aynı çağrı (aynı ya da daha eski bir cursor'la)
/// tekrarlanırsa yalnızca gerçekten hâlâ okunmamış satırlar etkilenir, hata
/// üretmez.
///
/// Döndürülen değer kaç satırın (yeni) okundu işaretlendiği — istemcinin
/// "kaç bildirim okundu" diye ayrıca sormasına gerek kalmasın diye.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn mark_all_read(pool: &PgPool, actor_id: i64, cursor: Option<Cursor>) -> Result<u64> {
    let (cursor_created_at, cursor_id) = match cursor {
        None => (None, None),
        Some(Cursor {
            sort: SortKey::New { created_at },
            id,
        }) => (Some(created_at), Some(id)),
        Some(_) => return Err(Error::InvalidCursor),
    };

    let result = sqlx::query!(
        r#"
        UPDATE notifications
        SET read_at = COALESCE(read_at, now())
        WHERE recipient_actor_id = $1
          AND read_at IS NULL
          AND (
              $2::timestamptz IS NULL
              OR (created_at, id) >= ($2::timestamptz, $3::bigint)
          )
        "#,
        actor_id,
        cursor_created_at,
        cursor_id,
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}
