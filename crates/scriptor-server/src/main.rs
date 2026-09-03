//! `scriptor-server` - the collaboration relay binary.
//!
//! Serves the websocket relay on `SCRIPTOR_PORT` (default 8091). Persistence is in-memory unless
//! built with `--features postgres` *and* `DATABASE_URL` is set, in which case Postgres is the
//! system-of-record. `RUST_LOG` controls tracing (e.g. `RUST_LOG=scriptor_server=debug`).

use std::sync::Arc;

use anyhow::Result;
use scriptor_server::persistence::Memory;
use scriptor_server::router;

#[tokio::main]
async fn main() -> Result<()> {
    // Default to a useful level when RUST_LOG is unset, so a bare `cargo run` shows the listening
    // line (and room/relay activity) instead of nothing.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("scriptor_server=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let port: u16 = std::env::var("SCRIPTOR_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8091);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "scriptor-server listening");

    #[cfg(feature = "postgres")]
    if let Ok(url) = std::env::var("DATABASE_URL") {
        let persist = Arc::new(scriptor_server::persistence::Postgres::connect(&url).await?);
        tracing::info!("persistence: postgres");
        axum::serve(listener, router(persist)).await?;
        return Ok(());
    }

    tracing::info!(
        "persistence: in-memory (ephemeral) - build --features postgres + set DATABASE_URL \
         for durability"
    );
    let persist = Arc::new(Memory::new());
    axum::serve(listener, router(persist)).await?;
    Ok(())
}
