//! Periyodik bakım işleri.
//!
//! İki iş var ve ikisi de aynı şekli paylaşıyor: sabit aralıkla koş, hata
//! olursa logla ve devam et, sıfır aralık verilirse hiç başlama. Ortak
//! çalıştırıcı [`spawn_periodic`].
//!
//! **Her iş kendi PostgreSQL advisory lock'ını kendi içinde alıyor**
//! (`actos_core::tag::cleanup_unused`, `actos_core::feed::recompute_hot_scores`),
//! bu yüzden burada birden fazla instance koordinasyonu yok: her instance
//! kendi zamanlayıcısını çalıştırır, kilit aynı anda yalnızca birinin
//! gerçekten iş yapmasını sağlar. Kilidi işin içine koymak, burada
//! tutmaktan iyi: fonksiyon nereden çağrılırsa çağrılsın (testten, ileride
//! bir CLI'dan) koruma birlikte geliyor.
//!
//! Görevler `axum::serve`in graceful shutdown'ına bağlanmıyor. İkisi de
//! kısa tek bir `UPDATE`/`DELETE` çalıştırıyor ve süreç kapanırken tokio
//! runtime'ı ile birlikte düşüyorlar; yarım kalmış bir transaction
//! bırakmıyorlar.

use std::{future::Future, time::Duration};

use sqlx::PgPool;

/// Verilen işi `interval` aralığıyla arka planda çalıştırır.
///
/// `interval` sıfırsa görev **hiç başlatılmaz** — temizliği dışarıdan
/// (cron, elle) yürütmek isteyen bir dağıtım işi kapatabilsin diye.
///
/// İlk tick hemen ateşlenir: uzun süre kapalı kalmış bir dağıtımda
/// birikmiş işin ilk turda görülmesini istiyoruz.
///
/// `is_ad` yalnızca loglarda görünüyor; hangi işin atlandığını/başarısız
/// olduğunu ayırt etmek için.
pub fn spawn_periodic<F, Fut>(is_ad: &'static str, pool: PgPool, interval: Duration, mut is: F)
where
    F: FnMut(PgPool) -> Fut + Send + 'static,
    Fut: Future<Output = Result<u64, actos_core::Error>> + Send,
{
    if interval.is_zero() {
        tracing::info!(is = is_ad, "periyodik iş devre dışı (aralık 0)");
        return;
    }

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(err) = is(pool.clone()).await {
                // Bakım işinin başarısız olması isteklerin doğruluğunu
                // etkilemiyor; sunucu çalışmaya devam etmeli.
                tracing::warn!(is = is_ad, error = %err, "periyodik iş başarısız oldu");
            }
        }
    });
}
