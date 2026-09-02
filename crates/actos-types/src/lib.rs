//! Actos API'sinin paylaşılan tipleri.
//!
//! Bu crate **hiçbir zorunlu** sunucu bağımlılığı içermez (veritabanı, HTTP
//! framework yok) — çünkü aynı tipleri CLI, TUI ve Rust SDK de kullanacak.
//! API'nin şekli değiştiğinde hepsi derleme zamanında kırılır,
//! senkronizasyonu elle takip etmeye gerek kalmaz.
//!
//! ## `openapi` feature'ı — ilke bozulmuyor, opsiyonel kalıyor
//!
//! `actos-api`'nin OpenAPI spec'i (Faz 16) her DTO için bir `utoipa::ToSchema`
//! türetmesi istiyor, ve bu türetme `utoipa`'ya derleme zamanı bağımlılığı
//! gerektiriyor. Bunu bu crate'e koşulsuz eklemek yukarıdaki ilkeyi bozardı:
//! CLI/TUI/SDK gibi HTTP sunucusuyla hiç ilgisi olmayan tüketiciler `utoipa`
//! ve onun proc-macro bağımlılıklarını (`syn`, `quote`, ...) boşuna
//! derlemek zorunda kalırdı.
//!
//! Çözüm: `utoipa` bağımlılığı `optional = true` ve yalnızca bu crate'in
//! `openapi` feature'ı açıkken çekiliyor (bkz. `Cargo.toml`). Her DTO'daki
//! türetme de buna göre koşullu:
//! ```ignore
//! #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
//! ```
//! Feature kapalıyken (varsayılan — CLI/SDK'nın aldığı hâl) bu satır hiçbir
//! iz bırakmıyor, tip yalnızca `serde` ile var oluyor. Yalnızca `actos-api`
//! bu feature'ı açıyor (bkz. `crates/actos-api/Cargo.toml`) — ilke ("hiçbir
//! sunucu bağımlılığı" değil, artık "hiçbir **zorunlu** sunucu bağımlılığı")
//! feature bayrağının kendisiyle korunuyor.

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

/// `GET /search` uçlarının yanıt tipleri.
pub mod search;

/// Etiket uçlarının yanıt tipleri.
pub mod tag;

pub use error::ErrorCode;
