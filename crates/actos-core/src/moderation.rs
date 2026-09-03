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
use sqlx::{PgConnection, PgPool};

use crate::{
    actor::{Page, paginate},
    auth::AdminRole,
    cursor::{Cursor, SortKey},
    error::{Error, Result},
    notification::{self, NotificationKind},
    text,
};

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
/// Hedef yoksa [`Error::NotFound`]; silinmişse [`Error::Gone`]; hedef türü
/// içerikle uyuşmuyorsa ya da gerekçe geçersizse [`Error::Validation`];
/// aynı şikayet tekrarlanırsa [`Error::Conflict`]; veritabanı hatası
/// [`Error::Database`].
pub async fn create_report(
    pool: &PgPool,
    reporter_actor_id: i64,
    target_type: ReportTargetType,
    target_id: i64,
    reason: &str,
) -> Result<Report> {
    let reason = dogrula_gerekce(reason, "reason")?;

    let hedef = sqlx::query!(
        r#"SELECT content_type::text AS "content_type!", deleted_at FROM contents WHERE id = $1"#,
        target_id,
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

    let sonuc = sqlx::query_as!(
        Report,
        r#"
        INSERT INTO reports (reporter_actor_id, target_type, target_id, reason)
        VALUES ($1, $2, $3, $4)
        RETURNING id, reporter_actor_id,
                  target_type AS "target_type: ReportTargetType",
                  target_id, reason,
                  status AS "status: ReportStatus",
                  notes, created_at, resolved_by, resolved_at
        "#,
        reporter_actor_id,
        target_type as ReportTargetType,
        target_id,
        reason,
    )
    .fetch_one(pool)
    .await;

    match sonuc {
        Ok(rapor) => Ok(rapor),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Err(Error::Conflict(
            "you have already reported this target".to_owned(),
        )),
        Err(e) => Err(Error::from(e)),
    }
}

/// `GET /admin/reports`: moderasyon kuyruğu, cursor'lu.
///
/// `status` verilirse yalnızca o durumdakiler. Sıralama **en eski önce**
/// (`created_at ASC`): kuyruk bir iş listesi, en uzun bekleyen önce
/// görülmeli — diğer listelerin "en yeni önce" düzeninin bilinçli tersi.
///
/// # Errors
/// Cursor bu listenin sıralamasına ait değilse [`Error::InvalidCursor`];
/// veritabanı hatası [`Error::Database`].
pub async fn list_reports(
    pool: &PgPool,
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

    let rows = sqlx::query_as!(
        Report,
        r#"
        SELECT id, reporter_actor_id,
               target_type AS "target_type: ReportTargetType",
               target_id, reason,
               status AS "status: ReportStatus",
               notes, created_at, resolved_by, resolved_at
        FROM reports
        WHERE ($1::report_status IS NULL OR status = $1::report_status)
          AND (
              $2::timestamptz IS NULL
              OR (created_at, id) > ($2::timestamptz, $3::bigint)
          )
        ORDER BY created_at ASC, id ASC
        LIMIT $4
        "#,
        status as Option<ReportStatus>,
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
/// # Errors
/// Şikayet yoksa [`Error::NotFound`]; not geçersizse [`Error::Validation`];
/// veritabanı hatası [`Error::Database`].
pub async fn update_report(
    pool: &PgPool,
    admin_actor_id: i64,
    report_id: i64,
    status: ReportStatus,
    notes: Option<&str>,
) -> Result<Report> {
    let notes = notes.map(|n| dogrula_gerekce(n, "notes")).transpose()?;

    let mut tx = pool.begin().await?;

    let pending = matches!(status, ReportStatus::Pending);

    let rapor = sqlx::query_as!(
        Report,
        r#"
        UPDATE reports
        SET status = $2,
            notes = COALESCE($3, notes),
            -- Açık cast'ler şart: `NULL` dalı sqlx'e tip ipucu vermiyor
            -- ve parametre `text` olarak çıkarılıyor.
            resolved_by = CASE WHEN $4::boolean THEN NULL ELSE $5::bigint END,
            resolved_at = CASE WHEN $4::boolean THEN NULL ELSE now() END
        WHERE id = $1
        RETURNING id, reporter_actor_id,
                  target_type AS "target_type: ReportTargetType",
                  target_id, reason,
                  status AS "status: ReportStatus",
                  notes, created_at, resolved_by, resolved_at
        "#,
        report_id,
        status as ReportStatus,
        notes.as_deref(),
        pending,
        admin_actor_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("report"))?;

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

    Ok(rapor)
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
/// gerekçe geçersizse [`Error::Validation`]; veritabanı hatası
/// [`Error::Database`].
pub async fn moderate_delete_content(
    pool: &PgPool,
    admin_actor_id: i64,
    content_id: i64,
    reason: &str,
) -> Result<()> {
    let reason = dogrula_gerekce(reason, "reason")?;

    let mut tx = pool.begin().await?;

    let mevcut = sqlx::query!(
        r#"SELECT actor_id, deleted_at FROM contents WHERE id = $1 FOR UPDATE"#,
        content_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound("content"))?;

    if mevcut.deleted_at.is_some() {
        return Err(Error::Gone("content"));
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

/// `POST /admin/bans`: bir actor'ü banla.
///
/// `expires_at` `None` ise kalıcı. Aynı actor için ikinci bir ban
/// **çakışma değil güncelleme**: `bans` tablosunun PK'sı `actor_id`, yani
/// bir actor'ün aynı anda en fazla bir ban kaydı olabilir. Süreyi uzatmak
/// ya da gerekçeyi düzeltmek yeni bir uç gerektirmemeli.
///
/// Banlanan actor'e `moderation_action` bildirimi gider (bkz.
/// `crate::notification`).
///
/// # Errors
/// Kullanıcı yoksa [`Error::NotFound`]; gerekçe geçersiz ya da bitiş zamanı
/// geçmişteyse [`Error::Validation`]; veritabanı hatası [`Error::Database`].
pub async fn ban_actor(
    pool: &PgPool,
    admin_actor_id: i64,
    username: &str,
    reason: &str,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Ban> {
    let reason = dogrula_gerekce(reason, "reason")?;

    if let Some(bitis) = expires_at
        && bitis <= Utc::now()
    {
        return Err(Error::Validation(
            "ban expiration time must be in the future".to_owned(),
        ));
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

    let ban = sqlx::query!(
        r#"
        INSERT INTO bans (actor_id, banned_by, reason, expires_at)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (actor_id) DO UPDATE
        SET banned_by = EXCLUDED.banned_by,
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
        serde_json::json!({ "action_type": "actor_ban", "reason": reason }),
    )
    .await?;

    tx.commit().await?;

    Ok(Ban {
        actor_id: hedef.id,
        username: hedef.username,
        banned_by: admin_actor_id,
        reason,
        banned_at: ban.banned_at,
        expires_at: ban.expires_at,
    })
}

/// `DELETE /admin/bans/{username}`: ban'i kaldır.
///
/// Ban yoksa da başarı dönüyor (idempotent) — `crate::interaction`'daki
/// aynı desen. Ama denetim izine yalnızca gerçekten bir ban kaldırıldıysa
/// yazılıyor: olmayan bir ban'i "kaldırdım" diye kaydetmek izi kirletirdi.
///
/// # Errors
/// Kullanıcı yoksa [`Error::NotFound`]; veritabanı hatası
/// [`Error::Database`].
pub async fn unban_actor(pool: &PgPool, admin_actor_id: i64, username: &str) -> Result<()> {
    let normalized = text::normalize_text(username);

    let hedef_id: i64 =
        sqlx::query_scalar!(r#"SELECT id FROM actors WHERE username = $1"#, normalized)
            .fetch_optional(pool)
            .await?
            .ok_or(Error::NotFound("actor"))?;

    let mut tx = pool.begin().await?;

    let silinen = sqlx::query!(r#"DELETE FROM bans WHERE actor_id = $1"#, hedef_id)
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

// --- Roller -----------------------------------------------------------------

/// `POST /admin/roles`: rol ver ya da al. **Yalnızca `admin`.**
///
/// `role` `None` ise rol kaldırılır.
///
/// **Kendi rolünü değiştirmek engelli:** son admin'in kendi yetkisini
/// kazara alması sistemi yönetilemez bırakırdı ve bunu geri almanın API
/// üzerinden bir yolu olmazdı (`bin/seed` ile veritabanına elle girmek
/// gerekirdi).
///
/// # Errors
/// Kullanıcı yoksa [`Error::NotFound`]; çağıran kendini hedef alıyorsa
/// [`Error::Validation`]; veritabanı hatası [`Error::Database`].
pub async fn set_role(
    pool: &PgPool,
    admin_actor_id: i64,
    username: &str,
    role: Option<AdminRole>,
) -> Result<()> {
    let normalized = text::normalize_text(username);

    let hedef_id: i64 =
        sqlx::query_scalar!(r#"SELECT id FROM actors WHERE username = $1"#, normalized)
            .fetch_optional(pool)
            .await?
            .ok_or(Error::NotFound("actor"))?;

    if hedef_id == admin_actor_id {
        return Err(Error::Validation(
            "an admin cannot change their own role".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;

    match role {
        Some(rol) => {
            sqlx::query!(
                r#"
                INSERT INTO admin_roles (actor_id, role, granted_by)
                VALUES ($1, $2, $3)
                ON CONFLICT (actor_id) DO UPDATE
                SET role = EXCLUDED.role,
                    granted_by = EXCLUDED.granted_by,
                    granted_at = now()
                "#,
                hedef_id,
                rol as AdminRole,
                admin_actor_id,
            )
            .execute(&mut *tx)
            .await?;

            log_action(
                &mut tx,
                admin_actor_id,
                "role_grant",
                "actor",
                hedef_id,
                None,
            )
            .await?;
        }
        None => {
            sqlx::query!(r#"DELETE FROM admin_roles WHERE actor_id = $1"#, hedef_id)
                .execute(&mut *tx)
                .await?;

            log_action(
                &mut tx,
                admin_actor_id,
                "role_revoke",
                "actor",
                hedef_id,
                None,
            )
            .await?;
        }
    }

    tx.commit().await?;

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
