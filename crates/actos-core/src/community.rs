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
//! ## Faz 4B-1: özel toplulukların yaşam döngüsü
//!
//! Faz 4A görünürlük kapısını ([`crate::visibility`]) tüm okuma yollarına
//! yaydı; 4B-1 anahtarı çeviriyor: [`create_community`] artık `private`
//! kabul ediyor, [`get_community_for_viewer`] göremeyene **kapak** dönüyor
//! (§2), [`update_community`] public→private tek yönlü geçişini uyguluyor
//! ve [`close_community`] / [`handle_owner_departure_in_tx`] kapanış ile
//! devralmayı (§4) yürütüyor. Davet/başvuru akışı ayrı bir iştir; burada
//! yok.
//!
//! **Kapalı topluluk her uçta `404`** (`closed_at IS NOT NULL`): kapanış
//! satırı silmez, adı rezerve tutar. Public bir topluluğun postları
//! bağımsıza bırakılır (`community_id = NULL`), private'ınki soft-delete
//! edilir — kapalı kapı ardında yazılan içerik yok olur (§4).
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
use sqlx::{PgConnection, PgPool, Postgres};

use crate::{
    actor::{Page, paginate, split_new_cursor},
    auth::{ActorRecord, ActorType, Grant, Permission, PermissionScope},
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

/// Bir topluluk sahibinin kendi topluluğunda otomatik olarak tuttuğu
/// topluluk kapsamlı izinler (COMMUNITY_PLAN.md §4-5).
///
/// **Sahiplik gizli bir süper kullanıcı değildir.** Sahip bu satırları
/// [`create_community`] transaction'ında gerçek `permissions` kayıtları
/// olarak alır; yetki kontrolü yapan her yer onları yine [`crate::authz`]
/// üzerinden okur. Böylece "sahip şunu da yapabilmeli" diye ayrı bir kod
/// yolu yok ve sahip olmak, izinlere sahip olmanın başka bir adıdır.
///
/// Liste `migrations/0030_community_moderation.up.sql`'in backfill'iyle
/// **birebir aynı** olmalı; biri değişirse diğeri de değişmeli.
pub const OWNER_PERMISSIONS: &[Permission] = &[
    Permission::ContentDelete,
    Permission::CommunityEdit,
    Permission::CommunityClose,
    Permission::MemberInvite,
    Permission::MemberApprove,
    Permission::MemberKick,
    Permission::MemberBan,
    Permission::RoleGrant,
    Permission::ReportView,
    Permission::ReportResolve,
];

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

    /// İstemciden gelen `visibility` dizesini ayrıştırır.
    ///
    /// # Errors
    /// `public`/`private` dışındaki her değer [`Error::Validation`] —
    /// sessizce varsayılana düşmek, yazım hatası yapan istemciye istediği
    /// görünürlüğü vermezdi ([`PostSort::parse`]'teki aynı gerekçe).
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "public" => Ok(Self::Public),
            "private" => Ok(Self::Private),
            other => Err(Error::Validation(format!(
                "invalid visibility value: \"{other}\" (expected: public, private)"
            ))),
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
    /// Sahibin atadığı devralıcı (§4) — [`handle_owner_departure_in_tx`]
    /// kararında kullanılıyor.
    pub successor_actor_id: Option<i64>,
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
/// **Kapalı topluluk da yok sayılır** (§4): `closed_at IS NOT NULL` bir
/// satır buradan geçmez, dolayısıyla kapalı bir topluluğa post açmak da
/// [`Error::NotFound`] döner.
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
        r#"SELECT id FROM communities WHERE name = $1 AND closed_at IS NULL"#,
        normalized.as_str()
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("community"))
}

/// [`resolve_community_id_in`]'e ek olarak sahibi, görünürlüğü ve atanmış
/// devralıcıyı da getirir.
///
/// # Errors
/// Böyle bir topluluk yoksa (ya da kapalıysa) [`Error::NotFound`];
/// veritabanı hatası [`Error::Database`].
pub(crate) async fn lookup_community_in<'e, E>(executor: E, name: &str) -> Result<CommunityLookup>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let normalized = text::normalize_text(name);
    let row = sqlx::query!(
        r#"
        SELECT id, owner_actor_id, visibility AS "visibility: CommunityVisibility",
               successor_actor_id
        FROM communities
        WHERE name = $1 AND closed_at IS NULL
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
        successor_actor_id: row.successor_actor_id,
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

/// Topluluk adını iç kimliğe çevirir; yoksa [`Error::NotFound`].
///
/// [`resolve_community_id_in`]'in havuz üzerinden çalışan, `pub` hâli.
/// API katmanı (izin kapsamı çözümü, ban hedefi) adı kullanıcıdan alıp
/// içeride id'ye çevirmek zorunda; bu, o çevirinin tek noktası.
///
/// # Errors
/// Böyle bir topluluk yoksa [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
pub async fn resolve_id_by_name(pool: &PgPool, name: &str) -> Result<i64> {
    resolve_community_id_in(pool, name).await
}

/// Verilen topluluk id'lerinin adlarını döner (`id -> name`).
///
/// `whoami` gibi topluluk kapsamlı izinleri isimle göstermesi gereken
/// yanıtlar için: kapsam çözümünde topluluk başına ayrı sorgu atmak N+1
/// olurdu. Var olmayan id'ler (ör. silinmiş bir topluluk) sonuçta yer
/// almaz; çağıran `None` olarak ele alır.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn names_for(
    pool: &PgPool,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let rows = sqlx::query!(
        r#"SELECT id, name::text AS "name!" FROM communities WHERE id = ANY($1::bigint[])"#,
        ids,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|row| (row.id, row.name)).collect())
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
/// `visibility` dâhil her iki değeri de kabul eder (Faz 4B-1); `private`
/// oluşturmak topluluğu dizinden gizler ve kapağını gösterir (§2).
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

    // Sahiplik = izin (COMMUNITY_PLAN.md §5): gizli bir süper kullanıcı yok,
    // sahip bütün topluluk kapsamlı izinleri gerçek satırlar olarak alır.
    // Aynı transaction'da yazılıyor ki topluluk görünüp de sahibi bir an
    // için yetkisiz kalmasın.
    for permission in OWNER_PERMISSIONS {
        crate::auth::grant_permission_in_tx(
            &mut tx,
            owner_id,
            *permission,
            crate::auth::PermissionScope::Community,
            Some(community_id),
            None,
        )
        .await?;
    }

    tx.commit().await?;

    get_community(pool, &name).await
}

/// `PATCH /communities/{name}`: yalnızca sahibi **veya** bu toplulukta
/// `community.edit` sahibi; açıklamayı ve (isteğe bağlı) görünürlüğü
/// günceller.
///
/// Görünürlük geçişi **tek yönlüdür** (§2): public → private serbest,
/// private → public [`Error::Validation`]. Gerekçe: private'a geçmek zaten
/// açık olan içeriği gizler (zararsız), ama public'e dönmek kapalı kapı
/// ardında tutulan konuşmaları ifşa eder. Aynı değeri tekrar göndermek
/// hata değil, etkisiz bir güncellemedir.
///
/// # Errors
/// Topluluk yoksa (ya da kapalıysa) [`Error::NotFound`]; çağıran ne sahibi
/// ne bu toplulukta `community.edit` sahibiyse [`Error::Forbidden`];
/// açıklama doğrulamadan geçmezse ya da private→public istenirse
/// [`Error::Validation`]; veritabanı hatası [`Error::Database`].
pub async fn update_community(
    pool: &PgPool,
    actor_id: i64,
    permissions: &[Grant],
    name: &str,
    new_description: Option<&str>,
    new_visibility: Option<CommunityVisibility>,
) -> Result<Community> {
    let lookup = lookup_community(pool, name).await?;

    let is_owner = lookup.owner_actor_id == actor_id;
    let can_edit = crate::authz::has_for(permissions, Permission::CommunityEdit, Some(lookup.id));

    if !is_owner && !can_edit {
        return Err(Error::Forbidden);
    }

    // `None` = alanı değiştirme (kısmi güncelleme): yalnızca görünürlüğü
    // çevirmek isteyen bir istemci açıklamayı yeniden göndermek zorunda
    // kalmasın — aksi hâlde araya giren bir düzenleme sessizce ezilirdi.
    let description = new_description.map(validate_description).transpose()?;

    if new_visibility == Some(CommunityVisibility::Public)
        && lookup.visibility == CommunityVisibility::Private
    {
        return Err(Error::Validation(
            "a private community cannot become public".to_owned(),
        ));
    }

    // `None` = görünürlüğe dokunma; aynı değeri yazmak da etkisiz.
    let visibility = new_visibility.unwrap_or(lookup.visibility);

    sqlx::query!(
        r#"
        UPDATE communities
        SET description = COALESCE($2, description), visibility = $3, updated_at = now()
        WHERE id = $1
        "#,
        lookup.id,
        description,
        visibility as CommunityVisibility,
    )
    .execute(pool)
    .await?;

    get_community(pool, name).await
}

/// `POST /communities/{name}/join`: public topluluğa anında üyelik.
///
/// **İdempotent:** zaten üye olmak hata değil (`ON CONFLICT DO NOTHING`).
///
/// Private topluluğa doğrudan katılma **reddedilir** (§3): oraya giriş
/// davet ya da başvuru iledir (ayrı iş). `Error::Validation`, `403` değil —
/// istek yanlış biçimde kurulmuş, çağıranın kim olduğu değil.
///
/// # Errors
/// Topluluk yoksa (ya da kapalıysa) [`Error::NotFound`]; topluluk private
/// ise [`Error::Validation`]; çağıran o topluluktan banlıysa
/// [`Error::Banned`]; veritabanı hatası [`Error::Database`].
pub async fn join_community(pool: &PgPool, actor_id: i64, name: &str) -> Result<()> {
    let lookup = lookup_community(pool, name).await?;

    if lookup.visibility != CommunityVisibility::Public {
        return Err(Error::Validation(
            "private communities cannot be joined directly; use an invitation or application"
                .to_owned(),
        ));
    }

    // Topluluk ban'ı katılmayı engeller (COMMUNITY_PLAN.md §6): ban
    // ileriye dönüktür, üyeliği de kapsar.
    if crate::moderation::is_banned_from_community(pool, lookup.id, actor_id).await? {
        return Err(Error::Banned);
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
/// **Sahip ayrılabilir** (Faz 4B-1): ayrılma devralmayı tetikler (§4). Yerine
/// atanmış ve hâlâ canlı bir devralıcı varsa sahiplik ona geçer; yoksa en
/// kıdemli topluluk moderatörü sahiplenir; o da yoksa topluluk kapanır
/// ([`handle_owner_departure_in_tx`]).
///
/// **İdempotent:** üye olmayanın ayrılma isteği hata değil.
///
/// # Errors
/// Topluluk yoksa (ya da kapalıysa) [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
pub async fn leave_community(pool: &PgPool, actor_id: i64, name: &str) -> Result<()> {
    let lookup = lookup_community(pool, name).await?;

    let mut tx = pool.begin().await?;

    if lookup.owner_actor_id == actor_id {
        transfer_or_close_in_tx(
            &mut tx,
            actor_id,
            lookup.id,
            lookup.visibility,
            lookup.successor_actor_id,
        )
        .await?;
    } else {
        sqlx::query!(
            r#"DELETE FROM community_members WHERE community_id = $1 AND actor_id = $2"#,
            lookup.id,
            actor_id,
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(())
}

// --- Kapanış ve devralma (Faz 4B-1) ------------------------------------

/// `POST /communities/{name}/close`: topluluğu kapatır.
///
/// Yetki [`crate::authz::has_for`] ile `community.close` için, **hedef
/// topluluğun** kapsamında sorulur: sahip bu izni zaten topluluk kapsamlı
/// bir satır olarak tutar, global `community.close` her topluluğu kapatır.
///
/// Kapanış satırı silmez; `closed_at` işaretler (§4). İçeriğin kaderi
/// görünürlüğe bağlıdır: **public** topluluğun içeriği bağımsıza bırakılır
/// (`community_id = NULL`; yazar/oy/yorum ağı korunur), **private** topluluğun
/// içeriği soft-delete edilir — kapalı kapı ardında yazılan metin bağımsız
/// bir post olarak herkese açılamaz.
///
/// Zaten kapalı bir topluluğu kapatmak [`Error::NotFound`]: kapanış kalıcıdır
/// ve tekrarlanabilir bir eylem değildir.
///
/// # Errors
/// Topluluk yoksa (ya da kapalıysa) [`Error::NotFound`]; çağıranın
/// `community.close` yetkisi yoksa [`Error::Forbidden`]; veritabanı hatası
/// [`Error::Database`].
pub async fn close_community(
    pool: &PgPool,
    actor_id: i64,
    permissions: &[Grant],
    name: &str,
    reason: Option<&str>,
) -> Result<()> {
    let lookup = lookup_community(pool, name).await?;

    if !crate::authz::has_for(permissions, Permission::CommunityClose, Some(lookup.id)) {
        return Err(Error::Forbidden);
    }

    let mut tx = pool.begin().await?;

    apply_closure_in_tx(&mut tx, lookup.id, lookup.visibility).await?;

    // Denetim izi hedefi topluluğun kendisi (`target_type = "community"`):
    // içerik silme/kick kayıtlarından ayırt edilebilsin diye.
    crate::moderation::log_action(
        &mut tx,
        actor_id,
        "community_close",
        "community",
        lookup.id,
        reason,
    )
    .await?;

    tx.commit().await?;

    Ok(())
}

/// `PUT /communities/{name}/successor`: sahibin devralıcı ataması.
///
/// **Yalnızca sahip** ([`Error::Forbidden`]): `community.edit` bu kararı
/// vermez. Hedef **var olmalı ve silinmemiş olmalı** ([`Error::NotFound`]).
/// Sahibin kendisini göndermesi atamayı **temizler**: ayrılış anında
/// [`resolve_successor_in_tx`] sahibi aday saymaz, sıra en kıdemli
/// moderatöre (yoksa kapanışa) geçer.
///
/// # Errors
/// Topluluk yoksa (ya da kapalıysa) [`Error::NotFound`]; çağıran sahibi
/// değilse [`Error::Forbidden`]; hedef actor yoksa/silinmişse
/// [`Error::NotFound`]; veritabanı hatası [`Error::Database`].
pub async fn set_successor(
    pool: &PgPool,
    actor_id: i64,
    name: &str,
    successor_username: &str,
) -> Result<()> {
    let lookup = lookup_community(pool, name).await?;

    if lookup.owner_actor_id != actor_id {
        return Err(Error::Forbidden);
    }

    let normalized = text::normalize_text(successor_username);
    let target_id = sqlx::query_scalar!(
        r#"SELECT id FROM actors WHERE username = $1 AND deleted_at IS NULL"#,
        normalized,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("actor"))?;

    sqlx::query!(
        r#"UPDATE communities SET successor_actor_id = $2, updated_at = now() WHERE id = $1"#,
        lookup.id,
        target_id,
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Sahibi ayrılan **her** topluluk için devralma ya da kapanış uygular.
///
/// [`crate::actor::delete_account`] hesabı soft-delete ederken, silinen
/// aktörün sahibi olduğu bütün topluluklar için çağırır — ayrılma
/// ([`leave_community`]) ise yalnızca tek bir topluluk için
/// [`transfer_or_close_in_tx`]'i doğrudan çağırır.
///
/// Çağıranın transaction'ında çalışır çünkü hesap silme ile devralma ya
/// hep birlikte gerçekleşmeli ya da hiç: sahibi silinmiş ama devralınmamış
/// bir topluluk ara durumu olamaz.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn handle_owner_departure_in_tx(tx: &mut PgConnection, owner_id: i64) -> Result<()> {
    let owned = sqlx::query!(
        r#"
        SELECT id,
               visibility AS "visibility: CommunityVisibility",
               successor_actor_id
        FROM communities
        WHERE owner_actor_id = $1 AND closed_at IS NULL
        ORDER BY id
        "#,
        owner_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    for community in owned {
        transfer_or_close_in_tx(
            &mut *tx,
            owner_id,
            community.id,
            community.visibility,
            community.successor_actor_id,
        )
        .await?;
    }

    Ok(())
}

/// Topluluğu kapatır ve içeriğini görünürlüğe göre tasfiye eder.
///
/// `close` ucu ile devralıcısız kapanış aynı davranışı paylaşsın diye tek
/// yerde; çağıran transaction'ı yönetir.
async fn apply_closure_in_tx(
    tx: &mut PgConnection,
    community_id: i64,
    visibility: CommunityVisibility,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE communities SET closed_at = now(), updated_at = now() WHERE id = $1"#,
        community_id,
    )
    .execute(&mut *tx)
    .await?;

    match visibility {
        // Public: post ve yorumlar bağımsız kalır. Yazar, oy ve yorum
        // ağı korunur; yalnızca topluluk bağı kopar (§4).
        CommunityVisibility::Public => {
            sqlx::query!(
                r#"UPDATE contents SET community_id = NULL WHERE community_id = $1"#,
                community_id,
            )
            .execute(&mut *tx)
            .await?;
        }
        // Private: kapalı kapı ardındaki içerik bağımsız olamaz, yok olur.
        CommunityVisibility::Private => {
            sqlx::query!(
                r#"UPDATE contents SET deleted_at = now()
                   WHERE community_id = $1 AND deleted_at IS NULL"#,
                community_id,
            )
            .execute(&mut *tx)
            .await?;
        }
    }

    Ok(())
}

/// Tek bir toplulukta sahip ayrılışını işler: devral ya da kapat.
///
/// Devralma sırası (§4): atanmış ve hâlâ canlı devralıcı; yoksa en kıdemli
/// topluluk kapsamlı izin sahibi. Devralan kişi üye yapılır ve
/// [`OWNER_PERMISSIONS`] satırları ona gerçek kayıtlar olarak verilir;
/// ayrılan sahibin o topluluktaki izinleri geri alınır. Devralan yoksa
/// topluluk kapanır ([`apply_closure_in_tx`]).
async fn transfer_or_close_in_tx(
    tx: &mut PgConnection,
    owner_id: i64,
    community_id: i64,
    visibility: CommunityVisibility,
    designated_successor: Option<i64>,
) -> Result<()> {
    let successor =
        resolve_successor_in_tx(tx, owner_id, community_id, designated_successor).await?;

    match successor {
        Some(new_owner) => {
            sqlx::query!(
                r#"
                UPDATE communities
                SET owner_actor_id = $2, successor_actor_id = NULL, updated_at = now()
                WHERE id = $1
                "#,
                community_id,
                new_owner,
            )
            .execute(&mut *tx)
            .await?;

            // Sahip her zaman üyedir; devralan üye değilse (yalnızca izin
            // sahibi olabilirdi) önce üye yapıyoruz.
            sqlx::query!(
                r#"
                INSERT INTO community_members (community_id, actor_id)
                VALUES ($1, $2)
                ON CONFLICT DO NOTHING
                "#,
                community_id,
                new_owner,
            )
            .execute(&mut *tx)
            .await?;

            for permission in OWNER_PERMISSIONS {
                crate::auth::grant_permission_in_tx(
                    &mut *tx,
                    new_owner,
                    *permission,
                    PermissionScope::Community,
                    Some(community_id),
                    None,
                )
                .await?;
            }

            // Ayrılan sahibin bu topluluktaki tüm topluluk kapsamlı
            // izinleri düşer; sahiplik artık onda değil.
            sqlx::query!(
                r#"DELETE FROM permissions WHERE actor_id = $1 AND community_id = $2"#,
                owner_id,
                community_id,
            )
            .execute(&mut *tx)
            .await?;
        }
        None => {
            apply_closure_in_tx(tx, community_id, visibility).await?;

            crate::moderation::log_action(
                &mut *tx,
                owner_id,
                "community_close",
                "community",
                community_id,
                Some("owner departed with no successor"),
            )
            .await?;
        }
    }

    // Ayrılan sahip artık bu topluluğun üyesi de değil. Kapanış dalında da
    // geçerli: kapalı topluluğun üye listesi anlamsız.
    sqlx::query!(
        r#"DELETE FROM community_members WHERE community_id = $1 AND actor_id = $2"#,
        community_id,
        owner_id,
    )
    .execute(&mut *tx)
    .await?;

    Ok(())
}

/// Devralıcıyı seçer: atanmış ve canlı aday, yoksa en kıdemli moderatör.
///
/// "En kıdemli" [(§4)] ölçütü `permissions.granted_at ASC, actor_id ASC`:
/// bu toplulukta en eski topluluk kapsamlı izni almış, silinmemiş aktör.
/// Sahibin kendisi hariç tutulur — ada bağlı olmasa bile "sahibini devral"
/// saçma olurdu. Kıdem, başka yerde kazanılmış bir itibarın aksine burada
/// geriye dönük edinilemez ve herkes için öngörülebilir.
///
/// Atanmış aday sahibin kendisiyse de yok sayılır: `set_successor`'ın
/// kendini göndermeyi kabul etmesi "atanmış devralıcıyı temizle" demenin
/// yoludur (bkz. `SuccessorRequest` dokümanı), aksi hâlde sahip ayrılırken
/// kendi kendini devralıp izinsiz/üyesiz kalırdı.
async fn resolve_successor_in_tx(
    tx: &mut PgConnection,
    owner_id: i64,
    community_id: i64,
    designated: Option<i64>,
) -> Result<Option<i64>> {
    if let Some(candidate) = designated.filter(|candidate| *candidate != owner_id) {
        let live = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                   SELECT 1 FROM actors WHERE id = $1 AND deleted_at IS NULL
               ) AS "exists!""#,
            candidate,
        )
        .fetch_one(&mut *tx)
        .await?;
        if live {
            return Ok(Some(candidate));
        }
    }

    let longest_serving = sqlx::query_scalar!(
        r#"
        SELECT p.actor_id
        FROM permissions p
        JOIN actors a ON a.id = p.actor_id
        WHERE p.community_id = $1
          AND p.scope = 'community'::permission_scope
          AND p.actor_id <> $2
          AND a.deleted_at IS NULL
        ORDER BY p.granted_at ASC, p.actor_id ASC
        LIMIT 1
        "#,
        community_id,
        owner_id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    Ok(longest_serving)
}

/// `DELETE /communities/{name}/members/{username}`: bir üyeyi topluluktan
/// atar.
///
/// Yetki [`crate::authz::has_for`] ile `member.kick` için, **hedef
/// topluluğun** kapsamında sorulur: topluluk kapsamlı bir moderatör yalnızca
/// kendi topluluğunda atabilir; global `member.kick` her toplulukta.
///
/// **Sahip atılamaz** ([`Error::Validation`]): devralma kuralları Faz 4'ün
/// işi, o gelene kadar sahipsiz topluluk oluşmasın diye kapı burada.
///
/// **Üye olmayan için [`Error::NotFound`]:** istek idempotent DEĞİL. "Zaten
/// değildi" ile "attım" ayrımını saklamıyoruz; denetim izine yalnızca
/// gerçekten bir üyelik silindiğinde kayıt düşülüyor (`revoke_key`'deki
/// aynı gerekçe).
///
/// # Errors
/// Topluluk ya da hedef actor yoksa [`Error::NotFound`]; çağıranın
/// `member.kick` yetkisi yoksa [`Error::Forbidden`]; hedef sahipse ya da
/// üye değilse [`Error::Validation`] / [`Error::NotFound`]; veritabanı
/// hatası [`Error::Database`].
pub async fn kick_member(
    pool: &PgPool,
    actor_id: i64,
    permissions: &[Grant],
    community_name: &str,
    target_username: &str,
) -> Result<()> {
    let lookup = lookup_community(pool, community_name).await?;

    if !crate::authz::has_for(permissions, Permission::MemberKick, Some(lookup.id)) {
        return Err(Error::Forbidden);
    }

    let target_id = crate::moderation::resolve_target_actor(pool, target_username).await?;

    if target_id == lookup.owner_actor_id {
        return Err(Error::Validation(
            "the owner cannot be kicked from their community".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;

    let silinen = sqlx::query!(
        r#"DELETE FROM community_members WHERE community_id = $1 AND actor_id = $2"#,
        lookup.id,
        target_id,
    )
    .execute(&mut *tx)
    .await?;

    if silinen.rows_affected() == 0 {
        return Err(Error::NotFound("membership"));
    }

    // Denetim izi gerekçesi topluluğu anıyor: `admin_actions_log.target_id`
    // yalnızca actor id'si, hangi topluluktan atıldığı yalnızca metinden
    // anlaşılıyor (target_type tek başına bunu taşıyamaz).
    crate::moderation::log_action(
        &mut tx,
        actor_id,
        "member_kick",
        "actor",
        target_id,
        Some(&format!("kicked from community \"{community_name}\"")),
    )
    .await?;

    tx.commit().await?;

    Ok(())
}

// --- Okuma yolları -----------------------------------------------------

/// `GET /communities/{name}`: tek topluluk + sahibi + sayaçlar.
///
/// Kapanmış topluluk [`Error::NotFound`] (§4): satır dursa da artık bir
/// topluluk ucu değildir.
///
/// # Errors
/// Topluluk yoksa ya da kapalıysa [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
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
        WHERE communities.name = $1 AND communities.closed_at IS NULL
        "#,
        normalized.as_str(),
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("community"))?;

    Ok(row.into())
}

/// [`get_community`]'ın **izleyiciye göre** karar veren hâli (COMMUNITY_PLAN.md §2).
///
/// Public topluluk herkese tam döner. Private topluluk yalnızca görebilene
/// (üye, topluluk kapsamlı izin sahibi, global moderatör) tam döner;
/// göremeyen `viewer_communities`'te yoksa **kapak** alır: aynı ad ve
/// açıklama, ama `member_count = 0`, `post_count = 0` — içeriden hiçbir şey
/// sızmaz. "Görebilir mi" kararı [`crate::visibility::visible_community_ids`]'in
/// ürettiği kümeye bırakılıyor; burada ikinci bir yetki mantığı yok.
///
/// Dönen [`Community`]'in sayaçları kapak dalında sıfırlanır; çağıran
/// `is_member`'ı kapak için `false` kabul etmeli.
///
/// # Errors
/// Topluluk yoksa ya da kapalıysa [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
pub async fn get_community_for_viewer(
    pool: &PgPool,
    name: &str,
    viewer_communities: &[i64],
) -> Result<CommunityView> {
    let mut community = get_community(pool, name).await?;

    if community.visibility == CommunityVisibility::Private
        && !viewer_communities.contains(&community.id)
    {
        community.member_count = 0;
        community.post_count = 0;
        return Ok(CommunityView::Cover(community));
    }

    Ok(CommunityView::Full(community))
}

/// [`get_community_for_viewer`]'ın iki dalı.
///
/// [`Community`]'i ayrı bir kapak tipine kopyalamak yerine taşıyoruz: kapak
/// yalnızca sayaçları sıfırlanmış bir topluluktur, DTO aynı kalır. Enum,
/// handler'ın "özel mi, kapak mı" ayrımını açıkça yazmasını sağlıyor.
pub enum CommunityView {
    /// İzleyici içeriyi görebiliyor (ya da topluluk public).
    Full(Community),
    /// Private topluluk, izleyici göremiyor: sayaçlar sıfır.
    Cover(Community),
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
          AND communities.closed_at IS NULL
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
/// Topluluk yoksa [`Error::NotFound`]; topluluk private ve okuyucu
/// göremiyorsa [`Error::Forbidden`]; veritabanı hatası [`Error::Database`].
pub async fn list_members(
    pool: &PgPool,
    name: &str,
    cursor: Option<Cursor>,
    limit: i64,
    viewer_communities: &[i64],
) -> Result<Page<MemberEntry>> {
    let community = lookup_community(pool, name).await?;

    // Özel topluluğun üye listesi de public yüzey değil (Faz 4A): kapak
    // sayfası üye listesi göstermez (§2), dolayısıyla göremeyen okuyucuya
    // burada da `403`. Görebilen üye/moderatör listeyi alır.
    if community.visibility == CommunityVisibility::Private
        && !viewer_communities.contains(&community.id)
    {
        return Err(Error::Forbidden);
    }

    let community_id = community.id;
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
/// **Özel topluluk public bir yüzey değildir** (Faz 4A): okuyucu o
/// topluluğu göremiyorsa (üye değil ve topluluk kapsamlı izni yok)
/// [`Error::Forbidden`] döner. Kapak sayfası Faz 4B'nin işi. Public
/// topluluklar için kapı yok — içerik zaten herkese açık.
///
/// # Errors
/// Topluluk yoksa [`Error::NotFound`]; topluluk private ve okuyucu
/// göremiyorsa [`Error::Forbidden`]; cursor sıralamaya ait değilse
/// [`Error::InvalidCursor`]; veritabanı hatası [`Error::Database`].
pub async fn list_posts_in_community(
    pool: &PgPool,
    name: &str,
    sort: PostSort,
    cursor: Option<Cursor>,
    limit: i64,
    viewer_communities: &[i64],
) -> Result<Page<Content>> {
    let community = lookup_community(pool, name).await?;

    if community.visibility == CommunityVisibility::Private
        && !viewer_communities.contains(&community.id)
    {
        return Err(Error::Forbidden);
    }

    let community_id = community.id;
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
                viewer_communities,
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
                viewer_communities,
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
                viewer_communities,
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

// --- Faz 4B-2: davetler ve başvurular ----------------------------------
//
// Private topluluğa giriş iki yönlüdür ve ikisi de bir gelen kutusu öğesi
// üretir (§3): moderatörün gönderdiği **davet** ve kişinin yazdığı
// **başvuru**. Public topluluğa doğrudan katılmak anında gerçekleştiği için
// (`join_community`) public'e yapılan davet/başvuru bir istemci hatasıdır —
// yok sayılmaz, [`Error::Validation`] döner.
//
// Davet edilen/başvuran kişi, kabul edilene kadar üye DEĞİLDİR. Her iki
// tabloda `(community_id, actor_id)` üzerinde kısmi bir `UNIQUE` index var
// (yalnızca `pending` satırlar için): aynı kişiye ikinci bekleyen davet ya da
// başvuru sessizce birikmez, [`Error::Conflict`] döner. Çözülmüş bir satır
// index'ten çıkar, dolayısıyla kişi ileride yeniden davet edilebilir.
//
// Reddetmek/iptal etmek satırı silmez; durumu ve `resolved_at`'i yazar
// (`migrations/0033`'ün çözüm şekli CHECK'i bunu zorunlu kılıyor).

/// `migrations/0033_invitations_applications.up.sql` →
/// `ck_community_applications_reason_length` üst sınırı.
const APPLICATION_REASON_MAX: usize = 2000;

/// `migrations/0033_invitations_applications.up.sql` → `invitation_status`
/// Postgres enum'ının Rust karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "invitation_status", rename_all = "snake_case")]
pub enum InvitationStatus {
    Pending,
    Accepted,
    Declined,
}

/// `migrations/0033_invitations_applications.up.sql` → `application_status`
/// Postgres enum'ının Rust karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "application_status", rename_all = "snake_case")]
pub enum ApplicationStatus {
    Pending,
    Accepted,
    Rejected,
}

impl ApplicationStatus {
    /// API'de görünen dize.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }

    /// İstemciden gelen `?status=` dizesini ayrıştırır.
    ///
    /// # Errors
    /// Tanınmayan değer [`Error::Validation`] — [`CommunityVisibility::parse`]
    /// ile aynı gerekçe: yazım hatası yapan istemciye sessizce varsayılan
    /// dönmek, istediği filtreyi uygulamamak olurdu.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            other => Err(Error::Validation(format!(
                "invalid status: \"{other}\" (expected: pending, accepted, rejected)"
            ))),
        }
    }
}

/// Davet/başvuru gerekçesini doğrular: [`text::normalize_text`] uygular,
/// boş olamama ve uzunluk kuralını işletir.
///
/// # Errors
/// Normalize edildikten sonra boşsa ya da `2000` karakteri aşarsa
/// [`Error::Validation`].
fn validate_reason(raw: &str) -> Result<String> {
    let normalized = text::normalize_text(raw);
    let len = normalized.chars().count();

    if len == 0 {
        return Err(Error::Validation(
            "application reason cannot be empty".to_owned(),
        ));
    }
    if len > APPLICATION_REASON_MAX {
        return Err(Error::Validation(format!(
            "application reason can be at most {APPLICATION_REASON_MAX} characters (received: {len} characters)"
        )));
    }

    Ok(normalized)
}

/// Alıcıya giden tek bir bekleyen davet: topluluk referansı + davet eden
/// actor + kurulma anı.
#[derive(Debug, Clone)]
pub struct Invitation {
    pub id: i64,
    pub community: CommunityRef,
    /// Daveti gönderen moderatör.
    pub invited_by: ActorRecord,
    pub created_at: DateTime<Utc>,
}

/// Moderasyon kuyruğundaki tek bir başvuru.
#[derive(Debug, Clone)]
pub struct Application {
    pub id: i64,
    pub community: CommunityRef,
    pub applicant: ActorRecord,
    pub reason: String,
    pub status: ApplicationStatus,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

struct InvitationRow {
    id: i64,
    community_id: i64,
    community_name: String,
    invited_by_id: i64,
    invited_by_username: String,
    invited_by_actor_type: ActorType,
    invited_by_display_name: Option<String>,
    invited_by_bio: Option<String>,
    invited_by_created_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
}

impl From<InvitationRow> for Invitation {
    fn from(row: InvitationRow) -> Self {
        Self {
            id: row.id,
            community: CommunityRef {
                id: row.community_id,
                name: row.community_name,
            },
            invited_by: ActorRecord {
                id: row.invited_by_id,
                username: row.invited_by_username,
                actor_type: row.invited_by_actor_type,
                display_name: row.invited_by_display_name,
                bio: row.invited_by_bio,
                created_at: row.invited_by_created_at,
            },
            created_at: row.created_at,
        }
    }
}

struct ApplicationRow {
    id: i64,
    community_id: i64,
    community_name: String,
    applicant_id: i64,
    applicant_username: String,
    applicant_actor_type: ActorType,
    applicant_display_name: Option<String>,
    applicant_bio: Option<String>,
    applicant_created_at: DateTime<Utc>,
    reason: String,
    status: ApplicationStatus,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
}

impl From<ApplicationRow> for Application {
    fn from(row: ApplicationRow) -> Self {
        Self {
            id: row.id,
            community: CommunityRef {
                id: row.community_id,
                name: row.community_name,
            },
            applicant: ActorRecord {
                id: row.applicant_id,
                username: row.applicant_username,
                actor_type: row.applicant_actor_type,
                display_name: row.applicant_display_name,
                bio: row.applicant_bio,
                created_at: row.applicant_created_at,
            },
            reason: row.reason,
            status: row.status,
            created_at: row.created_at,
            resolved_at: row.resolved_at,
        }
    }
}

/// `POST /communities/{name}/invitations`: bir moderatör, kullanıcı adıyla
/// private topluluğa davet eder.
///
/// Yalnızca **private** topluluk davet kabul eder (§3): public topluluğa
/// katılmak anında olduğu için oraya davet anlamsızdır, [`Error::Validation`].
/// Yetki [`crate::authz::has_for`] ile `member.invite` için, **hedef
/// topluluğun** kapsamında sorulur; sahip bu izni topluluk kapsamlı bir satır
/// olarak tutar.
///
/// Hedef actor **var olmalı ve canlı olmalı** ([`crate::actor::resolve_live_actor_id`]);
/// zaten üyeye [`Error::Validation`], topluluktan banlıya [`Error::Banned`]
/// döner. Davet `pending` yazılır ve alıcıya `community_invitation`
/// bildirimi gider.
///
/// **Aynı kişiye ikinci bekleyen davet [`Error::Conflict`]:** kısmi `UNIQUE`
/// index çakışır, satır birikmez ve ikinci bildirim gönderilmez. İdempotent
/// değil çünkü "davet gönder" iki kez çağrıldığında kullanıcının niyeti
/// genelde gerçekten ikinci bir hatırlatmadır, ama bildirim spam'i olurdu;
/// çakışmayı açıkça söylemek daha dürüst.
///
/// # Errors
/// Topluluk yoksa (ya da kapalıysa) [`Error::NotFound`]; topluluk public ise,
/// hedef zaten üye ise, hedef actor değilse (silinmişse [`Error::Gone`]) ya
/// da hedef actor yoksa [`Error::NotFound`] / [`Error::Validation`]; çağıranın
/// `member.invite` yetkisi yoksa [`Error::Forbidden`]; zaten bekleyen davet
/// varsa [`Error::Conflict`]; hedef banlıysa [`Error::Banned`]; veritabanı
/// hatası [`Error::Database`].
pub async fn invite_member(
    pool: &PgPool,
    inviter_id: i64,
    permissions: &[Grant],
    community_name: &str,
    username: &str,
) -> Result<()> {
    let lookup = lookup_community(pool, community_name).await?;

    if lookup.visibility != CommunityVisibility::Private {
        return Err(Error::Validation(
            "public communities join instantly; invitations are for private communities".to_owned(),
        ));
    }

    if !crate::authz::has_for(permissions, Permission::MemberInvite, Some(lookup.id)) {
        return Err(Error::Forbidden);
    }

    let target_id = crate::actor::resolve_live_actor_id(pool, username).await?;

    let mut tx = pool.begin().await?;

    if is_member_in(&mut *tx, lookup.id, target_id).await? {
        return Err(Error::Validation(
            "the invited actor is already a member".to_owned(),
        ));
    }

    if crate::moderation::is_banned_from_community_in(&mut *tx, lookup.id, target_id).await? {
        return Err(Error::Banned);
    }

    let inserted = sqlx::query!(
        r#"
        INSERT INTO community_invitations (community_id, invited_actor_id, invited_by)
        VALUES ($1, $2, $3)
        "#,
        lookup.id,
        target_id,
        inviter_id,
    )
    .execute(&mut *tx)
    .await;

    if let Err(sqlx::Error::Database(db_err)) = inserted {
        // `uq_community_invitations_pending`: aynı kişiye zaten bekleyen bir
        // davet var. Gerekçe fonksiyon dokümanında (Conflict seçimi).
        if db_err.is_unique_violation() {
            return Err(Error::Conflict(
                "an invitation is already pending for this actor".to_owned(),
            ));
        }
        return Err(Error::from(sqlx::Error::Database(db_err)));
    }

    crate::notification::create_notification(
        &mut tx,
        target_id,
        crate::notification::NotificationKind::CommunityInvitation,
        Some(inviter_id),
        "community",
        lookup.id,
        serde_json::json!({ "community": text::normalize_text(community_name) }),
    )
    .await?;

    tx.commit().await?;

    Ok(())
}

/// `GET /me/invitations?cursor=&limit=`: çağırana adreslenmiş bekleyen
/// davetler, en yeni önce.
///
/// Topluluğu kapanmış davet listelenmez (`closed_at IS NULL`): kapanış satırı
/// silmez, ama o topluluğa kabul edilmek artık `404` olurdu — daveti
/// göstermek yanıltıcı olurdu.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn list_my_invitations(
    pool: &PgPool,
    actor_id: i64,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Invitation>> {
    let (cursor_created_at, cursor_id) = split_new_cursor(cursor);

    let rows = sqlx::query_as!(
        InvitationRow,
        r#"
        SELECT
            invitations.id,
            communities.id AS community_id,
            communities.name AS community_name,
            actors.id AS invited_by_id,
            actors.username AS invited_by_username,
            actors.actor_type AS "invited_by_actor_type: ActorType",
            actors.display_name AS invited_by_display_name,
            actors.bio AS invited_by_bio,
            actors.created_at AS invited_by_created_at,
            invitations.created_at
        FROM community_invitations AS invitations
        JOIN communities ON communities.id = invitations.community_id
        JOIN actors ON actors.id = invitations.invited_by
        WHERE invitations.invited_actor_id = $1
          AND invitations.status = 'pending'::invitation_status
          AND communities.closed_at IS NULL
          AND (
              $2::timestamptz IS NULL
              OR (invitations.created_at, invitations.id) < ($2::timestamptz, $3::bigint)
          )
        ORDER BY invitations.created_at DESC, invitations.id DESC
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
        |row: &InvitationRow| row.id,
        |row: &InvitationRow| SortKey::New {
            created_at: row.created_at,
        },
        Invitation::from,
    ))
}

/// `POST /me/invitations/{id}/accept`: daveti kabul eder, üyeliği kurar.
///
/// Davet **çağıranın olmalı ve hâlâ `pending` olmalı**; başka birinin daveti
/// ile "hiç yok" ayrımı sızdırılmadan [`Error::NotFound`], çözülmüş davet
/// [`Error::Conflict`]. Topluluk açık olmalı ([`Error::NotFound`]) ve çağıran
/// o topluluktan banlı olmamalı ([`Error::Banned`]): private topluluğun banı
/// davet kabulünü de kapsar (§6). Üyelik `ON CONFLICT DO NOTHING` ile
/// kurulur — başka bir yoldan çoktan üye olmak hata değil.
///
/// Üyelik + davetin `accepted` işaretlenmesi **tek transaction'da**.
///
/// # Errors
/// Davet yoksa/silinmişse ya da çağırana ait değilse [`Error::NotFound`];
/// davet artık pending değilse [`Error::Conflict`]; topluluk kapalıysa
/// [`Error::NotFound`]; çağıran banlıysa [`Error::Banned`]; veritabanı hatası
/// [`Error::Database`].
pub async fn accept_invitation(pool: &PgPool, actor_id: i64, invitation_id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;

    let invitation = sqlx::query!(
        r#"
        SELECT community_id, status AS "status: InvitationStatus"
        FROM community_invitations
        WHERE id = $1 AND invited_actor_id = $2
        FOR UPDATE
        "#,
        invitation_id,
        actor_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("invitation"))?;

    if invitation.status != InvitationStatus::Pending {
        return Err(Error::Conflict(
            "invitation is no longer pending".to_owned(),
        ));
    }

    // Kapanmış topluluğa katılma yok (§4): satır durusa da uç artık 404.
    let open = sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM communities WHERE id = $1 AND closed_at IS NULL
           ) AS "exists!""#,
        invitation.community_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    if !open {
        return Err(Error::NotFound("community"));
    }

    if crate::moderation::is_banned_from_community_in(&mut *tx, invitation.community_id, actor_id)
        .await?
    {
        return Err(Error::Banned);
    }

    sqlx::query!(
        r#"
        INSERT INTO community_members (community_id, actor_id)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        invitation.community_id,
        actor_id,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        UPDATE community_invitations
        SET status = 'accepted'::invitation_status, resolved_at = now()
        WHERE id = $1
        "#,
        invitation_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

/// `POST /me/invitations/{id}/decline`: daveti reddeder.
///
/// Sahiplik ve `pending` kontrolleri [`accept_invitation`] ile aynıdır;
/// üyelik kurulmaz, satır `declined` + `resolved_at` olur. Topluluğun açık
/// olması gerekmez: davet edildiğin bir topluluğun bu arada kapanmış olması
/// reddetmeyi engellememeli.
///
/// # Errors
/// Davet yoksa/silinmişse ya da çağırana ait değilse [`Error::NotFound`];
/// davet artık pending değilse [`Error::Conflict`]; veritabanı hatası
/// [`Error::Database`].
pub async fn decline_invitation(pool: &PgPool, actor_id: i64, invitation_id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;

    let invitation = sqlx::query!(
        r#"
        SELECT status AS "status: InvitationStatus"
        FROM community_invitations
        WHERE id = $1 AND invited_actor_id = $2
        FOR UPDATE
        "#,
        invitation_id,
        actor_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("invitation"))?;

    if invitation.status != InvitationStatus::Pending {
        return Err(Error::Conflict(
            "invitation is no longer pending".to_owned(),
        ));
    }

    sqlx::query!(
        r#"
        UPDATE community_invitations
        SET status = 'declined'::invitation_status, resolved_at = now()
        WHERE id = $1
        "#,
        invitation_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

/// `POST /communities/{name}/applications`: private topluluğa başvuru.
///
/// Topluluk **private** olmalı ([`Error::Validation`] public için), çağıran
/// üye olmamalı ([`Error::Validation`]) ve topluluktan banlı olmamalı
/// ([`Error::Banned`]). Gerekçe 1-2000 karakter ([`validate_reason`]).
///
/// Başvuru `pending` yazılır ve o toplulukta `member.approve` tutan **her**
/// aktöre `community_application` bildirimi gider (`permissions` tablosundan
/// sorulur; sahip bu izni topluluk kapsamlı bir satır olarak tutar). Aynı
/// kişinin bekleyen ikinci başvurusu [`Error::Conflict`] — kısmi `UNIQUE`
/// index çakışır, kuyruk şişmez.
///
/// # Errors
/// Topluluk yoksa/kapalıysa [`Error::NotFound`]; topluluk public ise, çağıran
/// zaten üye ise ya da gerekçe geçersizse [`Error::Validation`]; çağıran
/// banlıysa [`Error::Banned`]; bekleyen başvuru varsa [`Error::Conflict`];
/// veritabanı hatası [`Error::Database`].
pub async fn apply_to_community(
    pool: &PgPool,
    actor_id: i64,
    community_name: &str,
    reason: &str,
) -> Result<()> {
    let lookup = lookup_community(pool, community_name).await?;

    if lookup.visibility != CommunityVisibility::Private {
        return Err(Error::Validation(
            "public communities join instantly; applications are for private communities"
                .to_owned(),
        ));
    }

    let reason = validate_reason(reason)?;
    let name = text::normalize_text(community_name);

    let mut tx = pool.begin().await?;

    if is_member_in(&mut *tx, lookup.id, actor_id).await? {
        return Err(Error::Validation(
            "a member cannot apply to their own community".to_owned(),
        ));
    }

    if crate::moderation::is_banned_from_community_in(&mut *tx, lookup.id, actor_id).await? {
        return Err(Error::Banned);
    }

    let inserted = sqlx::query!(
        r#"
        INSERT INTO community_applications (community_id, applicant_actor_id, reason)
        VALUES ($1, $2, $3)
        "#,
        lookup.id,
        actor_id,
        reason,
    )
    .execute(&mut *tx)
    .await;

    if let Err(sqlx::Error::Database(db_err)) = inserted {
        if db_err.is_unique_violation() {
            return Err(Error::Conflict(
                "an application is already pending for this actor".to_owned(),
            ));
        }
        return Err(Error::from(sqlx::Error::Database(db_err)));
    }

    // `member.approve` topluluk kapsamlı bir izin (`ck_permissions_community_only`),
    // dolayısıyla global bir atama burada yok. Sahip de bu satırı tutar.
    let approvers = sqlx::query_scalar!(
        r#"
        SELECT actor_id FROM permissions
        WHERE permission = 'member.approve'::permission
          AND scope = 'community'::permission_scope
          AND community_id = $1
        "#,
        lookup.id,
    )
    .fetch_all(&mut *tx)
    .await?;

    for approver_id in approvers {
        crate::notification::create_notification(
            &mut tx,
            approver_id,
            crate::notification::NotificationKind::CommunityApplication,
            Some(actor_id),
            "community",
            lookup.id,
            serde_json::json!({ "community": name }),
        )
        .await?;
    }

    tx.commit().await?;

    Ok(())
}

/// `GET /communities/{name}/applications?status=&cursor=&limit=`: başvuru
/// kuyruğu, **en eski önce** (bir iş kuyruğu, şikayet listesindeki gibi §3).
///
/// Yetki [`crate::authz::has_for`] ile `member.approve`, hedef topluluğun
/// kapsamında. `status` verilmezse her durum listelenir.
///
/// # Errors
/// Topluluk yoksa/kapalıysa [`Error::NotFound`]; çağıranın `member.approve`
/// yetkisi yoksa [`Error::Forbidden`]; cursor bu listenin sıralamasına ait
/// değilse [`Error::InvalidCursor`]; veritabanı hatası [`Error::Database`].
pub async fn list_applications(
    pool: &PgPool,
    _actor_id: i64,
    permissions: &[Grant],
    community_name: &str,
    status: Option<ApplicationStatus>,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Application>> {
    let lookup = lookup_community(pool, community_name).await?;

    if !crate::authz::has_for(permissions, Permission::MemberApprove, Some(lookup.id)) {
        return Err(Error::Forbidden);
    }

    let (cursor_created_at, cursor_id) = split_new_cursor(cursor);
    let community_id = lookup.id;

    // Kuyruk **artan** sırada (`created_at ASC, id ASC`) — üye listesindeki
    // gerekçenin aynısı: cursor mekanizması yön bilmez, SQL karşılaştırması
    // bu yüzden burada açıkça `>` yazılıyor.
    let rows = sqlx::query_as!(
        ApplicationRow,
        r#"
        SELECT
            applications.id,
            communities.id AS community_id,
            communities.name AS community_name,
            actors.id AS applicant_id,
            actors.username AS applicant_username,
            actors.actor_type AS "applicant_actor_type: ActorType",
            actors.display_name AS applicant_display_name,
            actors.bio AS applicant_bio,
            actors.created_at AS applicant_created_at,
            applications.reason,
            applications.status AS "status: ApplicationStatus",
            applications.created_at,
            applications.resolved_at
        FROM community_applications AS applications
        JOIN communities ON communities.id = applications.community_id
        JOIN actors ON actors.id = applications.applicant_actor_id
        WHERE applications.community_id = $1
          AND ($4::application_status IS NULL OR applications.status = $4::application_status)
          AND (
              $2::timestamptz IS NULL
              OR (applications.created_at, applications.id) > ($2::timestamptz, $3::bigint)
          )
        ORDER BY applications.created_at ASC, applications.id ASC
        LIMIT $5
        "#,
        community_id,
        cursor_created_at,
        cursor_id,
        status as Option<ApplicationStatus>,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |row: &ApplicationRow| row.id,
        |row: &ApplicationRow| SortKey::New {
            created_at: row.created_at,
        },
        Application::from,
    ))
}

/// `POST /communities/{name}/applications/{id}/accept|reject`: başvuruyu
/// sonuçlandırır.
///
/// Yetki `member.approve`, hedef topluluğun kapsamında. Başvuru **o topluluğa
/// ait ve hâlâ `pending`** olmalı ([`Error::NotFound`] / [`Error::Conflict`]).
///
/// `accept` ise üyelik `ON CONFLICT DO NOTHING` ile kurulur; her iki dalda da
/// satır `accepted`/`rejected`, `resolved_by` ve `resolved_at` ile kapatılır
/// (şemadaki çözüm şekli CHECK'i üçünü bağlar). Başvurana
/// `community_application_result` bildirimi gider,
/// `payload = {"community": ..., "accepted": bool}`.
///
/// Üyelik + durum + bildirim **tek transaction'da**.
///
/// # Errors
/// Topluluk yoksa/kapalıysa [`Error::NotFound`]; çağıranın `member.approve`
/// yetkisi yoksa [`Error::Forbidden`]; başvuru bu toplulukta yoksa
/// [`Error::NotFound`]; başvuru artık pending değilse [`Error::Conflict`];
/// veritabanı hatası [`Error::Database`].
pub async fn resolve_application(
    pool: &PgPool,
    actor_id: i64,
    permissions: &[Grant],
    community_name: &str,
    application_id: i64,
    accept: bool,
) -> Result<()> {
    let lookup = lookup_community(pool, community_name).await?;

    if !crate::authz::has_for(permissions, Permission::MemberApprove, Some(lookup.id)) {
        return Err(Error::Forbidden);
    }

    let name = text::normalize_text(community_name);

    let mut tx = pool.begin().await?;

    let application = sqlx::query!(
        r#"
        SELECT applicant_actor_id, status AS "status: ApplicationStatus"
        FROM community_applications
        WHERE id = $1 AND community_id = $2
        FOR UPDATE
        "#,
        application_id,
        lookup.id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("application"))?;

    if application.status != ApplicationStatus::Pending {
        return Err(Error::Conflict(
            "application is no longer pending".to_owned(),
        ));
    }

    if accept {
        sqlx::query!(
            r#"
            INSERT INTO community_members (community_id, actor_id)
            VALUES ($1, $2)
            ON CONFLICT DO NOTHING
            "#,
            lookup.id,
            application.applicant_actor_id,
        )
        .execute(&mut *tx)
        .await?;
    }

    let status = if accept {
        ApplicationStatus::Accepted
    } else {
        ApplicationStatus::Rejected
    };

    sqlx::query!(
        r#"
        UPDATE community_applications
        SET status = $2, resolved_by = $3, resolved_at = now()
        WHERE id = $1
        "#,
        application_id,
        status as ApplicationStatus,
        actor_id,
    )
    .execute(&mut *tx)
    .await?;

    crate::notification::create_notification(
        &mut tx,
        application.applicant_actor_id,
        crate::notification::NotificationKind::CommunityApplicationResult,
        Some(actor_id),
        "community",
        lookup.id,
        serde_json::json!({ "community": name, "accepted": accept }),
    )
    .await?;

    tx.commit().await?;

    Ok(())
}
