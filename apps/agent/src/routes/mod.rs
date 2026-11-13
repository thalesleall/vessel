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

/// Unauthenticated and account routes carry small JSON documents: cap them well below the
/// limit record pushes need, so anonymous clients cannot make the agent buffer large bodies.
const AUTH_BODY_LIMIT: usize = 256 * 1024;
/// Authenticated record pushes (up to `MAX_BATCH` records of `MAX_BLOB_LEN`).
const BODY_LIMIT: usize = 64 * 1024 * 1024;

pub fn router(state: AppState) -> Router {
    let auth_routes = Router::new()
        .route("/auth/register/start", post(auth::register_start))
        .route("/auth/register/finish", post(auth::register_finish))
        .route("/auth/login/start", post(auth::login_start))
        .route("/auth/login/finish", post(auth::login_finish))
        .route("/auth/refresh", post(auth::refresh))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/recovery/start", post(auth::recovery_start))
        .route("/auth/recovery/finish", post(auth::recovery_finish))
        .route("/account/password/start", post(auth::password_start))
        .route("/account/password/finish", post(auth::password_finish))
        .layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT));
    let api = Router::new()
        .route("/server/info", get(account::server_info))
        .merge(auth_routes)
        // Account, devices, users
        .route("/account", get(account::get_account))
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
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Client IP for rate limiting, honoring `X-Forwarded-For` only when configured to.
pub fn client_ip(state: &AppState, headers: &HeaderMap, peer: &ConnectInfo<SocketAddr>) -> IpAddr {
    forwarded_ip(state.config.trust_proxy, headers).unwrap_or(peer.0.ip())
}

/// With `hops` trusted proxies, each appends the address it saw: the client is the `hops`-th
/// entry from the right. Entries further left are whatever the client sent.
fn forwarded_ip(hops: usize, headers: &HeaderMap) -> Option<IpAddr> {
    if hops == 0 {
        return None;
    }
    let all: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .collect();
    all.len()
        .checked_sub(hops)
        .and_then(|i| all[i].trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xff(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", v.parse().unwrap());
        h
    }

    #[test]
    fn client_ip_counts_trusted_proxies_from_the_right() {
        let spoofed = xff("6.6.6.6, 203.0.113.7, 198.51.100.1");
        assert_eq!(forwarded_ip(0, &spoofed), None);
        assert_eq!(forwarded_ip(1, &spoofed), "198.51.100.1".parse().ok());
        // A CDN in front of Caddy: the CDN appended the client, Caddy appended the CDN.
        assert_eq!(forwarded_ip(2, &spoofed), "203.0.113.7".parse().ok());
        // Fewer entries than proxies: the header is not trusted.
        assert_eq!(forwarded_ip(4, &spoofed), None);
    }
}
