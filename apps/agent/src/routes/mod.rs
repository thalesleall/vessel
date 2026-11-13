mod account;
mod auth;
mod events;
mod invites;
mod records;
mod vaults;

use std::net::{IpAddr, SocketAddr};

use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit};
use axum::http::HeaderMap;
use axum::routing::{delete, get, post, put};
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/server/info", get(account::server_info))
        // Authentication
        .route("/auth/register/start", post(auth::register_start))
        .route("/auth/register/finish", post(auth::register_finish))
        .route("/auth/login/start", post(auth::login_start))
        .route("/auth/login/finish", post(auth::login_finish))
        .route("/auth/refresh", post(auth::refresh))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/recovery/start", post(auth::recovery_start))
        .route("/auth/recovery/finish", post(auth::recovery_finish))
        // Account, devices, users
        .route("/account", get(account::get_account))
        .route("/account/password/start", post(auth::password_start))
        .route("/account/password/finish", post(auth::password_finish))
        .route("/devices", get(account::list_devices))
        .route("/devices/{id}", delete(account::revoke_device))
        .route("/users/lookup", get(account::lookup_user))
        .route("/users/{id}", get(account::get_user))
        // Vaults
        .route("/vaults", get(vaults::list).post(vaults::create))
        .route(
            "/vaults/{id}",
            get(vaults::get).delete(vaults::delete_vault),
        )
        .route("/vaults/{id}/meta", put(vaults::update_meta))
        .route(
            "/vaults/{id}/membership",
            get(vaults::membership).post(vaults::change_membership),
        )
        .route("/vaults/{id}/grants", get(vaults::my_grants))
        .route("/vaults/{id}/rotate/start", post(vaults::rotate_start))
        .route("/vaults/{id}/rotate/finish", post(vaults::rotate_finish))
        .route("/vaults/{id}/invites", post(invites::create_vault_invite))
        .route(
            "/vaults/{id}/changes",
            get(records::pull).post(records::push),
        )
        // Invites
        .route("/admin/invites", post(invites::create_signup_invite))
        .route("/invites/{token}/accept", post(invites::accept))
        // Real time
        .route("/events", get(events::ws));

    Router::new()
        .nest("/v1", api)
        .route("/invite/{token}", get(invites::landing_page))
        .route("/health", get(|| async { "ok" }))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Client IP for rate limiting, honoring `X-Forwarded-For` only when configured to.
pub fn client_ip(state: &AppState, headers: &HeaderMap, peer: &ConnectInfo<SocketAddr>) -> IpAddr {
    if state.config.trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok());
        if let Some(ip) = forwarded {
            return ip;
        }
    }
    peer.0.ip()
}
