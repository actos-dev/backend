//! Topluluklar: dizin, üyelik ve topluluk akışı (COMMUNITY_PLAN.md §1-4, §10-11).
//!
//! Bir topluluk; bir sahibi, üyeleri ve açıklaması olan bir kaptır. **Etiket
//! değildir** (§1): etiketler serbest, çoklu ve sahipsiz kalır; bir post
//! ikisini de taşıyabilir ya da hiçbirini. Bir post için topluluk
//! **isteğe bağlıdır** — topluluksuz bir post "daha aşağı" bir tür değil,
//! yalnızca bağımsız bir posttur (`contents.community_id IS NULL`).
//!
//! HTTP'yi bilmez; `actos-api/src/routes/communities.rs` bu modülün
//! fonksiyonlarını çağırıp HTTP'ye çevirir (bkz. `crate::actor`/
//! `crate::content` modüllerindeki aynı katman ayrımı).
//!
//! ## Faz 2: yalnızca public
//!
//! `visibility` sütunu ve [`CommunityVisibility`] şimdiden var (şema son
//! şeklini alsın diye), ama [`create_community`] `private` değerini
//! reddediyor: Faz 4'ün görünürlük kapısı tüm okuma yollarına yayılmadan
//! özel toplulukların var olması bir sızıntı olurdu. Geri kalan kod bu
//! yüzden "her topluluk public" varsayabilir.
//!
//! ## Sayfalama
//!
//! Dizin ve üye listeleri `crate::cursor`'ın `New` sıralamasını kullanıyor
//! (`actor::list_directory` ile aynı desen). Üye listesinin sırası
//! **artan** (`joined_at ASC`) — en uzun süredir üye olan önce gelir, çünkü
//! ileride devralma (succession) en kıdemli moderatöre geçecek (§4). Cursor
//! mekanizması yön bilmez (`SortKey::New` yalnızca `created_at` taşır);
//! SQL'deki karşılaştırma operatörü (`>` yerine `<`) bu yüzden bu modülde
//! açıkça yazılıyor.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres};

use crate::{
    actor::{Page, paginate, split_new_cursor},
    auth::{ActorRecord, ActorType, Grant, Permission},
    content::{BodyFormat, Content, ContentType, PostSort},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    text,
};

/// Bir aktörün sahip olabileceği azami topluluk sayısı (COMMUNITY_PLAN.md §4).
///
/// Küçük başlıyor: yükseltmesi kolay, düşürmesi zor. Sahiplik sayımı
/// [`create_community`]'nin transaction'ı içinde, sahibin `actors` satırı
/// kilitlenerek yapılıyor — iki eşzamanlı istek sınırı aşamasın diye.
pub const MAX_COMMUNITIES_PER_OWNER: i64 = 3;

/// `migrations/0029_communities.up.sql` → `ck_communities_description_length`
/// üst sınırı. Alt sınır `1` (boş açıklama yok).
const DESCRIPTION_MAX: usize = 10_000;

// --- Domain tipleri ----------------------------------------------------

/// `migrations/0029_communities.up.sql` → `community_visibility` Postgres
/// enum'ının Rust karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "community_visibility", rename_all = "snake_case")]
pub enum CommunityVisibility {
    Public,
    Private,
}

impl CommunityVisibility {
    /// API'de görünen dize.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

/// Bir topluluğa yapılan referans — içerik DTO'ları için (`Content.community`).
///
/// Tam [`Community`] yerine dar bir tip: bir post'un yanıtında topluluğun
/// üye sayısı/açıklaması değil yalnızca kimliği ve adı gerekir; her içerik
/// satırında tüm topluluk satırını taşımak gereksiz olurdu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityRef {
    pub id: i64,
    pub name: String,
}

/// `communities` tablosundan (sahibi ve sayaçlarıyla) okunan bir satır.
///
/// `owner_deleted` ayrı bir alan, `owner: Option<ActorRecord>` değil —
/// `crate::content::Content::author_deleted` ile birebir aynı gerekçe: sahip
/// satırı soft-delete'te silinmiyor, yalnızca gösterilip gösterilmeyeceği
/// değişiyor; maskeleme kararı HTTP katmanında veriliyor.
#[derive(Debug, Clone)]
pub struct Community {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub visibility: CommunityVisibility,
    pub owner: ActorRecord,
    pub owner_deleted: bool,
    /// `community_members` satır sayısı.
    pub member_count: i64,
    /// Canlı (`deleted_at IS NULL`) post sayısı.
    pub post_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

struct CommunityRow {
    id: i64,
    name: String,
    description: String,
    visibility: CommunityVisibility,
    owner_id: i64,
    owner_username: String,
    owner_actor_type: ActorType,
    owner_display_name: Option<String>,
    owner_bio: Option<String>,
    owner_created_at: DateTime<Utc>,
    owner_deleted_at: Option<DateTime<Utc>>,
    member_count: i64,
    post_count: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<CommunityRow> for Community {
    fn from(row: CommunityRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            description: row.description,
            visibility: row.visibility,
            owner: ActorRecord {
                id: row.owner_id,
                username: row.owner_username,
                actor_type: row.owner_actor_type,
                display_name: row.owner_display_name,
                bio: row.owner_bio,
                created_at: row.owner_created_at,
            },
            owner_deleted: row.owner_deleted_at.is_some(),
            member_count: row.member_count,
            post_count: row.post_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Bir topluluğun sahibi + görünürlüğü. Kimlik doğrulaması gerektiren
/// yollarda (güncelleme, katılma, ayrılma, üye listesi, topluluk akışı)
/// isimden iç kimliğe geçmenin tek noktası.
pub(crate) struct CommunityLookup {
    pub id: i64,
    pub owner_actor_id: i64,
    pub visibility: CommunityVisibility,
}

// [`Community`] satırlarının ortak `SELECT` gövdesi. `sqlx::query_as!`
// sorgunun string **literal** olmasını şart koştuğu için bu bir
// `const &str` olarak paylaşılamıyor; iki çağrı (tekil okuma + dizin) bu
// yüzden sütun listesini kendi metinlerinde tekrar ediyor (bkz.
// `crate::content`'teki aynı desen — sorgu metni bir sabite çekilemiyor).
//
// Sayaçlar korele alt sorgular: `LEFT JOIN`+`GROUP BY` yerine bunu seçtik
// çünkü iki farklı tabloyu (`community_members`, `contents`) aynı `GROUP
// BY`'da saymak Kartezyen çarpım üretirdi.

// --- Girdi doğrulama ---------------------------------------------------

/// Topluluk açıklamasını doğrular: [`text::normalize_text`] uygular, boş
/// olamama ve uzunluk kuralını işletir.
///
/// Açıklama markdown'dır (§11) ama sunucu tarafında render edilmez; yalnızca
/// metin olarak saklanır ve uzunluğu denetlenir — ölçü şemadaki
/// `ck_communities_description_length` ile birebir.
///
/// # Errors
/// Normalize edildikten sonra boşsa ya da `10000` karakteri aşarsa
/// [`Error::Validation`].
fn validate_description(raw: &str) -> Result<String> {
    let normalized = text::normalize_text(raw);
    let len = normalized.chars().count();

    if len == 0 {
        return Err(Error::Validation(
            "community description cannot be empty".to_owned(),
        ));
    }
    if len > DESCRIPTION_MAX {
        return Err(Error::Validation(format!(
            "community description can be at most {DESCRIPTION_MAX} characters (received: {len} characters)"
        )));
    }

    Ok(normalized)
}

// --- İç yardımcılar (transaction içinden çağrılabilir) -----------------

/// İsimden topluluk kimliğini çözer; yoksa [`Error::NotFound`].
///
/// `E: Executor` olması bilinçli: [`create_community`] bunu bir `&mut
/// PgConnection` (transaction) üzerinden çağırırken liste/güncelleme yolları
/// `&PgPool` geçiriyor — aynı sorgunun iki kopyası olmasın diye.
///
/// # Errors
/// Böyle bir topluluk yoksa [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
pub(crate) async fn resolve_community_id_in<'e, E>(executor: E, name: &str) -> Result<i64>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let normalized = text::normalize_text(name);
    sqlx::query_scalar!(
        r#"SELECT id FROM communities WHERE name = $1"#,
        normalized.as_str()
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("community"))
}

/// [`resolve_community_id_in`]'e ek olarak sahibi ve görünürlüğü de getirir.
///
/// # Errors
/// Böyle bir topluluk yoksa [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
pub(crate) async fn lookup_community_in<'e, E>(executor: E, name: &str) -> Result<CommunityLookup>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let normalized = text::normalize_text(name);
    let row = sqlx::query!(
        r#"
        SELECT id, owner_actor_id, visibility AS "visibility: CommunityVisibility"
        FROM communities
        WHERE name = $1
        "#,
        normalized.as_str(),
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("community"))?;

    Ok(CommunityLookup {
        id: row.id,
        owner_actor_id: row.owner_actor_id,
        visibility: row.visibility,
    })
}

/// [`lookup_community_in`]'in havuz üzerinden çalışan hâli.
async fn lookup_community(pool: &PgPool, name: &str) -> Result<CommunityLookup> {
    lookup_community_in(pool, name).await
}

/// Bir aktörün verilen toplulukta üye olup olmadığı.
///
/// `E: Executor`: bkz. [`resolve_community_id_in`] üzerindeki gerekçe —
/// [`crate::content::create_post`] bunu transaction içinden çağırıyor.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub(crate) async fn is_member_in<'e, E>(
    executor: E,
    community_id: i64,
    actor_id: i64,
) -> Result<bool>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM community_members
            WHERE community_id = $1 AND actor_id = $2
        ) AS "exists!"
        "#,
        community_id,
        actor_id,
    )
    .fetch_one(executor)
    .await?;

    Ok(exists)
}

/// [`is_member_in`]'in havuz üzerinden çalışan hâli.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn is_member(pool: &PgPool, community_id: i64, actor_id: i64) -> Result<bool> {
    is_member_in(pool, community_id, actor_id).await
}

// --- Yazma yolları -----------------------------------------------------

/// Yeni bir topluluk oluşturur: topluluk satırı + sahibin üyeliği, tek
/// transaction'da.
///
/// Sahiplik sayımı yarış durumuna karşı **sahibin `actors` satırı
/// kilitlenerek** yapılıyor (`FOR UPDATE`): iki eşzamanlı istek aksi hâlde
/// ikisi de "2 topluluğum var" okuyup sınırı aşabilirdi. Sınır aşılırsa
/// [`Error::Validation`].
///
/// **Faz 2 `visibility = Private` değerini reddeder** — gerekçe modül
/// dokümantasyonunda.
///
/// # Errors
/// İsim [`text::validate_community_name`]'den, açıklama
/// [`validate_description`]'dan geçmezse ya da sahiplik sınırı aşılırsa
/// [`Error::Validation`]; isim alınmışsa [`Error::Conflict`]; veritabanı
/// hatası [`Error::Database`].
pub async fn create_community(
    pool: &PgPool,
    owner_id: i64,
    name: &str,
    description: &str,
    visibility: CommunityVisibility,
) -> Result<Community> {
    let name = text::validate_community_name(name).map_err(|e| Error::Validation(e.to_string()))?;
    let description = validate_description(description)?;

    if visibility != CommunityVisibility::Public {
        return Err(Error::Validation(
            "private communities are not available yet".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;

    // Sahip satırını kilitle: sahiplik sayımı ile INSERT arasında başka bir
    // yaratma isteği araya giremesin.
    let owner_locked = sqlx::query!(
        r#"SELECT id FROM actors WHERE id = $1 AND deleted_at IS NULL FOR UPDATE"#,
        owner_id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    if owner_locked.is_none() {
        return Err(Error::Internal(format!(
            "could not create community: actor {owner_id} not found (should not happen after authenticate())"
        )));
    }

    let owned = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM communities WHERE owner_actor_id = $1"#,
        owner_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    if owned >= MAX_COMMUNITIES_PER_OWNER {
        return Err(Error::Validation(format!(
            "an actor may own at most {MAX_COMMUNITIES_PER_OWNER} communities"
        )));
    }

    let inserted = sqlx::query!(
        r#"
        INSERT INTO communities (name, description, visibility, owner_actor_id)
        VALUES ($1, $2, $3, $4)
        RETURNING id
        "#,
        name.as_str(),
        description,
        visibility as CommunityVisibility,
        owner_id,
    )
    .fetch_one(&mut *tx)
    .await;

    let community_id = match inserted {
        Ok(row) => row.id,
        // `communities.name` üzerindeki UNIQUE kısıtı (citext). Gerekçe
        // `crate::auth::register`'daki aynı yakalama.
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            return Err(Error::Conflict(format!(
                "community name \"{name}\" is already taken"
            )));
        }
        Err(e) => return Err(Error::from(e)),
    };

    // Sahip her zaman üyedir: ayrı bir "sahip mi" kontrolüne gerek kalsın
    // istemiyoruz, üyelik tablosu tek doğruluk kaynağı.
    sqlx::query!(
        r#"INSERT INTO community_members (community_id, actor_id) VALUES ($1, $2)"#,
        community_id,
        owner_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    get_community(pool, &name).await
}

/// `PATCH /communities/{name}`: yalnızca sahibi **veya** global
/// `community.edit` sahibi; açıklamayı günceller.
///
/// # Errors
/// Topluluk yoksa [`Error::NotFound`]; çağıran ne sahibi ne
/// `community.edit` sahibiyse [`Error::Forbidden`]; açıklama doğrulamadan
/// geçmezse [`Error::Validation`]; veritabanı hatası [`Error::Database`].
pub async fn update_community(
    pool: &PgPool,
    actor_id: i64,
    permissions: &[Grant],
    name: &str,
    new_description: &str,
) -> Result<Community> {
    let lookup = lookup_community(pool, name).await?;

    let is_owner = lookup.owner_actor_id == actor_id;
    let can_edit = crate::authz::has_global(permissions, Permission::CommunityEdit);

    if !is_owner && !can_edit {
        return Err(Error::Forbidden);
    }

    let description = validate_description(new_description)?;

    sqlx::query!(
        r#"UPDATE communities SET description = $2, updated_at = now() WHERE id = $1"#,
        lookup.id,
        description,
    )
    .execute(pool)
    .await?;

    get_community(pool, name).await
}

/// `POST /communities/{name}/join`: public topluluğa anında üyelik.
///
/// **İdempotent:** zaten üye olmak hata değil (`ON CONFLICT DO NOTHING`).
///
/// # Errors
/// Topluluk yoksa [`Error::NotFound`]; topluluk private ise (Faz 2'de
/// oluşturulamaz ama şema izin veriyor) [`Error::Validation`]; veritabanı
/// hatası [`Error::Database`].
pub async fn join_community(pool: &PgPool, actor_id: i64, name: &str) -> Result<()> {
    let lookup = lookup_community(pool, name).await?;

    if lookup.visibility != CommunityVisibility::Public {
        return Err(Error::Validation(
            "private communities are not available yet".to_owned(),
        ));
    }

    sqlx::query!(
        r#"
        INSERT INTO community_members (community_id, actor_id)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        lookup.id,
        actor_id,
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// `DELETE /communities/{name}/join`: üyelikten ayrılma.
///
/// **Sahip ayrılamaz** ([`Error::Validation`]): devralma kuralları Faz 4'ün
/// işi, o gelene kadar sahipsiz topluluk oluşmasın diye kapı burada.
/// **İdempotent:** üye olmayanın ayrılma isteği hata değil.
///
/// # Errors
/// Topluluk yoksa [`Error::NotFound`]; çağıran sahibiyse
/// [`Error::Validation`]; veritabanı hatası [`Error::Database`].
pub async fn leave_community(pool: &PgPool, actor_id: i64, name: &str) -> Result<()> {
    let lookup = lookup_community(pool, name).await?;

    if lookup.owner_actor_id == actor_id {
        return Err(Error::Validation(
            "the owner cannot leave their community".to_owned(),
        ));
    }

    sqlx::query!(
        r#"DELETE FROM community_members WHERE community_id = $1 AND actor_id = $2"#,
        lookup.id,
        actor_id,
    )
    .execute(pool)
    .await?;

    Ok(())
}

// --- Okuma yolları -----------------------------------------------------

/// `GET /communities/{name}`: tek topluluk + sahibi + sayaçlar.
///
/// # Errors
/// Topluluk yoksa [`Error::NotFound`]; veritabanı hatası [`Error::Database`].
pub async fn get_community(pool: &PgPool, name: &str) -> Result<Community> {
    let normalized = text::normalize_text(name);

    let row = sqlx::query_as!(
        CommunityRow,
        r#"
        SELECT
            communities.id,
            communities.name,
            communities.description,
            communities.visibility AS "visibility: CommunityVisibility",
            communities.created_at,
            communities.updated_at,
            actors.id AS owner_id,
            actors.username AS owner_username,
            actors.actor_type AS "owner_actor_type: ActorType",
            actors.display_name AS owner_display_name,
            actors.bio AS owner_bio,
            actors.created_at AS owner_created_at,
            actors.deleted_at AS owner_deleted_at,
            (
                SELECT count(*) FROM community_members
                WHERE community_members.community_id = communities.id
            ) AS "member_count!",
            (
                SELECT count(*) FROM contents
                WHERE contents.community_id = communities.id
                  AND contents.content_type = 'post'::content_type
                  AND contents.deleted_at IS NULL
            ) AS "post_count!"
        FROM communities
        JOIN actors ON actors.id = communities.owner_actor_id
        WHERE communities.name = $1
        "#,
        normalized.as_str(),
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("community"))?;

    Ok(row.into())
}

/// `GET /communities?cursor=&limit=`: keşif dizini — yalnızca public
/// topluluklar, en yeni önce.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn list_directory(
    pool: &PgPool,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Community>> {
    let (cursor_created_at, cursor_id) = split_new_cursor(cursor);

    let rows = sqlx::query_as!(
        CommunityRow,
        r#"
        SELECT
            communities.id,
            communities.name,
            communities.description,
            communities.visibility AS "visibility: CommunityVisibility",
            communities.created_at,
            communities.updated_at,
            actors.id AS owner_id,
            actors.username AS owner_username,
            actors.actor_type AS "owner_actor_type: ActorType",
            actors.display_name AS owner_display_name,
            actors.bio AS owner_bio,
            actors.created_at AS owner_created_at,
            actors.deleted_at AS owner_deleted_at,
            (
                SELECT count(*) FROM community_members
                WHERE community_members.community_id = communities.id
            ) AS "member_count!",
            (
                SELECT count(*) FROM contents
                WHERE contents.community_id = communities.id
                  AND contents.content_type = 'post'::content_type
                  AND contents.deleted_at IS NULL
            ) AS "post_count!"
        FROM communities
        JOIN actors ON actors.id = communities.owner_actor_id
        WHERE communities.visibility = 'public'::community_visibility
          AND (
              $1::timestamptz IS NULL
              OR (communities.created_at, communities.id) < ($1::timestamptz, $2::bigint)
          )
        ORDER BY communities.created_at DESC, communities.id DESC
        LIMIT $3
        "#,
        cursor_created_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row: &CommunityRow| row.id,
        |row: &CommunityRow| SortKey::New {
            created_at: row.created_at,
        },
        Community::from,
    ))
}

/// Üye listesinin tek satırı: üye actor + üyeliğin kurulduğu an.
#[derive(Debug, Clone)]
pub struct MemberEntry {
    pub actor: ActorRecord,
    pub joined_at: DateTime<Utc>,
}

struct MemberRow {
    id: i64,
    username: String,
    actor_type: ActorType,
    display_name: Option<String>,
    bio: Option<String>,
    created_at: DateTime<Utc>,
    joined_at: DateTime<Utc>,
}

impl From<MemberRow> for MemberEntry {
    fn from(row: MemberRow) -> Self {
        Self {
            actor: ActorRecord {
                id: row.id,
                username: row.username,
                actor_type: row.actor_type,
                display_name: row.display_name,
                bio: row.bio,
                created_at: row.created_at,
            },
            joined_at: row.joined_at,
        }
    }
}

/// `GET /communities/{name}/members?cursor=&limit=`: üyeler, **en uzun
/// süredir üye olan önce** (`joined_at ASC, actor_id ASC`).
///
/// Bu sıra ileride devralma için önemli (COMMUNITY_PLAN.md §4), bu yüzden
/// burada `New` sıralamasının alışılmış tersine çevrilmiş (DESC) hâli
/// DEĞİL, artan hâli kullanılıyor — bkz. modül dokümanı "Sayfalama".
///
/// # Errors
/// Topluluk yoksa [`Error::NotFound`]; veritabanı hatası [`Error::Database`].
pub async fn list_members(
    pool: &PgPool,
    name: &str,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<MemberEntry>> {
    let community_id = resolve_community_id_in(pool, name).await?;
    let (cursor_joined_at, cursor_id) = split_new_cursor(cursor);

    let rows = sqlx::query_as!(
        MemberRow,
        r#"
        SELECT
            actors.id,
            actors.username,
            actors.actor_type AS "actor_type: ActorType",
            actors.display_name,
            actors.bio,
            actors.created_at,
            community_members.joined_at
        FROM community_members
        JOIN actors ON actors.id = community_members.actor_id
        WHERE community_members.community_id = $1
          AND (
              $2::timestamptz IS NULL
              OR (community_members.joined_at, community_members.actor_id)
                 > ($2::timestamptz, $3::bigint)
          )
        ORDER BY community_members.joined_at ASC, community_members.actor_id ASC
        LIMIT $4
        "#,
        community_id,
        cursor_joined_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row: &MemberRow| row.id,
        |row: &MemberRow| SortKey::New {
            created_at: row.joined_at,
        },
        MemberEntry::from,
    ))
}

// --- Topluluk akışı ----------------------------------------------------

/// Cursor'ı sıralamaya göre `(created_at, score, hot_score, id)` dörtlüsüne
/// ayırır — `crate::content::split_post_cursor`'ın birebir eşi, ama o
/// fonksiyon `content.rs`'e özel. Üç sıralamayı tek sorguyla ifade edemediğimiz
/// için (bkz. [`list_posts_in_community`]) her dal kendi parametrelerini
/// bağlıyor.
///
/// # Errors
/// Cursor listenin sıralamasına ait değilse [`Error::InvalidCursor`].
#[allow(clippy::type_complexity)]
fn split_community_post_cursor(
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

/// Bir [`CommunityPostRow`]'dan sıralamanın cursor anahtarını türetir.
fn community_post_sort_key(sort: PostSort, row: &CommunityPostRow) -> SortKey {
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

/// Topluluk akışı sorgularının satır tipi: [`crate::content::Content`]'in
/// tüm alanları + topluluk referansı.
///
/// `Content`'e çevrilirken `community_id`/`community_name` birlikte
/// doldurulduğu için `Some` topluluk referansı üretir — bu uçtaki her satır
/// zaten o topluluğa ait.
struct CommunityPostRow {
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
    community_id: Option<i64>,
    community_name: Option<String>,
    author_id: i64,
    author_username: String,
    author_actor_type: ActorType,
    author_display_name: Option<String>,
    author_bio: Option<String>,
    author_created_at: DateTime<Utc>,
    author_deleted_at: Option<DateTime<Utc>>,
    tags: Vec<String>,
}

impl From<CommunityPostRow> for Content {
    fn from(row: CommunityPostRow) -> Self {
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
        }
    }
}

/// `GET /communities/{name}/posts?sort=`: bir topluluğun postları.
///
/// `crate::content::list_posts_by_tag`'in üç-sıralamalı, iki-aşamalı `page`
/// CTE deseninin birebir aynısı (bkz. o fonksiyonun dokümantasyonu ve
/// `docs/query-plans.md`): `page` yalnızca sayfanın id'lerini `ORDER BY ...
/// LIMIT` ile keser, dıştaki sorgu az sayıda id için `communities`/`actors`/
/// etiket JOIN'lerini yapar. `communities` JOIN'i bilerek yalnızca dış
/// sorguda — iç CTE mümkün olduğunca dar kalsın.
///
/// Topluluk yoksa `404`; var olup hiç canlı post'u yoksa boş liste döner
/// (etiket ucundaki aynı ayrım).
///
/// # Errors
/// Topluluk yoksa [`Error::NotFound`]; cursor sıralamaya ait değilse
/// [`Error::InvalidCursor`]; veritabanı hatası [`Error::Database`].
pub async fn list_posts_in_community(
    pool: &PgPool,
    name: &str,
    sort: PostSort,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Content>> {
    let community_id = resolve_community_id_in(pool, name).await?;
    let (cursor_created_at, cursor_score, cursor_hot, cursor_id) =
        split_community_post_cursor(sort, cursor)?;

    let rows = match sort {
        PostSort::New => {
            sqlx::query_as!(
                CommunityPostRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    WHERE contents.community_id = $1
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
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
                    contents.created_at,
                    contents.edited_at,
                    contents.deleted_at,
                    communities.id AS "community_id?",
                    communities.name AS "community_name?",
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
                LEFT JOIN communities ON communities.id = contents.community_id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id, communities.id
                ORDER BY contents.created_at DESC, contents.id DESC
                "#,
                community_id,
                cursor_created_at,
                cursor_id,
                limit + 1,
            )
            .fetch_all(pool)
            .await?
        }
        PostSort::Top => {
            sqlx::query_as!(
                CommunityPostRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    WHERE contents.community_id = $1
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
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
                    contents.created_at,
                    contents.edited_at,
                    contents.deleted_at,
                    communities.id AS "community_id?",
                    communities.name AS "community_name?",
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
                LEFT JOIN communities ON communities.id = contents.community_id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id, communities.id
                ORDER BY contents.score DESC, contents.id DESC
                "#,
                community_id,
                cursor_score,
                cursor_id,
                limit + 1,
            )
            .fetch_all(pool)
            .await?
        }
        PostSort::Hot => {
            sqlx::query_as!(
                CommunityPostRow,
                r#"
                WITH page AS (
                    SELECT contents.id
                    FROM contents
                    WHERE contents.community_id = $1
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
                    contents.score,
                    contents.upvotes,
                    contents.downvotes,
                    contents.comment_count,
                    contents.hot_score,
                    contents.created_at,
                    contents.edited_at,
                    contents.deleted_at,
                    communities.id AS "community_id?",
                    communities.name AS "community_name?",
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
                LEFT JOIN communities ON communities.id = contents.community_id
                JOIN actors ON actors.id = contents.actor_id
                LEFT JOIN content_tags ON content_tags.content_id = page.id
                LEFT JOIN tags ON tags.id = content_tags.tag_id
                GROUP BY contents.id, actors.id, communities.id
                ORDER BY contents.hot_score DESC, contents.id DESC
                "#,
                community_id,
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
        |row: &CommunityPostRow| row.id,
        |row: &CommunityPostRow| community_post_sort_key(sort, row),
        Content::from,
    ))
}
