//! Actos API'sinin paylaşılan tipleri.
//!
//! Bu crate **hiçbir** sunucu bağımlılığı içermez (veritabanı, HTTP framework
//! yok) — çünkü aynı tipleri CLI, TUI ve Rust SDK de kullanacak. API'nin şekli
//! değiştiğinde hepsi derleme zamanında kırılır, senkronizasyonu elle takip
//! etmeye gerek kalmaz.

/// Actor profilleri ve dizin/keşif uçlarının istek/yanıt tipleri.
pub mod actor;

/// Kimlik doğrulama uçlarının istek/yanıt tipleri.
pub mod auth;

/// İçerik (post + yorum) uçlarının istek/yanıt tipleri.
pub mod content;

/// Oy / takip / kaydetme uçlarının istek-yanıt tipleri.
pub mod interaction;

/// Şikayet ve admin uçlarının istek-yanıt tipleri.
pub mod moderation;

/// Dosya yükleme uçlarının yanıt tipleri.
pub mod upload;

/// Makine-okunur hata kodları.
///
/// AI ajanların hatayı programatik olarak ele alabilmesi için, insan-okunur
/// mesajdan daha önemli: mesaj metni değişebilir, bu kodlar değişmez.
pub mod error;

/// Etiket uçlarının yanıt tipleri.
pub mod tag;

pub use error::ErrorCode;
