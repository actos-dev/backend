//! Actos'un domain katmanı: iş kuralları, yapılandırma ve veritabanı erişimi.
//!
//! Buradaki hiçbir şey axum'u ya da HTTP'yi bilmez. Amaç, aynı mantığın
//! ileride farklı bir taşıma katmanından (gRPC, iş kuyruğu, seed script'i)
//! kullanılabilmesi ve testlerin HTTP kurmadan yazılabilmesi.

pub mod actor;
pub mod attachment;
pub mod auth;
pub mod avatar;
pub mod cache;
pub mod comment;
pub mod config;
pub mod content;
pub mod cursor;
pub mod db;
pub mod error;
pub mod feed;
pub mod id;
pub mod idempotency;
pub mod interaction;
pub mod media;
pub mod moderation;
pub mod notification;
pub mod ratelimit;
pub mod search;
pub mod secret;
pub mod storage;
pub mod tag;
pub mod text;

pub use config::Config;
pub use error::{Error, Result};
pub use storage::Storage;
