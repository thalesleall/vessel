//! `GET /v1/events`: WebSocket with change notifications for the authenticated user.

use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;
use vessel_protocol::ServerEvent;

use crate::auth::Auth;
use crate::state::AppState;

pub async fn ws(State(state): State<AppState>, auth: Auth, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| run(state, auth, socket))
}

async fn run(state: AppState, auth: Auth, mut socket: WebSocket) {
    let mut events = state.events.subscribe();
    let mut ping = tokio::time::interval(Duration::from_secs(25));
    loop {
        tokio::select! {
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
