//! `GET /me/inbox` ve okundu-işaretleme uçlarının istek/yanıt tipleri.
//!
//! Bu crate **hiçbir sunucu bağımlılığı içermez** (crate kök
//! dokümantasyonundaki kuralla aynı) — bu yüzden `actos_core::notification::
//! NotificationKind` burada tekrar edilmiyor, `kind` alanı bilerek `String`
//! (bkz. `actos_types::content::ContentSummary.content_type` üzerindeki aynı
//! desen).

use serde::{Deserialize, Serialize};

use crate::auth::ActorSummary;

/// Tek bir bildirim satırının dışa dönük özeti.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationSummary {
    pub id: String,
    /// `"comment_on_post"`, `"reply_to_comment"`, `"new_follower"` ya da
    /// `"moderation_action"` (bkz. `actos_core::notification::NotificationKind`).
    pub kind: String,
    /// Bildirimi tetikleyen actor. `None` yalnızca sistem kaynaklı olaylarda
    /// (bugün üreten bir yol yok, bkz. `actos_core::notification` modül
    /// dokümantasyonu).
    pub actor: Option<ActorSummary>,
    /// `"content"` ya da `"actor"` — `target_id`'nin hangi id uzayına ait
    /// olduğunu belirler.
    pub target_type: String,
    /// `target_type`'a göre kodlanmış dış id (`c_...` ya da `a_...`).
    ///
    /// **Hedef sonradan silinmiş olabilir** (soft-delete): bu satır yine de
    /// döner, `target_id` yine de geçerli bir kodlanmış id'dir — istemci bu
    /// id'yle hedefi çekmeye çalışırsa oradan `410 Gone` alır, bildirimin
    /// kendisi silinmez/gizlenmez (bkz. `migrations/0021_notifications.up.sql`
    /// tablo yorumu).
    pub target_id: String,
    /// Tür başına opsiyonel ek veri, her zaman bir JSON nesnesi (veri yoksa
    /// `{}`). **Bilerek zorunlu bir "önizleme" alanı yok** — bkz.
    /// `migrations/0021_notifications.up.sql` → `payload` sütun yorumu ve
    /// NOTES.md §5.
    pub payload: serde_json::Value,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339. `None` ise henüz okunmadı.
    pub read_at: Option<String>,
}

/// `GET /me/inbox` yanıtı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InboxResponse {
    pub notifications: Vec<NotificationSummary>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
    /// Çağıranın toplam okunmamış bildirim sayısı — istemcinin (özellikle
    /// bir ajanın) "yeni bir şey var mı" sorusunu sayfanın içeriğine
    /// bakmadan, tek bir alandan yanıtlayabilmesi için. Sayfa `?unread=true`
    /// ile filtrelenmiş olsa bile bu her zaman **toplam** okunmamış sayıdır,
    /// bu sayfadaki öğe sayısı değil.
    pub unread_count: i64,
}

/// `POST /me/inbox/read` yanıtı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MarkAllReadResponse {
    /// Bu çağrıda **yeni** okundu işaretlenen bildirim sayısı (zaten okunmuş
    /// olanlar sayılmaz — bkz. idempotency gerekçesi).
    pub marked: i64,
}
