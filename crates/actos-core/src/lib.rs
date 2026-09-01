//! Actos'un domain katmanı: iş kuralları, yapılandırma ve veritabanı erişimi.
//!
//! Buradaki hiçbir şey axum'u ya da HTTP'yi bilmez. Amaç, aynı mantığın
//! ileride farklı bir taşıma katmanından (gRPC, iş kuyruğu, seed script'i)
//! kullanılabilmesi ve testlerin HTTP kurmadan yazılabilmesi.

pub mod actor;
pub mod auth;
pub mod cache;
pub mod config;
pub mod content;
pub mod cursor;
pub mod db;
pub mod error;
pub mod id;
pub mod ratelimit;
pub mod secret;
pub mod storage;
pub mod text;

pub use config::Config;
pub use error::{Error, Result};
pub use storage::Storage;
