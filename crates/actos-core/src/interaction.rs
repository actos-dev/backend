//! Etkileşimler: oy verme, takip etme ve kaydetme.
//!
//! Üçü de aynı biçimde çalıştığı için tek modülde: bir actor ile bir hedef
//! (içerik ya da başka bir actor) arasında **en fazla bir satır** olan,
//! **idempotent** ilişkiler. Üçünün de HTTP karşılığı `PUT`/`DELETE` —
//! `POST` değil, çünkü aynı isteği iki kez göndermek yeni bir şey
//! yaratmamalı (buglu bir ajanın aynı oyu iki kez göndermesi sayaçları
//! kaydırmamalı).
//!
//! ## Sayaçlar neden trigger ile türetilmiyor
//!
//! `migrations/0009_votes.up.sql` üzerindeki COMMENT'in kararı: `contents`
//! üzerindeki `score`/`upvotes`/`downvotes` sayaçları `votes` tablosundan
//! trigger ile türetilmiyor, oyu yazan işlemle **aynı transaction içinde**
//! uygulama katmanı güncelliyor. Bu modül o sözleşmeyi yerine getiriyor:
//! [`set_vote`] oy satırını ve sayaçları tek transaction'da, içerik satırını
//! `FOR UPDATE` ile kilitleyerek değiştiriyor.
//!
//! Kilit şart: kilitsiz bir "oku, hesapla, yaz" döngüsünde aynı içeriğe
//! eşzamanlı gelen iki oy birbirinin okumasını görmez ve sayaç kayar.
//! Kilit, aynı içeriğe gelen oyları serileştiriyor — farklı içeriklere
//! gelen oylar birbirini beklemiyor.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    actor::{Page, paginate, resolve_live_actor_id},
    auth::{ActorRecord, ActorType},
    community::CommunityRef,
    content::{BodyFormat, Content, ContentType},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    notification::{self, NotificationKind},
};

/// Bir actor'ün bir içeriğe verdiği oy: `-1`, `0` (oy yok) veya `1`.
///
/// `0` **veritabanında saklanmaz** — oy geri çekildiğinde satır silinir
/// (bkz. `migrations/0009_votes.up.sql`, `ck_votes_value`). Sıfır yalnızca
/// API sınırında "oyumu geri çek" isteğini ve "bu içerikte oyum yok"
/// yanıtını ifade eden bir değer.
pub type VoteValue = i16;

/// [`set_vote`]'un sonucu: işlem sonrası içeriğin sayaç durumu.
///
/// İstemciye ekstra bir `GET` attırmamak için dönüyor — bir ajanın oy
/// verip hemen yeni skoru görmesi tipik akış.
#[derive(Debug, Clone, Copy)]
pub struct VoteOutcome {
    pub value: VoteValue,
    pub score: i32,
    pub upvotes: i32,
    pub downvotes: i32,
}

/// `PUT /contents/{id}/vote`: oy ver, değiştir ya da geri çek.
///
/// **İdempotent:** aynı değer ikinci kez gönderilirse sayaçlar değişmez.
/// `value = 0` oyu geri çeker (satır silinir); zaten oy yokken `0`
/// göndermek de hatasız geçer.
///
/// **Kendi içeriğine oy vermek engelli** (PLAN.md Faz 11'de sabitlenen
/// karar): `Error::Forbidden`.
///
/// `hot_score` burada, oyla **aynı transaction'da** yeniden hesaplanıyor.
/// Formülün kendisi ve neden iki yerde yazılı olduğu [`crate::feed`] modül
/// dokümantasyonunda; **buradaki ifade oradakiyle aynı kalmalı.** Periyodik
/// iş ([`crate::feed::recompute_hot_scores`]) yalnızca zaman ilerledikçe
/// kayan değerleri tazeliyor; anlık güncelleme burada.
///
/// ## The score is now a flat sum (REFACTOR.md §3)
///
/// `contents.score` was, for a while, `sum(value * weight)` — `weight` was
/// a 0/1 multiplier derived from the voter's `trust_level` (a fresh
/// account's vote was recorded but didn't contribute to the score). With
/// trust level removed entirely (see REFACTOR.md §3), `votes.weight` is
/// gone too: the score is now a flat `sum(value)`, every vote at full
/// weight.
///
/// **When a vote is changed or withdrawn, the previous contribution is
/// computed from `onceki_value`** — there's no longer a weight stored
/// separately on the row, the delta is simply `value - onceki_value`. This
/// preserves, with a simpler computation, the same invariant that had to
/// hold even when there was a weight (withdrawing a vote must never shift
/// the score by anything other than its own contribution) — test:
/// `crates/actos-core/tests/interaction.rs`.
///
/// # Errors
/// `value` `-1`/`0`/`1` dışındaysa [`Error::Validation`]; içerik yoksa
/// [`Error::NotFound`]; silinmişse [`Error::Gone`]; içerik çağıranın
/// kendisine aitse [`Error::Forbidden`]; veritabanı hatası
/// [`Error::Database`].
pub async fn set_vote(
    pool: &PgPool,
    actor_id: i64,
    content_id: i64,
    value: VoteValue,
    viewer_communities: &[i64],
) -> Result<VoteOutcome> {
    if !matches!(value, -1..=1) {
        return Err(Error::Validation(
            "vote value must be -1, 0, or 1".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;

    // İçerik satırı burada kilitleniyor: bundan sonraki okuma-hesaplama-yazma
    // dizisi aynı içerik için serileşiyor (bkz. modül dokümantasyonu).
    //
    // Görünmeyen özel içerik `404` (Faz 4A): okuyucunun göremediği bir
    // topluluktaki posta oy vermek onun var olduğunu doğrulamaz.
    let content = sqlx::query!(
        r#"
        SELECT actor_id, deleted_at
        FROM contents
        WHERE id = $1
          AND content_visible_to(contents.community_id, $2::bigint[])
        FOR UPDATE
        "#,
        content_id,
        viewer_communities,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("content"))?;

    if content.deleted_at.is_some() {
        return Err(Error::Gone("content"));
    }
    if content.actor_id == actor_id {
        return Err(Error::Forbidden);
    }

    // Previous vote — the delta of the old contribution will be computed from this.
    let mevcut = sqlx::query!(
        r#"SELECT value FROM votes WHERE actor_id = $1 AND content_id = $2"#,
        actor_id,
        content_id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let onceki_value = mevcut.map_or(0i16, |r| r.value);

    // Counter differences are derived from the previous and new values;
    // no need to recount "how many votes there were." `upvotes`/`downvotes`
    // are still a raw direction count; `score_delta` is now a plain
    // difference too (see the function documentation, "The score is now a
    // flat sum").
    let upvote_delta = i32::from(value == 1) - i32::from(onceki_value == 1);
    let downvote_delta = i32::from(value == -1) - i32::from(onceki_value == -1);
    let score_delta = i32::from(value) - i32::from(onceki_value);

    if value == 0 {
        sqlx::query!(
            r#"DELETE FROM votes WHERE actor_id = $1 AND content_id = $2"#,
            actor_id,
            content_id,
        )
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query!(
            r#"
            INSERT INTO votes (actor_id, content_id, value)
            VALUES ($1, $2, $3)
            ON CONFLICT (actor_id, content_id) DO UPDATE SET value = EXCLUDED.value
            "#,
            actor_id,
            content_id,
            value,
        )
        .execute(&mut *tx)
        .await?;
    }

    // Sayaçlar ve `hot_score` tek `UPDATE`'te. `hot_score` **yeni** skordan
    // hesaplanıyor (`score + $4`), çünkü `SET` içindeki `score` hâlâ eski
    // değeri okur — SQL'de bir `UPDATE`'in `SET` ifadeleri satırın güncelleme
    // öncesi hâlini görür.
    let guncel = sqlx::query!(
        r#"
        UPDATE contents
        SET upvotes = upvotes + $2,
            downvotes = downvotes + $3,
            score = score + $4,
            hot_score = (
                sign(score + $4) * log(greatest(abs(score + $4), 1)::numeric)
                + extract(epoch FROM created_at) / 45000.0
            )::double precision
        WHERE id = $1
        RETURNING score, upvotes, downvotes
        "#,
        content_id,
        upvote_delta,
        downvote_delta,
        score_delta,
    )
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(VoteOutcome {
        value,
        score: guncel.score,
        upvotes: guncel.upvotes,
        downvotes: guncel.downvotes,
    })
}

/// `GET /me/votes?content_ids=...`: çağıranın verilen içeriklerdeki oyları.
///
/// Feed'de her post için ayrı bir istek atmamak için toplu sorgu (PLAN.md
/// Faz 11). Yalnızca **oy verilmiş** içerikler dönüyor; listede olmayan bir
/// id "oy yok" demek — sıfır dolu satırlar göndermek yanıtı boşuna şişirirdi.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn votes_for(
    pool: &PgPool,
    actor_id: i64,
    content_ids: &[i64],
    viewer_communities: &[i64],
) -> Result<Vec<(i64, VoteValue)>> {
    if content_ids.is_empty() {
        return Ok(Vec::new());
    }

    // İçerikle JOIN şart (Faz 4A): görünmeyen özel içerik için oy **değeri**
    // dönmez. Aksi hâlde bu uç, artık göremediği bir içerikte kendi oyunu
    // hatırlatarak o içeriğin var olduğunu doğrulardı.
    let rows = sqlx::query!(
        r#"
        SELECT votes.content_id, votes.value
        FROM votes
        JOIN contents ON contents.id = votes.content_id
        WHERE votes.actor_id = $1
          AND votes.content_id = ANY($2::bigint[])
          AND content_visible_to(contents.community_id, $3::bigint[])
        "#,
        actor_id,
        content_ids,
        viewer_communities,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| (r.content_id, r.value)).collect())
}

// --- Takip ------------------------------------------------------------------

/// `PUT /actors/{username}/follow`: idempotent takip.
///
/// Zaten takip ediliyorsa hiçbir şey yapmaz (`ON CONFLICT DO NOTHING`) ve
/// yine başarı döner — `PUT`'un sözleşmesi bu.
///
/// Kendini takip etmek `ck_follows_no_self` ile şema seviyesinde de yasak,
/// ama burada önden reddediliyor ki istemci 500 değil anlaşılır bir hata
/// alsın (`crate::comment::create_comment`'teki aynı gerekçe).
///
/// **Bildirim yalnızca gerçekten YENİ bir takipte üretilir** — `RETURNING`
/// ile `INSERT`in fiilen bir satır ekleyip eklemediği (`ON CONFLICT DO
/// NOTHING` yüzünden sessizce hiçbir şey yapmamış olabilir) ayırt ediliyor.
/// Aksi hâlde `PUT`'un idempotent doğası gereği aynı takibi tekrar tekrar
/// gönderen bir istemci, takip edilen actor'ün gelen kutusunu her seferinde
/// "yeni takipçi" bildirimiyle doldururdu.
///
/// # Errors
/// Kullanıcı adı yoksa [`Error::NotFound`]; hedef silinmişse [`Error::Gone`];
/// kendini takip denemesi [`Error::Validation`]; veritabanı hatası
/// [`Error::Database`].
pub async fn follow(pool: &PgPool, follower_id: i64, username: &str) -> Result<()> {
    let followed_id = resolve_live_actor_id(pool, username).await?;

    if followed_id == follower_id {
        return Err(Error::Validation(
            "an actor cannot follow itself".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;

    let inserted = sqlx::query!(
        r#"
        INSERT INTO follows (follower_actor_id, followed_actor_id)
        VALUES ($1, $2)
        ON CONFLICT (follower_actor_id, followed_actor_id) DO NOTHING
        RETURNING follower_actor_id
        "#,
        follower_id,
        followed_id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    if inserted.is_some() {
        notification::create_notification(
            &mut tx,
            followed_id,
            NotificationKind::NewFollower,
            Some(follower_id),
            "actor",
            follower_id,
            serde_json::json!({}),
        )
        .await?;
    }

    tx.commit().await?;

    Ok(())
}

/// `DELETE /actors/{username}/follow`: idempotent takibi bırakma.
///
/// Takip edilmiyorsa da başarı döner. Hedefin **silinmiş olması burada
/// engel değil**: silinmiş bir hesabı takipten çıkarabilmek gerekiyor
/// (aksi hâlde takip listesinde kalıcı bir satır sıkışırdı), bu yüzden
/// [`resolve_live_actor_id`] yerine ham id çözümü yapılıyor.
///
/// # Errors
/// Kullanıcı adı hiç yoksa [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
pub async fn unfollow(pool: &PgPool, follower_id: i64, username: &str) -> Result<()> {
    let normalized = crate::text::normalize_text(username);

    let followed_id: i64 =
        sqlx::query_scalar!(r#"SELECT id FROM actors WHERE username = $1"#, normalized)
            .fetch_optional(pool)
            .await?
            .ok_or(Error::NotFound("actor"))?;

    sqlx::query!(
        r#"DELETE FROM follows WHERE follower_actor_id = $1 AND followed_actor_id = $2"#,
        follower_id,
        followed_id,
    )
    .execute(pool)
    .await?;

    Ok(())
}

// --- Kaydetme ---------------------------------------------------------------

/// `PUT /contents/{id}/save`: idempotent kaydetme (bookmark).
///
/// **Kendi içeriğini kaydetmek serbest** — oy vermenin aksine. Kaydetme
/// kişisel bir yer imi, sıralamayı etkileyen bir sinyal değil; kendi
/// yazdığını sonra bulmak için işaretlemek meşru bir kullanım.
///
/// Yorumlar da kaydedilebilir: `saves.content_id` `contents`'e bakıyor,
/// tür ayrımı yok.
///
/// # Errors
/// İçerik yoksa [`Error::NotFound`]; silinmişse [`Error::Gone`];
/// veritabanı hatası [`Error::Database`].
pub async fn save(
    pool: &PgPool,
    actor_id: i64,
    content_id: i64,
    viewer_communities: &[i64],
) -> Result<()> {
    let deleted_at: Option<DateTime<Utc>> = sqlx::query_scalar!(
        r#"SELECT deleted_at FROM contents
           WHERE id = $1
             AND content_visible_to(contents.community_id, $2::bigint[])"#,
        content_id,
        viewer_communities,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("content"))?;

    if deleted_at.is_some() {
        return Err(Error::Gone("content"));
    }

    sqlx::query!(
        r#"
        INSERT INTO saves (actor_id, content_id)
        VALUES ($1, $2)
        ON CONFLICT (actor_id, content_id) DO NOTHING
        "#,
        actor_id,
        content_id,
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// `DELETE /contents/{id}/save`: idempotent kaydı kaldırma.
///
/// [`unfollow`] ile aynı gerekçe: silinmiş içeriğin kaydı da
/// kaldırılabilmeli, yoksa kayıt listesinde sıkışıp kalırdı. Bu yüzden
/// içeriğin canlılığı kontrol edilmiyor; hiç var olmayan bir id ise
/// `DELETE` zaten hiçbir satıra dokunmaz ve başarı döner.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn unsave(pool: &PgPool, actor_id: i64, content_id: i64) -> Result<()> {
    sqlx::query!(
        r#"DELETE FROM saves WHERE actor_id = $1 AND content_id = $2"#,
        actor_id,
        content_id,
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// `GET /me/saves`: çağıranın kaydettiği içerikler, **en son kaydedilen
/// önce**.
///
/// Sıralama içeriğin `created_at`'ine değil `saves.created_at`'ine göre:
/// kullanıcı "en son ne kaydettim" diye bakıyor, "en yeni ne yazılmış"
/// diye değil. Cursor da bu zamanı taşıyor.
///
/// **Silinmiş içerikler listeden düşürülüyor.** Kaydın kendisi duruyor
/// (bkz. [`unsave`] — kullanıcı isterse kaldırabilsin), ama silinmiş bir
/// içeriği yer imi listesinde `[deleted]` olarak göstermenin bir değeri
/// yok: `crate::comment`'teki ağaç bağlamının aksine burada o düğümün
/// taşıdığı bir çocuk yok.
///
/// Post ve yorum ayrımı yapılmıyor: `saves.content_id` `contents`'e bakıyor,
/// ikisi de kaydedilebilir.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn list_saves(
    pool: &PgPool,
    actor_id: i64,
    cursor: Option<Cursor>,
    limit: i64,
    viewer_communities: &[i64],
) -> Result<Page<Content>> {
    // Cursor'daki zaman `saves.created_at`; tür olarak yine `New`.
    let (cursor_saved_at, cursor_id) = match cursor {
        None => (None, None),
        Some(Cursor {
            sort: SortKey::New { created_at },
            id,
        }) => (Some(created_at), Some(id)),
        Some(_) => return Err(Error::InvalidCursor),
    };

    /// Kaydedilen içerik + kaydın kendi zamanı. `crate::content::ContentRow`
    /// yeniden kullanılamıyor çünkü sayfalama anahtarı içeriğin değil
    /// **kaydın** zamanı; bu yüzden yerel bir satır tipi.
    struct SavedRow {
        saved_at: DateTime<Utc>,
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

    let rows = sqlx::query_as!(
        SavedRow,
        r#"
        SELECT
            saves.created_at AS saved_at,
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
        FROM saves
        JOIN contents ON contents.id = saves.content_id
        JOIN actors ON actors.id = contents.actor_id
        LEFT JOIN communities ON communities.id = contents.community_id
        LEFT JOIN content_tags ON content_tags.content_id = contents.id
        LEFT JOIN tags ON tags.id = content_tags.tag_id
        WHERE saves.actor_id = $1
          AND contents.deleted_at IS NULL
          AND content_visible_to(contents.community_id, $5::bigint[])
          AND (
              $2::timestamptz IS NULL
              OR (saves.created_at, contents.id) < ($2::timestamptz, $3::bigint)
          )
        GROUP BY saves.created_at, contents.id, actors.id, communities.id
        ORDER BY saves.created_at DESC, contents.id DESC
        LIMIT $4
        "#,
        actor_id,
        cursor_saved_at,
        cursor_id,
        limit + 1,
        viewer_communities,
    )
    .fetch_all(pool)
    .await?;

    let mut page = paginate(
        rows,
        limit,
        |row: &SavedRow| row.id,
        |row: &SavedRow| SortKey::New {
            created_at: row.saved_at,
        },
        |row| Content {
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
        },
    );
    crate::content::resolve_cross_posts(pool, viewer_communities, &mut page.items).await?;
    Ok(page)
}
