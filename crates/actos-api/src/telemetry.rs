//! Loglama ve istek kimliği.

use http::{HeaderValue, Request};
use tower_http::request_id::{MakeRequestId, RequestId};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// `tracing`'i kur.
///
/// `LOG_FORMAT=json` ise makine-okunur çıktı (üretim), aksi halde okunabilir
/// çıktı (geliştirme). Seviye `RUST_LOG` ile ayarlanır.
pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("actos_api=info,actos_core=info,warn"));

    let json = std::env::var("LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));

    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry
            .with(fmt::layer().json().flatten_event(true))
            .init();
    } else {
        registry.with(fmt::layer().compact()).init();
    }
}

/// Her isteğe UUIDv7 kimlik üretir.
///
/// v4 yerine v7: zaman sıralı olduğu için log'larda ve veritabanı
/// index'lerinde ardışık gelir, karşılaştırılabilir.
#[derive(Clone, Copy, Default)]
pub struct MakeRequestUuidV7;

impl MakeRequestId for MakeRequestUuidV7 {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = uuid::Uuid::now_v7().to_string();
        HeaderValue::from_str(&id).ok().map(RequestId::new)
    }
}

/// İçinde bulunulan isteğin kimliği — hata yanıtlarına eklemek için.
pub const REQUEST_ID_HEADER: &str = "x-request-id";
