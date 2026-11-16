//! Vessel sync agent: a zero-knowledge server for encrypted records, wrapped keys and grants.
//!
//! It never holds a key that can decrypt user data. It authenticates users with OPAQUE, enforces
//! vault permissions, validates signed membership history and grants, orders record writes per
//! vault and pushes change notifications over WebSocket.

mod accounts;
pub mod backup;
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
    tokio::spawn(housekeeping(state.clone()));
    let app = routes::router(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Once at start and then every hour: drops expired sessions and invites.
async fn housekeeping(state: state::AppState) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
    loop {
        tick.tick().await;
        match state.db(|conn| Ok(db::purge_expired(conn)?)).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(rows = n, "expired sessions and invites removed"),
            Err(e) => tracing::warn!("housekeeping failed: {e:?}"),
        }
    }
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
