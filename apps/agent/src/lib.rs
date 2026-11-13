//! Vessel sync agent: a zero-knowledge server for encrypted records, wrapped keys and grants.
//!
//! It never holds a key that can decrypt user data. It authenticates users with OPAQUE, enforces
//! vault permissions, validates signed membership history and grants, orders record writes per
//! vault and pushes change notifications over WebSocket.

mod accounts;
mod auth;
mod config;
mod db;
mod error;
mod routes;
mod state;

use std::net::SocketAddr;

pub use config::Config;
use tokio::net::TcpListener;

/// Builds the application and serves it until the process is stopped.
pub async fn serve(config: Config, listener: TcpListener) -> anyhow::Result<()> {
    let state = state::AppState::open(config)?;
    let app = routes::router(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Starts an agent on an ephemeral port in the background. Used by tests and local development.
pub async fn spawn_for_tests(config: Config) -> anyhow::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let state = state::AppState::open(config)?;
    let app = routes::router(state);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Ok(addr)
}
