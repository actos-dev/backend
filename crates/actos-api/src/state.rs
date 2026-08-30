//! Handler'ların paylaştığı uygulama durumu.

use std::sync::Arc;

use actos_core::{Config, Storage, id::IdCodec};
use deadpool_redis::Pool as RedisPool;
use sqlx::PgPool;

/// Tüm handler'lara `State` ile geçirilen paylaşılan bağımlılıklar.
///
/// Ucuz klonlanabilir olması gerekiyor (axum her istekte klonlar): içi `Arc`,
/// `PgPool` ve `RedisPool` zaten kendi içlerinde paylaşımlı.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    db: PgPool,
    redis: RedisPool,
    storage: Storage,
    id_codec: IdCodec,
}

impl AppState {
    #[must_use]
    pub fn new(
        config: Config,
        db: PgPool,
        redis: RedisPool,
        storage: Storage,
        id_codec: IdCodec,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                db,
                redis,
                storage,
                id_codec,
            }),
        }
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    #[must_use]
    pub fn db(&self) -> &PgPool {
        &self.inner.db
    }

    #[must_use]
    pub fn redis(&self) -> &RedisPool {
        &self.inner.redis
    }

    #[must_use]
    pub fn storage(&self) -> &Storage {
        &self.inner.storage
    }

    #[must_use]
    pub fn id_codec(&self) -> &IdCodec {
        &self.inner.id_codec
    }
}
