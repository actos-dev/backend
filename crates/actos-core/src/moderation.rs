//! Moderasyon: şikayetler, ban'ler, rol yönetimi ve denetim izi.
//!
//! ## Denetim izi neden çağırana bırakılmıyor
//!
//! PLAN.md Faz 14: "Her admin eylemi otomatik `admin_actions_log`'a —
//! middleware/helper ile, elle yazmaya bırakılmaz (unutulur)."
//!
//! Bu modül o kuralı **fonksiyonların içine** koyarak uyguluyor: her admin
//! işlemi kendi kaydını kendi transaction'ında yazıyor
//! ([`log_action`]). HTTP middleware'i tercih edilmedi çünkü middleware
//! yalnızca "şu yola şu metotla istek geldi"yi bilir; hangi hedefe ne
//! yapıldığını (ban gerekçesi, silinen içeriğin id'si, verilen rol) bilmez
//! ve yanıt üretilmeden önce log yazamaz. Kaydı işlemin kendi
//! transaction'ında tutmak ayrıca **atomiklik** kazandırıyor: eylem
//! gerçekleştiyse kaydı da var, olmadıysa kaydı da yok.
//!
//! `admin_actions_log` append-only (`migrations/0015`'teki
//! `forbid_mutation` trigger'ı `UPDATE`/`DELETE`'i reddediyor) — yani bir
//! moderatör kendi izini silemez.
//!
//! ## Ban semantiği
//!
//! Ban kimlik doğrulamayı düşürmüyor; `crate::auth::AuthenticatedActor`
//! yalnızca `banned` bayrağını taşıyor ve yazma engelini HTTP katmanı
//! uyguluyor (bkz. o alanın dokümantasyonu). Böylece banlı bir actor
//! okumaya devam edebiliyor — PLAN.md Faz 14'ün kararı.

use chrono::{DateTime, Utc};
use sqlx::{Connection as _, PgConnection, PgPool};

use crate::{
    actor::{Page, paginate},
    auth::{Grant, Permission, PermissionScope},
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    notification::{self, NotificationKind},
    text,
};

/// Toplu "banla ve içeriği sil" işlerini işleyen periyodik işin PostgreSQL
/// advisory lock anahtarı.
///
/// Sabit ve bu işe özel — `crate::tag::cleanup_unused` /
/// `crate::feed::recompute_hot_scores` ile aynı desen. Değer keyfi ama
/// **değiştirilmemeli**: çalışan eski bir instance ile yeni bir instance
/// farklı anahtar kullanırsa kilit amacını yitirir.
const MODERATION_JOBS_ADVISORY_LOCK_KEY: i64 = 0x0AC7_0300;

/// [`run_pending_jobs`]'ın tek turda işleyeceği azami iş sayısı.
///
/// Küçük tutuluyor: kuyruk bir ban'ın ardından birikmiş işleri taşır, tek
/// bir turun süresi periyodik aralığın (varsayılan 60 sn) çok altında
/// kalmalı. Kalan işler sonraki turlarda işlenir.
const MODERATION_JOBS_BATCH: i64 = 100;

/// Şikayet edilebilen hedef türü (`migrations/0014` → `report_target_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "report_target_type", rename_all = "snake_case")]
pub enum ReportTargetType {
    Post,
    Comment,
}

/// Şikayetin durumu (`migrations/0014` → `report_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "report_status", rename_all = "snake_case")]
pub enum ReportStatus {
    Pending,
    Resolved,
    Dismissed,
}

impl ReportStatus {
    /// `?status=` query parametresini ayrıştırır.
    ///
    /// # Errors
    /// Tanınmayan değer [`Error::Validation`].
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "resolved" => Ok(Self::Resolved),
            "dismissed" => Ok(Self::Dismissed),
            other => Err(Error::Validation(format!(
                "invalid status: \"{other}\" (expected: pending, resolved, dismissed)"
            ))),
        }
    }
}

/// Bir şikayet kaydı.
#[derive(Debug, Clone)]
pub struct Report {
    pub id: i64,
    pub reporter_actor_id: i64,
    pub target_type: ReportTargetType,
    pub target_id: i64,
    pub reason: String,
    pub status: ReportStatus,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_by: Option<i64>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// Şikayet edilen içeriğin topluluğunun adı; bağımsız içerikte `None`.
    ///
    /// Kimlik değil **ad** taşınıyor: yanıt DTO'su (`ReportSummary`) da adı
    /// gösteriyor ve bu alanın tüketicisi yalnızca o. Topluluk kapsamlı
    /// yetki kontrolü iç id ile yapılıyor, o yüzden [`list_reports`] /
    /// [`update_report`] id'yi ayrıca sorguluyor.
    pub community: Option<String>,
}

/// Bir denetim izi kaydı.
#[derive(Debug, Clone)]
pub struct AdminAction {
    pub id: i64,
    pub admin_actor_id: i64,
    pub admin_username: String,
    pub action_type: String,
    pub target_type: String,
    pub target_id: i64,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Bir ban kaydı.
#[derive(Debug, Clone)]
pub struct Ban {
    pub actor_id: i64,
    pub username: String,
    pub banned_by: i64,
    pub reason: String,
    pub banned_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Ban'ın kapsadığı topluluğun adı; platform geneli ban'de `None`.
    pub community: Option<String>,
}

/// Gerekçe/not alanlarının azami uzunluğu (şemadaki `CHECK`'lerle aynı).
const MAX_REASON_LEN: usize = 1000;

/// Bir gerekçe metnini doğrular.
///
/// Şemadaki `CHECK` zaten koruyor, ama ihlali orada yakalamak istemciye
/// `500` olarak yansırdı — burada `400` üretiliyor (aynı desen:
/// `crate::comment::create_comment`'in derinlik kontrolü).
fn dogrula_gerekce(raw: &str, alan: &str) -> Result<String> {
    let normalized = text::normalize_text(raw);
    if normalized.is_empty() {
        return Err(Error::Validation(format!("{alan} cannot be empty")));
    }
    if normalized.chars().count() > MAX_REASON_LEN {
        return Err(Error::Validation(format!(
            "{alan} can be at most {MAX_REASON_LEN} characters"
        )));
    }
    Ok(normalized)
}

/// Bir admin eylemini denetim izine yazar.
///
/// **Her admin fonksiyonu bunu kendi transaction'ında çağırıyor** — gerekçe
/// modül dokümantasyonunda. `pub(crate)` değil `pub`: ileride bu modül
/// dışında bir admin eylemi eklenirse (ör. Faz 16'da bir doküman
/// yayımlama ucu) aynı ize yazabilsin.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn log_action(
    tx: &mut PgConnection,
    admin_actor_id: i64,
    action_type: &str,
    target_type: &str,
    target_id: i64,
    reason: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO admin_actions_log
            (admin_actor_id, action_type, target_type, target_id, reason)
        VALUES ($1, $2, $3, $4, $5)
        "#,
        admin_actor_id,
        action_type,
        target_type,
        target_id,
        reason,
    )
    .execute(&mut *tx)
    .await?;

    Ok(())
}

// --- Şikayetler -------------------------------------------------------------

/// `POST /reports`: bir post ya da yorumu şikayet et.
///
/// Aynı actor'ün aynı hedefi ikinci kez şikayet etmesi
/// `uq_reports_reporter_target` ile engelli ve burada [`Error::Conflict`]
/// (`409`) olarak dönüyor — kuyruğu tek bir kullanıcının şişirmesini
/// önlüyor.
///
/// **Kendi içeriğini şikayet etmek engelli değil:** anlamsız ama zararsız
/// ve engellemek moderatöre ek bilgi vermiyor. Oy vermenin aksine burada
/// sıralamayı etkileyen bir sinyal yok.
///
/// # Errors
/// Hedef yoksa ya da okuyucuya görünmüyorsa [`Error::NotFound`]; silinmişse
/// [`Error::Gone`]; hedef türü içerikle uyuşmuyorsa ya da gerekçe geçersizse
/// [`Error::Validation`]; aynı şikayet tekrarlanırsa [`Error::Conflict`];
/// veritabanı hatası [`Error::Database`].
pub async fn create_report(
    pool: &PgPool,
    reporter_actor_id: i64,
    target_type: ReportTargetType,
    target_id: i64,
    reason: &str,
    viewer_communities: &[i64],
) -> Result<Report> {
    let reason = dogrula_gerekce(reason, "reason")?;

    // Görünmeyen hedef `404` (Faz 4A): okuyucunun göremediği özel bir
    // topluluktaki içeriği şikayet etmek, ona "burada böyle bir içerik var"
    // bilgisini verirdi.
    let hedef = sqlx::query!(
        r#"
        SELECT contents.content_type::text AS "content_type!",
               contents.deleted_at,
               contents.community_id,
               communities.name::text AS "community_name?"
        FROM contents
        LEFT JOIN communities ON communities.id = contents.community_id
        WHERE contents.id = $1
          AND content_visible_to(contents.community_id, $2::bigint[])
        "#,
        target_id,
        viewer_communities,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("content"))?;

    if hedef.deleted_at.is_some() {
        return Err(Error::Gone("content"));
    }

    // `target_type` ile içeriğin gerçek türü uyuşmalı: bir yorumu "post"
    // diye şikayet etmek moderasyon kuyruğunda yanlış bir bağlam gösterirdi.
    let beklenen = match target_type {
        ReportTargetType::Post => "post",
        ReportTargetType::Comment => "comment",
    };
    if hedef.content_type != beklenen {
        return Err(Error::Validation(format!(
            "target is a {}, not {beklenen}",
            hedef.content_type
        )));
    }

    // Şikayet, içeriğin topluluğunu **kendi satırında** taşır: kuyruk
    // sorgusu içeriğe geri dönüp bakmak zorunda kalsın istemiyoruz ve
    // içerik silinse bile şikayet hangi topluluğa ait olduğunu bilmeli.
    let sonuc = sqlx::query_scalar!(
        r#"
        INSERT INTO reports (reporter_actor_id, target_type, target_id, reason, community_id)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
        "#,
        reporter_actor_id,
        target_type as ReportTargetType,
        target_id,
        reason,
        hedef.community_id,
    )
    .fetch_one(pool)
    .await;

    match sonuc {
        Ok(id) => get_report(pool, id).await,
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Err(Error::Conflict(
            "you have already reported this target".to_owned(),
        )),
        Err(e) => Err(Error::from(e)),
    }
}

/// Bir şikayet satırını topluluğunun adıyla birlikte okur.
///
/// `INSERT ... RETURNING` içine korele alt sorgu yazmak yerine ayrı bir
/// okuma: topluluk adı `LEFT JOIN` istiyor ve okuma yolu [`list_reports`]'un
/// satır şekliyle **birebir aynı** kalıyor, iki kopya sorgu olmuyor.
async fn get_report(pool: &PgPool, id: i64) -> Result<Report> {
    let rapor = sqlx::query_as!(
        Report,
        r#"
        SELECT reports.id,
               reports.reporter_actor_id,
               reports.target_type AS "target_type: ReportTargetType",
               reports.target_id,
               reports.reason,
               reports.status AS "status: ReportStatus",
               reports.notes,
               reports.created_at,
               reports.resolved_by,
               reports.resolved_at,
               communities.name::text AS "community?"
        FROM reports
        LEFT JOIN communities ON communities.id = reports.community_id
        WHERE reports.id = $1
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("report"))?;

    Ok(rapor)
}

/// `GET /admin/reports`: moderasyon kuyruğu, cursor'lu.
///
/// `status` verilirse yalnızca o durumdakiler. Sıralama **en eski önce**
/// (`created_at ASC`): kuyruk bir iş listesi, en uzun bekleyen önce
/// görülmeli — diğer listelerin "en yeni önce" düzeninin bilinçli tersi.
///
/// **Görünürlük kapsama bağlı** (COMMUNITY_PLAN.md §7): global
/// `report.view` her şeyi, topluluk kapsamlı `report.view` yalnızca o
/// topluluğun raporlarını gösterir. Bağımsız içerik raporları
/// (`community_id IS NULL`) yalnızca global görüntüleyene görünür —
/// onların bağlanacağı bir topluluk moderatörü yok.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn list_reports(
    pool: &PgPool,
    permissions: &[Grant],
    status: Option<ReportStatus>,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<Report>> {
    let (cursor_created_at, cursor_id) = match cursor {
        None => (None, None),
        Some(Cursor {
            sort: SortKey::New { created_at },
            id,
        }) => (Some(created_at), Some(id)),
        Some(_) => return Err(Error::InvalidCursor),
    };

    let global = crate::authz::has_global(permissions, Permission::ReportView);
    let visible_communities: Vec<i64> = permissions
        .iter()
        .filter(|g| g.permission == Permission::ReportView && g.scope == PermissionScope::Community)
        .filter_map(|g| g.community_id)
        .collect();

    // Global görüntüleyici değil ve hiçbir toplulukta `report.view` yoksa
    // görünür tek bir satır bile olamaz — sorguyu hiç çalıştırmıyoruz.
    if !global && visible_communities.is_empty() {
        return Ok(Page {
            items: Vec::new(),
            next_cursor: None,
        });
    }

    let rows = sqlx::query_as!(
        Report,
        r#"
        SELECT reports.id,
               reports.reporter_actor_id,
               reports.target_type AS "target_type: ReportTargetType",
               reports.target_id,
               reports.reason,
               reports.status AS "status: ReportStatus",
               reports.notes,
               reports.created_at,
               reports.resolved_by,
               reports.resolved_at,
               communities.name::text AS "community?"
        FROM reports
        LEFT JOIN communities ON communities.id = reports.community_id
        WHERE ($1::report_status IS NULL OR reports.status = $1::report_status)
          AND ($2::boolean OR reports.community_id = ANY($3::bigint[]))
          AND (
              $4::timestamptz IS NULL
              OR (reports.created_at, reports.id) > ($4::timestamptz, $5::bigint)
          )
        ORDER BY reports.created_at ASC, reports.id ASC
        LIMIT $6
        "#,
        status as Option<ReportStatus>,
        global,
        visible_communities.as_slice(),
        cursor_created_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await?;

    Ok(paginate(
        rows,
        limit,
        |r: &Report| r.id,
        |r: &Report| SortKey::New {
            created_at: r.created_at,
        },
        |r| r,
    ))
}

/// `PATCH /admin/reports/{id}`: şikayeti çöz ya da reddet.
///
/// `ck_reports_resolution_shape` şemada `status <> 'pending'` iken
/// `resolved_by`/`resolved_at`'in dolu olmasını zorunlu kılıyor; bu
/// fonksiyon ikisini de kendisi dolduruyor, çağırana bırakmıyor.
/// `pending`'e geri döndürmek ise ikisini temizliyor.
///
/// Yetki, **raporun topluluğunun kapsamında** `report.resolve` için sorulur
/// (COMMUNITY_PLAN.md §7): topluluk moderatörü yalnızca kendi topluluğunun
/// şikayetini sonuçlandırabilir, bağımsız içerik şikayetini yalnızca global
/// yetkili.
///
/// # Errors
/// Şikayet yoksa [`Error::NotFound`]; çağıranın bu kapsamda yetkisi yoksa
/// [`Error::Forbidden`]; not geçersizse [`Error::Validation`]; veritabanı
/// hatası [`Error::Database`].
pub async fn update_report(
    pool: &PgPool,
    admin_actor_id: i64,
    permissions: &[Grant],
    report_id: i64,
    status: ReportStatus,
    notes: Option<&str>,
) -> Result<Report> {
    let notes = notes.map(|n| dogrula_gerekce(n, "notes")).transpose()?;

    // Hedefin topluluğu yetki kontrolünden ÖNCE okunuyor: yetki kapsamı
    // hedefe bağlı, bu yüzden hedefi bilmeden kontrol edemeyiz.
    let hedef_community_id = sqlx::query_scalar!(
        r#"SELECT community_id FROM reports WHERE id = $1"#,
        report_id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("report"))?;

    if !crate::authz::has_for(permissions, Permission::ReportResolve, hedef_community_id) {
        return Err(Error::Forbidden);
    }

    let mut tx = pool.begin().await?;

    let pending = matches!(status, ReportStatus::Pending);

    sqlx::query!(
        r#"
        UPDATE reports
        SET status = $2,
            notes = COALESCE($3, notes),
            -- Açık cast'ler şart: `NULL` dalı sqlx'e tip ipucu vermiyor
            -- ve parametre `text` olarak çıkarılıyor.
            resolved_by = CASE WHEN $4::boolean THEN NULL ELSE $5::bigint END,
            resolved_at = CASE WHEN $4::boolean THEN NULL ELSE now() END
        WHERE id = $1
        "#,
        report_id,
        status as ReportStatus,
        notes.as_deref(),
        pending,
        admin_actor_id,
    )
    .execute(&mut *tx)
    .await?;

    log_action(
        &mut tx,
        admin_actor_id,
        "report_update",
        "report",
        report_id,
        notes.as_deref(),
    )
    .await?;

    tx.commit().await?;

    get_report(pool, report_id).await
}

// --- İçerik moderasyonu -----------------------------------------------------

/// `DELETE /admin/contents/{id}`: moderatör silmesi, gerekçeli.
///
/// `crate::content::delete_post`'tan farkı: tür ayrımı yapmıyor (post da
/// yorum da silinebilir), sahiplik aramıyor (yetki zaten rolden geliyor) ve
/// **gerekçe zorunlu** — denetim izine yazılacak olan o.
///
/// İçeriğin yazarına `moderation_action` bildirimi gider (bkz.
/// `crate::notification` modül dokümantasyonu) — bir moderatörün kendi
/// içeriğini silmesi durumunda `create_notification`'ın kendi-bildirim
/// koruması bu satırı sessizce atlar, burada ayrıca kontrol edilmiyor.
///
/// # Errors
/// İçerik yoksa [`Error::NotFound`]; zaten silinmişse [`Error::Gone`];
/// çağıranın içeriğin topluluğunda `content.delete` yetkisi yoksa
/// [`Error::Forbidden`]; gerekçe geçersizse [`Error::Validation`];
/// veritabanı hatası [`Error::Database`].
pub async fn moderate_delete_content(
    pool: &PgPool,
    admin_actor_id: i64,
    permissions: &[Grant],
    content_id: i64,
    reason: &str,
) -> Result<()> {
    let reason = dogrula_gerekce(reason, "reason")?;

    let mut tx = pool.begin().await?;

    let mevcut = sqlx::query!(
        r#"SELECT actor_id, deleted_at, community_id FROM contents WHERE id = $1 FOR UPDATE"#,
        content_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("content"))?;

    if mevcut.deleted_at.is_some() {
        return Err(Error::Gone("content"));
    }

    // Yetki içeriğin topluluğunun kapsamında (COMMUNITY_PLAN.md §5):
    // topluluk moderatörü yalnızca kendi topluluğunun içeriğini siler,
    // bağımsız içeriği yalnızca global `content.delete`.
    if !crate::authz::has_for(permissions, Permission::ContentDelete, mevcut.community_id) {
        return Err(Error::Forbidden);
    }

    sqlx::query!(
        r#"UPDATE contents SET deleted_at = now() WHERE id = $1"#,
        content_id,
    )
    .execute(&mut *tx)
    .await?;

    log_action(
        &mut tx,
        admin_actor_id,
        "content_delete",
        "content",
        content_id,
        Some(&reason),
    )
    .await?;

    notification::create_notification(
        &mut tx,
        mevcut.actor_id,
        NotificationKind::ModerationAction,
        Some(admin_actor_id),
        "content",
        content_id,
        serde_json::json!({ "action_type": "content_delete", "reason": reason }),
    )
    .await?;

    tx.commit().await?;

    Ok(())
}

// --- Ban'ler ----------------------------------------------------------------

/// `POST /admin/bans`: bir actor'ü banla — platform genelinde ya da tek bir
/// toplulukta (COMMUNITY_PLAN.md §6).
///
/// `expires_at` `None` ise kalıcı. Aynı hedef için ikinci bir ban **çakışma
/// değil güncelleme**: iki kısmi tekillik indeksi (`uq_bans_global` /
/// `uq_bans_community`) bir actor'ün platform geneli tek ban'ı ve her
/// toplulukta tek ban'ı olmasını sağlıyor. Süreyi uzatmak ya da gerekçeyi
/// düzeltmek yeni bir uç gerektirmemeli.
///
/// **Kapsam yetkiyi belirler:** global `member.ban` her yerde, topluluk
/// kapsamlı `member.ban` yalnızca kendi topluluğunda. Topluluk ban'ı
/// üyeliği de siler — ban ileriye dönüktür, üye kalamaz.
///
/// `delete_content` yalnızca topluluk ban'ında anlamlıdır (global ban'da
/// silinecek "o topluluğun içeriği" yoktur); topluluk içeriğinin silinmesi
/// isteği **arka plan kuyruğuna** yazılır, inline yapılmaz (§6).
///
/// # Errors
/// Kullanıcı ya da topluluk yoksa [`Error::NotFound`]; çağıranın bu kapsamda
/// `member.ban` yetkisi yoksa [`Error::Forbidden`]; gerekçe geçersizse,
/// bitiş zamanı geçmişteyse ya da `delete_content` topluluksuz istenmişse
/// [`Error::Validation`]; veritabanı hatası [`Error::Database`].
#[allow(clippy::too_many_arguments)]
pub async fn ban_actor(
    pool: &PgPool,
    admin_actor_id: i64,
    permissions: &[Grant],
    username: &str,
    reason: &str,
    expires_at: Option<DateTime<Utc>>,
    community_id: Option<i64>,
    delete_content: bool,
) -> Result<Ban> {
    let reason = dogrula_gerekce(reason, "reason")?;

    if let Some(bitis) = expires_at
        && bitis <= Utc::now()
    {
        return Err(Error::Validation(
            "ban expiration time must be in the future".to_owned(),
        ));
    }

    if delete_content && community_id.is_none() {
        return Err(Error::Validation(
            "the delete option only applies to a community ban".to_owned(),
        ));
    }

    // Topluluk adı hem yanıt hem denetim izi için gerekli; ayrıca verilen
    // id'nin gerçekten var olduğunu da doğruluyor.
    let community_name: Option<String> = match community_id {
        Some(id) => Some(
            sqlx::query_scalar!(
                r#"SELECT name::text AS "name!" FROM communities WHERE id = $1"#,
                id
            )
            .fetch_optional(pool)
            .await?
            .ok_or(Error::NotFound("community"))?,
        ),
        None => None,
    };

    // Yetki, hedef actor çözülmeden önce soruluyor: yetkisiz bir çağıranın
    // "bu kullanıcı var mı" sorusuna cevap almaması için.
    if !crate::authz::has_for(permissions, Permission::MemberBan, community_id) {
        return Err(Error::Forbidden);
    }

    let normalized = text::normalize_text(username);

    let hedef = sqlx::query!(
        r#"SELECT id, username::text AS "username!" FROM actors WHERE username = $1"#,
        normalized,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(Error::NotFound("actor"))?;

    let mut tx = pool.begin().await?;

    if let Some(community_id) = community_id {
        sqlx::query!(
            r#"DELETE FROM community_members WHERE community_id = $1 AND actor_id = $2"#,
            community_id,
            hedef.id,
        )
        .execute(&mut *tx)
        .await?;
    }

    // İki ayrı `ON CONFLICT` hedefi: kısmi tekillik indeksleri kapsama göre
    // ayrıldığı için tek bir hedef yazılamıyor (bkz.
    // `crate::auth::grant_permission`'daki aynı desen).
    let (banned_at, expires_at) = match community_id {
        Some(community_id) => {
            let row = sqlx::query!(
                r#"
                INSERT INTO bans (actor_id, banned_by, reason, expires_at, community_id)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (community_id, actor_id) WHERE community_id IS NOT NULL
                DO UPDATE SET
                    banned_by = EXCLUDED.banned_by,
                    reason = EXCLUDED.reason,
                    banned_at = now(),
                    expires_at = EXCLUDED.expires_at
                RETURNING banned_at, expires_at
                "#,
                hedef.id,
                admin_actor_id,
                reason,
                expires_at,
                community_id,
            )
            .fetch_one(&mut *tx)
            .await?;
            (row.banned_at, row.expires_at)
        }
        None => {
            let row = sqlx::query!(
                r#"
                INSERT INTO bans (actor_id, banned_by, reason, expires_at)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (actor_id) WHERE community_id IS NULL
                DO UPDATE SET
                    banned_by = EXCLUDED.banned_by,
                    reason = EXCLUDED.reason,
                    banned_at = now(),
                    expires_at = EXCLUDED.expires_at
                RETURNING banned_at, expires_at
                "#,
                hedef.id,
                admin_actor_id,
                reason,
                expires_at,
            )
            .fetch_one(&mut *tx)
            .await?;
            (row.banned_at, row.expires_at)
        }
    };

    log_action(
        &mut tx,
        admin_actor_id,
        "actor_ban",
        "actor",
        hedef.id,
        Some(&reason),
    )
    .await?;

    notification::create_notification(
        &mut tx,
        hedef.id,
        NotificationKind::ModerationAction,
        Some(admin_actor_id),
        "actor",
        hedef.id,
        serde_json::json!({
            "action_type": "actor_ban",
            "reason": reason,
            "community": community_name.clone(),
        }),
    )
    .await?;

    // Banla-ve-sil ayrı bir arka plan işi: kuyruğa yazmak ban'ın kendisini
    // silinecek satır sayısına bağımlı kılmaz (§6).
    if delete_content && let Some(community_id) = community_id {
        sqlx::query!(
            r#"
            INSERT INTO moderation_jobs (kind, community_id, actor_id, requested_by)
            VALUES ('delete_actor_content_in_community'::moderation_job_kind, $1, $2, $3)
            "#,
            community_id,
            hedef.id,
            admin_actor_id,
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(Ban {
        actor_id: hedef.id,
        username: hedef.username,
        banned_by: admin_actor_id,
        reason,
        banned_at,
        expires_at,
        community: community_name,
    })
}

/// `DELETE /admin/bans/{username}`: ban'i kaldır (platform geneli ya da tek
/// topluluk).
///
/// Ban yoksa da başarı dönüyor (idempotent) — `crate::interaction`'daki
/// aynı desen. Ama denetim izine yalnızca gerçekten bir ban kaldırıldıysa
/// yazılıyor: olmayan bir ban'i "kaldırdım" diye kaydetmek izi kirletirdi.
///
/// # Errors
/// Çağıranın bu kapsamda `member.ban` yetkisi yoksa [`Error::Forbidden`];
/// kullanıcı yoksa [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
pub async fn unban_actor(
    pool: &PgPool,
    admin_actor_id: i64,
    permissions: &[Grant],
    username: &str,
    community_id: Option<i64>,
) -> Result<()> {
    if !crate::authz::has_for(permissions, Permission::MemberBan, community_id) {
        return Err(Error::Forbidden);
    }

    let hedef_id = resolve_target_actor(pool, username).await?;

    let mut tx = pool.begin().await?;

    // `IS NOT DISTINCT FROM`: `community_id` parametrik olduğu için `= NULL`
    // hiçbir satırı eşleştirmezdi; bu operatör iki dalı tek sorguda birleştirir.
    let silinen = sqlx::query!(
        r#"
        DELETE FROM bans
        WHERE actor_id = $1 AND community_id IS NOT DISTINCT FROM $2
        "#,
        hedef_id,
        community_id,
    )
    .execute(&mut *tx)
    .await?;

    if silinen.rows_affected() > 0 {
        log_action(
            &mut tx,
            admin_actor_id,
            "actor_unban",
            "actor",
            hedef_id,
            None,
        )
        .await?;
    }

    tx.commit().await?;

    Ok(())
}

/// Bir actor'ün verilen topluluktan banlı olup olmadığı (süresi dolmamış).
///
/// Platform geneli ban'ı **saymaz**: bu fonksiyon yalnızca "topluluğa yazma
/// engeli" sorusunu yanıtlar. Global ban zaten `CurrentActor` extractor'ı
/// tarafından bütün yazma yollarında erken uygulanıyor.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn is_banned_from_community(
    pool: &PgPool,
    community_id: i64,
    actor_id: i64,
) -> Result<bool> {
    is_banned_from_community_in(pool, community_id, actor_id).await
}

/// [`is_banned_from_community`]'in **çağıranın transaction'ında** çalışan
/// hâli; `crate::content::create_post` / `crate::comment::create_comment`'in
/// yazma ile ban kontrolünü ayırmaması için (`resolve_community_id_in`'deki
/// aynı `Executor` gerekçesi).
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub(crate) async fn is_banned_from_community_in<'e, E>(
    executor: E,
    community_id: i64,
    actor_id: i64,
) -> Result<bool>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let banned = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM bans
            WHERE community_id = $1 AND actor_id = $2
              AND (expires_at IS NULL OR expires_at > now())
        ) AS "exists!"
        "#,
        community_id,
        actor_id,
    )
    .fetch_one(executor)
    .await?;

    Ok(banned)
}

// --- Arka plan işleri -------------------------------------------------------

/// Banla-ve-sil işlerini işler; işlenen iş sayısını döner.
///
/// `crate::tag::cleanup_unused` / `crate::feed::recompute_hot_scores` ile
/// aynı advisory lock deseni (ayrı anahtarla): kilit beklemiyor, başkası
/// tutuyorsa tur atlanıyor ve `Ok(0)` dönüyor.
///
/// Her iş **kendi transaction'ında**: içerik silme ile `processed_at`
/// birlikte commit edilir. Bir iş yarıda kalırsa (ör. süreç ölürse)
/// `processed_at` yazılmamış olur ve sonraki tur onu yeniden alır; silme
/// idempotent olduğu için (`deleted_at IS NULL` koşulu) bu güvenli.
///
/// # Errors
/// Veritabanı hatası [`Error::Database`].
pub async fn run_pending_jobs(pool: &PgPool) -> Result<u64> {
    let mut conn = pool.acquire().await?;

    let locked = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_lock($1) AS "locked!""#,
        MODERATION_JOBS_ADVISORY_LOCK_KEY,
    )
    .fetch_one(&mut *conn)
    .await?;

    if !locked {
        tracing::debug!("moderasyon işleri başka bir instance'da işleniyor, bu tur atlandı");
        return Ok(0);
    }

    // İş yalnızca bu bağlantıda tutulan oturum kapsamlı advisory lock
    // altında çalıştığı için transaction'lar da aynı bağlantıdan açılıyor
    // (bkz. `crate::tag::cleanup_unused`'daki aynı gerekçe).
    let outcome = async {
        let jobs = sqlx::query!(
            r#"
            SELECT id, community_id, actor_id
            FROM moderation_jobs
            WHERE processed_at IS NULL
            ORDER BY id ASC
            LIMIT $1
            "#,
            MODERATION_JOBS_BATCH,
        )
        .fetch_all(&mut *conn)
        .await?;

        let mut processed: u64 = 0;

        for job in jobs {
            let mut tx = (*conn).begin().await?;

            sqlx::query!(
                r#"
                UPDATE contents
                SET deleted_at = now()
                WHERE actor_id = $1 AND community_id = $2 AND deleted_at IS NULL
                "#,
                job.actor_id,
                job.community_id,
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query!(
                r#"UPDATE moderation_jobs SET processed_at = now() WHERE id = $1"#,
                job.id,
            )
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            processed += 1;
        }

        Ok::<u64, Error>(processed)
    }
    .await;

    // Kilit her durumda bırakılmalı — iş hata verse bile (bkz.
    // `crate::tag::cleanup_unused`'daki aynı gerekçe).
    if let Err(err) = sqlx::query_scalar!(
        r#"SELECT pg_advisory_unlock($1) AS "unlocked!""#,
        MODERATION_JOBS_ADVISORY_LOCK_KEY,
    )
    .fetch_one(&mut *conn)
    .await
    {
        tracing::warn!(error = %err, "moderasyon işleri advisory lock'ı bırakılamadı");
    }

    let processed = outcome?;
    if processed > 0 {
        tracing::info!(processed, "moderasyon işleri işlendi");
    }

    Ok(processed)
}

// --- İzinler ----------------------------------------------------------------

/// `PUT /admin/permissions`: bir aktöre izin verir. **`role.grant` gerektirir.**
///
/// Denetim kaydı (`permission_grant`) ve upsert **aynı transaction'da** —
/// bkz. modül dokümantasyonu.
///
/// **Yetki kapsama bağlı** (COMMUNITY_PLAN.md §5): global `role.grant`
/// her kapsamda, topluluk kapsamlı `role.grant` yalnızca kendi
/// topluluğunda izin verebilir. Global bir izin vermek global `role.grant`
/// gerektirir — topluluk moderatörü platformu yönetemez.
///
/// **Kendi iznini değiştirmek engelli:** son `role.grant` sahibinin kendi
/// yetkisini kazara alması sistemi yönetilemez bırakırdı ve geri almanın API
/// üzerinden bir yolu olmazdı (`bin/seed` ile veritabanına elle girmek
/// gerekirdi).
///
/// # Errors
/// Kullanıcı ya da topluluk yoksa [`Error::NotFound`]; çağıranın bu kapsamda
/// `role.grant` yetkisi yoksa [`Error::Forbidden`]; çağıran kendini hedef
/// alıyorsa, izin topluluk-kapsamlı değilken topluluk kapsamı isteniyorsa
/// (ya da tersi) [`Error::Validation`]; veritabanı hatası
/// [`Error::Database`].
pub async fn grant_permission(
    pool: &PgPool,
    granter_actor_id: i64,
    granter_permissions: &[Grant],
    username: &str,
    permission: Permission,
    scope: PermissionScope,
    community_id: Option<i64>,
) -> Result<()> {
    let hedef_id = resolve_target_actor(pool, username).await?;

    if hedef_id == granter_actor_id {
        return Err(Error::Validation(
            "you cannot change your own permissions".to_owned(),
        ));
    }

    dogrula_kapsam(permission, scope, community_id)?;

    if !crate::authz::has_for(granter_permissions, Permission::RoleGrant, community_id) {
        return Err(Error::Forbidden);
    }

    if let Some(community_id) = community_id {
        let var = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM communities WHERE id = $1) AS "exists!""#,
            community_id,
        )
        .fetch_one(pool)
        .await?;
        if !var {
            return Err(Error::NotFound("community"));
        }
    }

    let mut tx = pool.begin().await?;

    crate::auth::grant_permission_in_tx(
        &mut tx,
        hedef_id,
        permission,
        scope,
        community_id,
        Some(granter_actor_id),
    )
    .await?;

    log_action(
        &mut tx,
        granter_actor_id,
        "permission_grant",
        "actor",
        hedef_id,
        Some(permission.as_str()),
    )
    .await?;

    tx.commit().await?;

    Ok(())
}

/// `DELETE /admin/permissions`: bir aktörden izni kaldırır.
///
/// Idempotent: zaten olmayan bir izni kaldırmak hata değildir ve denetim
/// izine **yalnızca gerçekten bir satır silindiyse** kayıt yazılır (aynı
/// `unban_actor` deseni).
///
/// # Errors
/// [`grant_permission`] ile aynı.
pub async fn revoke_permission(
    pool: &PgPool,
    revoker_actor_id: i64,
    revoker_permissions: &[Grant],
    username: &str,
    permission: Permission,
    scope: PermissionScope,
    community_id: Option<i64>,
) -> Result<bool> {
    let hedef_id = resolve_target_actor(pool, username).await?;

    if hedef_id == revoker_actor_id {
        return Err(Error::Validation(
            "you cannot change your own permissions".to_owned(),
        ));
    }

    dogrula_kapsam(permission, scope, community_id)?;

    if !crate::authz::has_for(revoker_permissions, Permission::RoleGrant, community_id) {
        return Err(Error::Forbidden);
    }

    let mut tx = pool.begin().await?;

    let removed =
        crate::auth::revoke_permission_in_tx(&mut tx, hedef_id, permission, scope, community_id)
            .await?;

    if removed {
        log_action(
            &mut tx,
            revoker_actor_id,
            "permission_revoke",
            "actor",
            hedef_id,
            Some(permission.as_str()),
        )
        .await?;
    }

    tx.commit().await?;

    Ok(removed)
}

/// Kullanıcı adını hedef actor id'sine çözer.
///
/// `pub(crate)`: `crate::community::kick_member` aynı çözümü kullanıyor.
pub(crate) async fn resolve_target_actor(pool: &PgPool, username: &str) -> Result<i64> {
    let normalized = text::normalize_text(username);
    sqlx::query_scalar!(r#"SELECT id FROM actors WHERE username = $1"#, normalized)
        .fetch_optional(pool)
        .await?
        .ok_or(Error::NotFound("actor"))
}

/// Kapsam ile iznin uyumunu uygulama katmanında doğrular.
///
/// Şemadaki `ck_permissions_*` kısıtları aynı kuralları zorlar; burada
/// kontrol edilmesinin sebebi, ihlalin `500` değil `400` dönmesi (aynı desen:
/// [`dogrula_gerekce`]).
fn dogrula_kapsam(
    permission: Permission,
    scope: PermissionScope,
    community_id: Option<i64>,
) -> Result<()> {
    match (scope, community_id) {
        (PermissionScope::Global, Some(_)) => {
            return Err(Error::Validation(
                "a global permission cannot carry a community".to_owned(),
            ));
        }
        (PermissionScope::Community, None) => {
            return Err(Error::Validation(
                "a community permission requires a community".to_owned(),
            ));
        }
        _ => {}
    }

    if permission.is_community_only() && scope != PermissionScope::Community {
        return Err(Error::Validation(format!(
            "{} is only valid at community scope",
            permission.as_str()
        )));
    }

    if permission == Permission::AuditView && scope != PermissionScope::Global {
        return Err(Error::Validation(
            "audit.view is only valid at global scope".to_owned(),
        ));
    }

    Ok(())
}

// --- Denetim izi ------------------------------------------------------------

/// `GET /admin/actions`: denetim izi, en yeni önce, cursor'lu.
///
/// Admin'in kullanıcı adı `JOIN` ile getiriliyor: izi okuyan kişi ham id
/// yerine kimin yaptığını görmeli.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn list_actions(
    pool: &PgPool,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page<AdminAction>> {
    let (cursor_created_at, cursor_id) = match cursor {
        None => (None, None),
        Some(Cursor {
            sort: SortKey::New { created_at },
            id,
        }) => (Some(created_at), Some(id)),
        Some(_) => return Err(Error::InvalidCursor),
    };

    let rows = sqlx::query_as!(
        AdminAction,
        r#"
        SELECT log.id, log.admin_actor_id,
               actors.username::text AS "admin_username!",
               log.action_type, log.target_type, log.target_id,
               log.reason, log.created_at
        FROM admin_actions_log AS log
        JOIN actors ON actors.id = log.admin_actor_id
        WHERE (
            $1::timestamptz IS NULL
            OR (log.created_at, log.id) < ($1::timestamptz, $2::bigint)
        )
        ORDER BY log.created_at DESC, log.id DESC
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
        |a: &AdminAction| a.id,
        |a: &AdminAction| SortKey::New {
            created_at: a.created_at,
        },
        |a| a,
    ))
}
