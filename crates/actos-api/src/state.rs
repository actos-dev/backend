//! Handler'ların paylaştığı uygulama durumu.

use std::sync::Arc;

use actos_core::{
    Config, Storage, cursor::CursorCodec, id::IdCodec, idempotency::IdempotencyStore,
    ratelimit::RateLimiter,
};
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
    // Aynı gerekçeyle `Arc`: `IdempotencyStore` de klonlanabilir olmak
    // zorunda değil, tek örneği `AppState` içinde paylaşılıyor.
    idempotency: Arc<IdempotencyStore>,
}

impl AppState {
    // `AppState::new` bu uygulamanın **tüm** paylaşılan bağımlılıklarını
    // bir araya getiren tek yer — parametre sayısının kendisi bir kod
    // kokusu değil, bu fonksiyonun görevinin doğal sonucu (bkz. `main.rs`
    // ve `tests/*.rs`'teki tek çağıran taraflar: hepsi zaten adlandırılmış
    // yerel değişkenlerden çağırıyor, pozisyonel argüman karışıklığı riski
    // yok). Bunları ayrı bir "builder" ya da ara struct'a bölmek burada
    // gerçek bir okunabilirlik kazancı sağlamadan bir dolaylama katmanı
    // eklerdi.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        config: Config,
        db: PgPool,
        redis: RedisPool,
        storage: Storage,
        id_codec: IdCodec,
        cursor_codec: CursorCodec,
        rate_limiter: RateLimiter,
        idempotency: IdempotencyStore,
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
                idempotency: Arc::new(idempotency),
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

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyStore {
        &self.inner.idempotency
    }

    /// İstek başına bir kez hesaplanan "bu izleyicinin görebildiği
    /// topluluklar" kümesi (Faz 4A) — `actos_core::visibility::
    /// viewer_communities`'in `AppState` kolaylığı.
    ///
    /// Her okuma yolu aynı sorguyu kendi gövdesinde tekrar yazmasın ve bir
    /// handler içinde birden fazla `actos_core` çağrısı varsa (ör. `GET
    /// /comments/{id}` → `get_comment` + `ancestors_of`) küme bir kez
    /// hesaplanıp paylaşılsın diye. Anonim izleyici (`None`) boş küme alır.
    ///
    /// # Errors
    /// Veritabanı hatası [`actos_core::Error::Database`].
    pub async fn viewer_communities(&self, viewer: Option<i64>) -> actos_core::Result<Vec<i64>> {
        actos_core::visibility::viewer_communities(&self.inner.db, viewer).await
    }
}
