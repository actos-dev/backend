//! Actos REST API sunucusu.

use std::net::SocketAddr;

use axum::{Router, routing::get};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Faz 2'de burası AppConfig'ten okunacak; şimdilik iskelet.
    let addr = SocketAddr::from(([127, 0, 0, 1], 3100));

    let app = Router::new().route("/health", get(|| async { "ok" }));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("actos-api dinlemede: http://{addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
