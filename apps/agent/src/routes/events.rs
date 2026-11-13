//! `GET /v1/events`: WebSocket with change notifications for the authenticated user.

use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;
use vessel_protocol::ServerEvent;

use axum::response::IntoResponse;
use rusqlite::params;

use crate::auth::Auth;
use crate::db::now;
use crate::state::{AppState, SocketSlot};

/// How often an open socket checks that its session still exists (logout, password change,
/// recovery and device revocation end it).
const SESSION_CHECK: Duration = Duration::from_secs(30);

pub async fn ws(State(state): State<AppState>, auth: Auth, upgrade: WebSocketUpgrade) -> Response {
    let slot = match state.socket_slot(auth.user_id) {
        Ok(slot) => slot,
        Err(e) => return e.into_response(),
    };
    upgrade.on_upgrade(move |socket| run(state, auth, socket, slot))
}

async fn session_alive(state: &AppState, auth: &Auth) -> bool {
    let (user, device) = (auth.user_id, auth.device_id);
    state
        .db(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions s JOIN devices d ON d.id = s.device_id
                 WHERE s.user_id = ?1 AND s.device_id = ?2 AND s.refresh_expires > ?3 AND d.revoked_at IS NULL)",
                params![user.as_bytes().as_slice(), device.as_bytes().as_slice(), now()],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .await
        .unwrap_or(false)
}

async fn run(state: AppState, auth: Auth, mut socket: WebSocket, _slot: SocketSlot) {
    let mut events = state.events.subscribe();
    let mut ping = tokio::time::interval(Duration::from_secs(25));
    let mut check = tokio::time::interval(SESSION_CHECK);
    check.tick().await;
    loop {
        tokio::select! {
            _ = check.tick() => {
                if !session_alive(&state, &auth).await {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            }
            received = events.recv() => match received {
                Ok(n) if n.users.contains(&auth.user_id) => {
                    if let ServerEvent::DeviceRevoked { device_id } = &n.event
                        && *device_id != auth.device_id
                    {
                        continue;
                    }
                    let Ok(text) = serde_json::to_string(&n.event) else { continue };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                    if matches!(n.event, ServerEvent::DeviceRevoked { .. }) {
                        break;
                    }
                }
                Ok(_) => {}
                // Missed events: clients pull on reconnect, so tell them to resync everything.
                Err(RecvError::Lagged(_)) => break,
                Err(RecvError::Closed) => break,
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}
