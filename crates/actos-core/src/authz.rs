//! Kapsamlı izin kontrolü: "bu aktör bu izni, bu kapsamda tutuyor mu?"
//!
//! Tek merkez, tek kural. HTTP katmanı (`actos-api`) ile domain katmanı
//! (ör. [`crate::content::delete_post`]) aynı fonksiyonları çağırır; yeni bir
//! uç eklendiğinde kendi kontrolünü yazmak yerine buradan geçer.
//!
//! ## Kapsam semantiği
//!
//! - Global izin, topluluk kapsamını **kapsar**: platform genelinde
//!   `content.delete` tutan bir aktör her toplulukta da silebilir. Bu,
//!   `COMMUNITY_PLAN.md` §5'in "Not every global admin should be able to
//!   close a community" maddesini ihlal etmez — orada anlatılan şey, global
//!   bir yöneticinin *her izne otomatik sahip olmaması*; sahip olduğu iznin
//!   tüm kapsamlara yayılması beklenen davranıştır.
//! - Topluluk izni yalnızca kendi topluluğunda geçerlidir.

use crate::auth::{AuthenticatedActor, Grant, Permission, PermissionScope};

/// Aktör, izni **platform genelinde** tutuyor mu?
///
/// Topluluk kapsamlı bir atama global sayılmaz: bir moderatörün yalnızca tek
/// bir toplulukta `content.delete` tutması, her içeriği silme yetkisi vermez.
#[must_use]
pub fn has_global(permissions: &[Grant], permission: Permission) -> bool {
    permissions
        .iter()
        .any(|g| g.permission == permission && g.scope == PermissionScope::Global)
}

/// Aktör, izni verilen **toplulukta** tutuyor mu?
///
/// Global atama her topluluğu kapsar; topluluk ataması yalnızca eşleşen
/// `community_id` için geçerlidir.
#[must_use]
pub fn has_community(permissions: &[Grant], permission: Permission, community_id: i64) -> bool {
    permissions.iter().any(|g| {
        g.permission == permission
            && match g.scope {
                PermissionScope::Global => true,
                PermissionScope::Community => g.community_id == Some(community_id),
            }
    })
}

/// [`has_global`]'in `AuthenticatedActor` kolaylığı.
#[must_use]
pub fn actor_has_global(actor: &AuthenticatedActor, permission: Permission) -> bool {
    has_global(&actor.permissions, permission)
}

/// [`has_community`]'nin `AuthenticatedActor` kolaylığı.
#[must_use]
pub fn actor_has_community(
    actor: &AuthenticatedActor,
    permission: Permission,
    community_id: i64,
) -> bool {
    has_community(&actor.permissions, permission, community_id)
}
