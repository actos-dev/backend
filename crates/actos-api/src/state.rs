//! Handler'ların paylaştığı uygulama durumu.

use std::sync::Arc;

use actos_core::{Config, Storage, cursor::CursorCodec, id::IdCodec, ratelimit::RateLimiter};
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
    cursor_codec: CursorCodec,
    // `RateLimiter`'ın kendisi ucuz klonlanabilir olmak zorunda değil (bkz.
    // o tip üzerindeki yorum) — burada tek bir örneği `Arc`layıp
    // paylaşıyoruz. `identity`/`ratelimit` middleware'leri fire-and-forget
    // görevlere (`tokio::spawn`) taşımak için ayrıca sahipli bir `Arc`
    // klonuna ihtiyaç duyuyor (bkz. `Self::rate_limiter_handle`).
    rate_limiter: Arc<RateLimiter>,
}

impl AppState {
    #[must_use]
    pub fn new(
        config: Config,
        db: PgPool,
        redis: RedisPool,
        storage: Storage,
        id_codec: IdCodec,
        cursor_codec: CursorCodec,
        rate_limiter: RateLimiter,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                db,
                redis,
                storage,
                id_codec,
                cursor_codec,
                rate_limiter: Arc::new(rate_limiter),
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

    #[must_use]
    pub fn cursor_codec(&self) -> &CursorCodec {
        &self.inner.cursor_codec
    }

    #[must_use]
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.inner.rate_limiter
    }

    /// [`Self::rate_limiter`] ile aynı `RateLimiter`'a sahipli bir tutamaç —
    /// isteği geciktirmemesi gereken `tokio::spawn` görevlerine
    /// taşınabilsin diye (ör. `record_key_use`). `Arc::clone` ucuz.
    #[must_use]
    pub fn rate_limiter_handle(&self) -> Arc<RateLimiter> {
        Arc::clone(&self.inner.rate_limiter)
    }
}
