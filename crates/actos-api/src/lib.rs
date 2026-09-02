//! Actos REST API — kütüphane hedefi.
//!
//! `main.rs` bu modülleri kullanan ince bir kabuk (yapılandırmayı okur,
//! bağımlılıkları kurar, `axum::serve` çağırır). Ayrı bir `lib.rs`
//! olmasının sebebi salt organizasyon değil: `tests/` altındaki entegrasyon
//! testlerinin `app::build`/`state::AppState`'e erişebilmesi için crate'in
//! bir kütüphane hedefi olması gerekiyor — bin hedeflerinin içi `tests/`den
//! görünmez.

pub mod app;
pub mod auth;
pub mod error;
pub mod fields;
pub mod jobs;
pub mod middleware;
/// OpenAPI dokümantasyonunun paylaşılan iskeleti (Faz 16) — bkz. modül
/// dokümantasyonu. `pub(crate)`: yalnızca `crate::routes` ve `routes/*.rs`
/// bu tipleri kullanıyor, crate dışına sızacak bir API sözleşmesi değil.
mod openapi;
pub mod routes;
pub mod state;
pub mod telemetry;
