//! Router'ın dışında, her isteğe uygulanan HTTP middleware'leri.
//!
//! `tower_http`'nin genel amaçlı katmanları (trace, timeout, cors, ...)
//! `crate::app`'te doğrudan kuruluyor; burada yaşayanlar Actos'a özgü, iki
//! aşamalı kimlik/hız-sınırlama akışının parçaları:
//!
//! 1. [`identity`] — `Authorization` header'ını **bir kez** çözer, sonucu
//!    request extension'ına koyar.
//! 2. [`ratelimit`] — o kimliğe (ya da IP'ye) göre hız sınırlama kararı
//!    verir, `X-RateLimit-*`/`Retry-After` header'larını **her yanıta**
//!    ekler.
//!
//! Sıra önemli: `ratelimit` doğru `Subject`'i (actor mı IP mi) seçebilmek
//! için `identity`'nin sonucuna ihtiyaç duyuyor — bkz. `crate::app`'teki
//! katman sırası yorumu.

pub mod client_ip;
pub mod identity;
pub mod ratelimit;
