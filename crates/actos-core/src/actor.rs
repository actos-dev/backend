//! Actor profilleri: public profil + istatistikler, kendi profilini
//! güncelleme, hesap soft-delete, takipçi/takip listeleri ve keşif dizini.
//!
//! Kimlik doğrulama/kayıt/API key yönetimi `crate::auth`'ta kalmaya devam
//! ediyor — bu modül onun üzerine "bir actor'ün DIŞA dönük görünümü ve
//! kendi profilini yönetmesi" katmanını ekliyor. Kurtarma kodu doğrulaması
//! burada YENİDEN yazılmıyor: [`delete_account`],
//! `crate::auth::verify_recovery_code_for_actor` /
//! `crate::auth::consume_recovery_code`'u kullanıyor — ikisi de
//! `crate::auth::recover`'ın kullandığı aynı zamanlama-güvenli (sabit
//! sayıda Argon2 doğrulaması yapan) iç mekanizmayı paylaşıyor.
//!
//! Sayfalama her yerde `crate::cursor` üzerinden: bu modüldeki her cursor
//! kullanımı [`crate::cursor::SortKey::New`] varyantı (`created_at DESC,
//! id DESC`) — `Top`/`Hot` varyantları `contents` feed'lerine özgü (bkz.
//! `cursor.rs` modül dokümantasyonu), burada ihtiyaç duyulmuyor. Cursor'ın
//! HMAC imzalama/doğrulama mekanizması varlık türünden bağımsız genel bir
//! yapı olduğu için bu, `cursor.rs`'i "kendi cursor'unu icat etmeden"
//! yeniden kullanmanın doğal yolu.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    auth,
    auth::{ActorRecord, ActorType},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    text,
};

// --- Sayfalama politikası ---------------------------------------------------

/// Bir `limit` query parametresi verilmediğinde kullanılan sayfa boyutu.
pub const DEFAULT_PAGE_SIZE: i64 = 25;

/// İstemcinin talep edebileceği azami sayfa boyutu. Çok büyük tek bir sayfa
/// istemek DB'yi ve yanıtı gereksiz şişirir; burada sert bir tavan var.
pub const MAX_PAGE_SIZE: i64 = 100;

/// Ham `limit` query parametresini `[1, MAX_PAGE_SIZE]` aralığına
/// sıkıştırır; hiç verilmemişse [`DEFAULT_PAGE_SIZE`] kullanılır.
///
/// Bilerek hata döndürmüyor: `limit=999999` gönderen bir istemciyi
/// reddetmek yerine sessizce tavana sıkıştırmak, özellikle "tek istekte
/// her şeyi almayı" deneyen ajan istemciler için daha iyi bir davranış.
#[must_use]
pub fn clamp_page_size(raw: Option<i64>) -> i64 {
    raw.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE)
}

// --- Profil ------------------------------------------------------------

/// `GET /actors/{username}` için public profil + içerik istatistikleri.
#[derive(Debug, Clone)]
pub struct Profile {
    pub actor: ActorRecord,
    pub post_count: i64,
    pub comment_count: i64,
    pub total_score: i64,
}

struct ProfileRow {
    id: i64,
    username: String,
    actor_type: ActorType,
    display_name: Option<String>,
    bio: Option<String>,
    created_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
    post_count: i64,
    comment_count: i64,
    total_score: i64,
}

/// Bir actor'ün public profilini ve içerik istatistiklerini **tek
/// sorguda** döner (`contents` üzerinde `LEFT JOIN` + agrega) — actor
/// başına ayrı bir istatistik sorgusu atmıyoruz.
///
/// İstatistikler yalnızca canlı içerikten (`contents.deleted_at IS NULL`)
/// hesaplanır: silinmiş bir post/yorum artık actor'ün "üretkenliğinin"
/// parçası olarak gösterilmemeli.
///
/// **Silinmiş actor `404` değil `410` döner:** `username` soft-delete'te
/// serbest bırakılmıyor (impersonation riskine karşı, bkz.
/// `migrations/0002_actors.up.sql` üzerindeki COMMENT) — yani bu kullanıcı
/// adı hâlâ "var", sadece hesabı silinmiş. `NotFound` ("böyle bir kullanıcı
/// hiç olmadı") burada yanıltıcı olurdu; `Gone` ("vardı, silindi") anlamı
/// doğru taşıyan ve sistemde zaten tanımlı olan kod (`ErrorCode::Gone`,
/// bkz. `actos-types/src/error.rs` — `PLAN.md` Faz 8'in "silinmiş post
/// 410" kuralıyla da tutarlı).
///
/// # Errors
/// Kullanıcı adı hiç yoksa [`Error::NotFound`]; actor soft-delete
/// edilmişse [`Error::Gone`]; veritabanı hatası [`Error::Database`].
pub async fn get_profile(pool: &PgPool, username: &str) -> Result<Profile> {
    let normalized = text::normalize_text(username);

    let row = sqlx::query_as!(
        ProfileRow,
        r#"
        SELECT
            actors.id,
            actors.username,
            actors.actor_type AS "actor_type: ActorType",
            actors.display_name,
            actors.bio,
            actors.created_at,
            actors.deleted_at,
            COUNT(contents.id) FILTER (
                WHERE contents.content_type = 'post' AND contents.deleted_at IS NULL
            ) AS "post_count!",
            COUNT(contents.id) FILTER (
                WHERE contents.content_type = 'comment' AND contents.deleted_at IS NULL
            ) AS "comment_count!",
            COALESCE(
                SUM(contents.score) FILTER (WHERE contents.deleted_at IS NULL),
                0
            )::bigint AS "total_score!"
        FROM actors
        LEFT JOIN contents ON contents.actor_id = actors.id
        WHERE actors.username = $1
        GROUP BY actors.id
        "#,
        normalized.as_str(),
    )
    .fetch_optional(pool)
    .await?;

    let row = row.ok_or(Error::NotFound("actor"))?;

    if row.deleted_at.is_some() {
        return Err(Error::Gone("actor"));
    }

    Ok(Profile {
        actor: ActorRecord {
            id: row.id,
            username: row.username,
            actor_type: row.actor_type,
            display_name: row.display_name,
            bio: row.bio,
            created_at: row.created_at,
        },
        post_count: row.post_count,
        comment_count: row.comment_count,
        total_score: row.total_score,
    })
}

// --- Profil güncelleme ---------------------------------------------------

/// `PATCH /actors/me`: `display_name`/`bio`'yu kısmi günceller.
///
/// Her iki parametre de `Option<Option<String>>`: dış `None` "bu alana
/// dokunma", `Some(None)` "temizle (NULL yap)", `Some(Some(v))` "`v`'ye
/// güncelle" anlamına gelir — HTTP katmanı JSON'daki alan var/yok ayrımını
/// buraya kadar aynen taşıyor (bkz.
/// `actos_types::actor::UpdateProfileRequest` üzerindeki yorum).
///
/// **Tek statik SQL, dinamik `UPDATE` string birleştirmesi yok:** `CASE
/// WHEN $touch THEN $value ELSE mevcut_kolon END` kalıbı, sqlx'in derleme
/// zamanı doğruladığı sabit bir sorguyla "dokunma/temizle/güncelle" üçlü
/// mantığını ifade ediyor.
///
/// `actors.updated_at`'e burada elle dokunulmuyor:
/// `migrations/0017_triggers.up.sql`'deki `trg_actors_set_updated_at`
/// zaten her `UPDATE`'te otomatik günceller.
///
/// `actor_id` her zaman [`crate::auth::authenticate`]'ten geçmiş, canlı bir
/// actor'e ait olduğu için `WHERE ... AND deleted_at IS NULL` eşleşmemesi
/// pratikte imkânsız — yine de olursa bunu çağıranın değil sunucunun bir
/// tutarsızlığı sayıp [`Error::Internal`] dönüyoruz (bkz. `routes/auth.rs`
/// `whoami`'deki aynı gerekçe).
///
/// # Errors
/// `display_name`/`bio` doğrulamadan geçmezse [`Error::Validation`];
/// yukarıdaki tutarsızlık durumunda [`Error::Internal`]; veritabanı hatası
/// [`Error::Database`].
pub async fn update_profile(
    pool: &PgPool,
    actor_id: i64,
    display_name: Option<Option<String>>,
    bio: Option<Option<String>>,
) -> Result<ActorRecord> {
    let display_name = validate_optional_update(display_name, text::validate_display_name)?;
    let bio = validate_optional_update(bio, text::validate_bio)?;

    let touch_display_name = display_name.is_some();
    let new_display_name = display_name.flatten();
    let touch_bio = bio.is_some();
    let new_bio = bio.flatten();

    let actor = sqlx::query_as!(
        ActorRecord,
        r#"
        UPDATE actors
        SET
            display_name = CASE WHEN $2 THEN $3 ELSE display_name END,
            bio = CASE WHEN $4 THEN $5 ELSE bio END
        WHERE id = $1 AND deleted_at IS NULL
        RETURNING id, username, actor_type AS "actor_type: ActorType", display_name, bio, created_at
        "#,
        actor_id,
        touch_display_name,
        new_display_name,
        touch_bio,
        new_bio,
    )
    .fetch_optional(pool)
    .await?;

    actor.ok_or_else(|| {
        Error::Internal(format!(
            "profil güncellenemedi: actor {actor_id} bulunamadı (authenticate() sonrası olmamalı)"
        ))
    })
}

/// [`update_profile`]'ın "dokunma/temizle/güncelle" alanlarından biri için
/// ortak doğrulama adımı: doğrulayıcıyı yalnızca gerçekten yeni bir değer
/// geldiğinde (`Some(Some(raw))`) çalıştırır — `None`/`Some(None)` zaten
/// geçerli, doğrulanacak bir metin taşımıyor.
fn validate_optional_update<F>(
    field: Option<Option<String>>,
    validate: F,
) -> Result<Option<Option<String>>>
where
    F: FnOnce(&str) -> std::result::Result<String, text::TextError>,
{
    match field {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(raw)) => {
            let validated = validate(&raw).map_err(|e| Error::Validation(e.to_string()))?;
            Ok(Some(Some(validated)))
        }
    }
}

// --- Hesap silme -----------------------------------------------------------

/// `DELETE /actors/me`: hesabı soft-delete eder.
///
/// Üç adım **tek transaction'da**: (1) sunulan kurtarma kodu tüketilir —
/// yanlış kod hiçbir yan etki bırakmadan [`Error::InvalidKey`] döner; (2)
/// `actors.deleted_at` işaretlenir; (3) actor'ün **tüm** aktif API
/// key'leri iptal edilir. Üçü birlikte commit olmazsa hiçbiri kalıcı
/// olmaz — "hesap silindi ama key hâlâ çalışıyor" gibi bir ara durum
/// oluşamaz.
///
/// Kod doğrulaması (Argon2, pahalı) bilerek transaction **dışında**
/// yapılıyor — bkz. `crate::auth::verify_recovery_code_for_actor` üzerindeki
/// yorum (`crate::auth::recover`'daki aynı desen): bir veritabanı
/// transaction'ını CPU-ağır bir işlem boyunca açık tutmak istemiyoruz.
///
/// Username **serbest bırakılmaz** — `actors` satırı silinmez, yalnızca
/// işaretlenir (bkz. `migrations/0002_actors.up.sql`). İçeriklerin
/// `[silindi]` görünmesi bu fonksiyonun işi değil (okuma tarafı, Faz 8).
///
/// # Errors
/// Kod yanlışsa/tükenmişse [`Error::InvalidKey`]; veritabanı hatası
/// [`Error::Database`].
pub async fn delete_account(pool: &PgPool, actor_id: i64, recovery_code: &str) -> Result<()> {
    let code_row_id = auth::verify_recovery_code_for_actor(pool, actor_id, recovery_code).await?;

    let mut tx = pool.begin().await?;

    auth::consume_recovery_code(&mut tx, code_row_id).await?;

    sqlx::query!(
        r#"UPDATE actors SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL"#,
        actor_id,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"UPDATE api_keys SET revoked_at = now() WHERE actor_id = $1 AND revoked_at IS NULL"#,
        actor_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

// --- Sayfalı sonuçlar ortak yapısı -----------------------------------------

/// Sayfalı bir liste + varsa sonraki sayfanın cursor'ı (henüz encode
/// edilmemiş — encode/decode HTTP katmanının işi, bkz.
/// `crate::cursor::CursorCodec` ve `actos-api/src/routes/actors.rs`).
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<Cursor>,
}

/// `rows` (`limit + 1` çekilmiş ham satırlar) üzerinden bir [`Page`] kurar.
///
/// **`limit + 1` deseni:** "sonraki sayfa var mı" sorusunu ayrı bir
/// `COUNT(*)` sorgusuna gerek kalmadan yanıtlar — fazladan çekilen tek satır
/// varsa bir sonraki sayfa vardır, o satır yanıta dahil edilmeden atılır ve
/// cursor, kalan son (yani `limit`'inci) satırdan türetilir.
fn paginate<Row, T>(
    mut rows: Vec<Row>,
    limit: i64,
    row_id: impl Fn(&Row) -> i64,
    row_sort_value: impl Fn(&Row) -> DateTime<Utc>,
    into_item: impl Fn(Row) -> T,
) -> Page<T> {
    let has_more = i64::try_from(rows.len()).unwrap_or(i64::MAX) > limit;
    if has_more {
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }

    let next_cursor = if has_more {
        rows.last().map(|row| Cursor {
            sort: SortKey::New {
                created_at: row_sort_value(row),
            },
            id: row_id(row),
        })
    } else {
        None
    };

    Page {
        items: rows.into_iter().map(into_item).collect(),
        next_cursor,
    }
}

/// Bir [`Cursor`]'ı `(created_at, id)` bindable çiftine ayırır. Bu
/// modüldeki her cursor `SortKey::New` varyantı (bkz. modül başındaki
/// gerekçe); `None` ise her iki değer de `None` döner, ki SQL tarafındaki
/// `$n::timestamptz IS NULL` koşulu ilk sayfayı (filtre yok) üretsin.
///
/// `Some` ama `New` dışında bir varyant burada asla oluşmaz: HTTP katmanı
/// cursor'ı her zaman `SortKind::New` beklentisiyle çözüyor
/// (`CursorCodec::decode`), başka bir varyant zaten orada
/// `CursorError::SortMismatch` ile reddedilir. Yine de panik yerine
/// savunmacı bir `None` dönüşü tercih edildi.
fn split_new_cursor(cursor: Option<Cursor>) -> (Option<DateTime<Utc>>, Option<i64>) {
    match cursor {
        Some(Cursor {
            sort: SortKey::New { created_at },
            id,
        }) => (Some(created_at), Some(id)),
        Some(_) | None => (None, None),
    }
}

/// `username`'in var olup olmadığını ve canlı olduğunu doğrular, iç
/// `actor_id`'sini döner.
///
/// [`get_profile`], [`list_followers`] ve [`list_following`] aynı kuralı
/// paylaşıyor: silinmiş bir hesabın listelerine bakmak da profiline bakmak
/// gibi `410 Gone` döner (bkz. [`get_profile`] üzerindeki gerekçe — aynı
/// username hâlâ "var", sadece hesap silinmiş).
///
/// # Errors
/// Kullanıcı adı hiç yoksa [`Error::NotFound`]; actor silinmişse
/// [`Error::Gone`]; veritabanı hatası [`Error::Database`].
async fn resolve_live_actor_id(pool: &PgPool, username: &str) -> Result<i64> {
    let normalized = text::normalize_text(username);

    let row = sqlx::query!(
        r#"SELECT id, deleted_at FROM actors WHERE username = $1"#,
        normalized.as_str(),
    )
    .fetch_optional(pool)
    .await?;

    let row = row.ok_or(Error::NotFound("actor"))?;

    if row.deleted_at.is_some() {
        return Err(Error::Gone("actor"));
    }

    Ok(row.id)
}

// --- Takipçi / takip listeleri ----------------------------------------------

/// Bir takip listesi (followers ya da following) sayfasındaki tek satır:
/// karşı taraftaki actor + takip ilişkisinin kurulduğu an.
///
/// `followed_at`, `follows.created_at`'tir — cursor'ın sıralama değeri
/// budur, actor'ün kendi `created_at`'i (hesap açılış tarihi) DEĞİL.
#[derive(Debug, Clone)]
pub struct FollowEntry {
    pub actor: ActorRecord,
    pub followed_at: DateTime<Utc>,
}

struct FollowRow {
    id: i64,
    username: String,
    actor_type: ActorType,
    display_name: Option<String>,
    bio: Option<String>,
    created_at: DateTime<Utc>,
    followed_at: DateTime<Utc>,
}

impl FollowRow {
    fn into_entry(self) -> FollowEntry {
        FollowEntry {
            actor: ActorRecord {
                id: self.id,
                username: self.username,
                actor_type: self.actor_type,
                display_name: self.display_name,
                bio: self.bio,
                created_at: self.created_at,
            },
            followed_at: self.followed_at,
        }
    }
}

/// `GET /actors/{username}/followers`: `username`'i takip edenler, en yeni
/// takip ilişkisi önce.
///
/// Yalnızca canlı actor'ler listelenir (`actors.deleted_at IS NULL` —
/// takip eden taraf için): silinmiş bir hesap takipçi listesinde
/// görünmemeli. `follows` satırının kendisi hedef actor silinmedikçe
/// (bkz. [`resolve_live_actor_id`]) dokunulmadan kalır.
///
/// # Errors
/// [`resolve_live_actor_id`] ile aynı; veritabanı hatası [`Error::Database`].
pub async fn list_followers(
    pool: &PgPool,
    username: &str,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<FollowEntry>> {
    let target_id = resolve_live_actor_id(pool, username).await?;
    let (cursor_created_at, cursor_id) = split_new_cursor(cursor);

    let rows = sqlx::query_as!(
        FollowRow,
        r#"
        SELECT
            actors.id,
            actors.username,
            actors.actor_type AS "actor_type: ActorType",
            actors.display_name,
            actors.bio,
            actors.created_at,
            follows.created_at AS followed_at
        FROM follows
        JOIN actors ON actors.id = follows.follower_actor_id
        WHERE follows.followed_actor_id = $1
          AND actors.deleted_at IS NULL
          AND (
              $2::timestamptz IS NULL
              OR (follows.created_at, follows.follower_actor_id) < ($2::timestamptz, $3::bigint)
          )
        ORDER BY follows.created_at DESC, follows.follower_actor_id DESC
        LIMIT $4
        "#,
        target_id,
        cursor_created_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row| row.id,
        |row| row.followed_at,
        FollowRow::into_entry,
    ))
}

/// `GET /actors/{username}/following`: `username`'in takip ettikleri, en
/// yeni takip ilişkisi önce.
///
/// # Errors
/// [`list_followers`] ile aynı.
pub async fn list_following(
    pool: &PgPool,
    username: &str,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<FollowEntry>> {
    let source_id = resolve_live_actor_id(pool, username).await?;
    let (cursor_created_at, cursor_id) = split_new_cursor(cursor);

    let rows = sqlx::query_as!(
        FollowRow,
        r#"
        SELECT
            actors.id,
            actors.username,
            actors.actor_type AS "actor_type: ActorType",
            actors.display_name,
            actors.bio,
            actors.created_at,
            follows.created_at AS followed_at
        FROM follows
        JOIN actors ON actors.id = follows.followed_actor_id
        WHERE follows.follower_actor_id = $1
          AND actors.deleted_at IS NULL
          AND (
              $2::timestamptz IS NULL
              OR (follows.created_at, follows.followed_actor_id) < ($2::timestamptz, $3::bigint)
          )
        ORDER BY follows.created_at DESC, follows.followed_actor_id DESC
        LIMIT $4
        "#,
        source_id,
        cursor_created_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row| row.id,
        |row| row.followed_at,
        FollowRow::into_entry,
    ))
}

// --- Keşif dizini ------------------------------------------------------

struct DirectoryRow {
    id: i64,
    username: String,
    actor_type: ActorType,
    display_name: Option<String>,
    bio: Option<String>,
    created_at: DateTime<Utc>,
}

/// `GET /actors?type=...&sort=new`: keşif dizini, en yeni kayıt önce.
///
/// Yalnızca canlı (`deleted_at IS NULL`) actor'ler listelenir — silinmiş
/// bir hesap "keşfedilebilir" olmamalı ([`get_profile`]'ın `Gone` ile
/// ulaşılabilir kalması ayrı bir şey; dizin aktif olarak öne çıkarmaktır).
///
/// `sort`: şu an yalnızca `new` (`actors.created_at DESC, id DESC`)
/// destekleniyor — bu yüzden burada ayrı bir `sort` parametresi yok, HTTP
/// katmanı zaten başka bir değeri buraya hiç geçirmiyor (bkz.
/// `actos-api/src/routes/actors.rs`). Gelecekte yeni bir sıralama
/// eklenirse (ör. `top`) burada da bir parametre olarak eklenmesi gerekir.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn list_directory(
    pool: &PgPool,
    actor_type: Option<ActorType>,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<ActorRecord>> {
    let (cursor_created_at, cursor_id) = split_new_cursor(cursor);

    let rows = sqlx::query_as!(
        DirectoryRow,
        r#"
        SELECT id, username, actor_type AS "actor_type: ActorType", display_name, bio, created_at
        FROM actors
        WHERE deleted_at IS NULL
          AND ($1::actor_type IS NULL OR actor_type = $1)
          AND (
              $2::timestamptz IS NULL
              OR (created_at, id) < ($2::timestamptz, $3::bigint)
          )
        ORDER BY created_at DESC, id DESC
        LIMIT $4
        "#,
        actor_type as Option<ActorType>,
        cursor_created_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row| row.id,
        |row| row.created_at,
        |row| ActorRecord {
            id: row.id,
            username: row.username,
            actor_type: row.actor_type,
            display_name: row.display_name,
            bio: row.bio,
            created_at: row.created_at,
        },
    ))
}
