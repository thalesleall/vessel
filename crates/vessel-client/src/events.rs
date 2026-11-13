//! Real-time notifications from the agent (WebSocket `/v1/events`).

use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use vessel_protocol::ServerEvent;

use crate::{ClientError, Result};

/// Connects and calls `on_event` for every event until the connection closes.
/// Reconnecting (with a fresh token from `Vessel::event_credentials`) is up to the caller.
pub async fn listen(
    server_url: &str,
    access_token: &str,
    mut on_event: impl FnMut(ServerEvent) + Send,
) -> Result<()> {
    let ws_url = if let Some(rest) = server_url.strip_prefix("https://") {
        format!("wss://{rest}/v1/events")
    } else if let Some(rest) = server_url.strip_prefix("http://") {
        format!("ws://{rest}/v1/events")
    } else {
        return Err(ClientError::invalid(
            "server URL must start with http:// or https://",
        ));
    };
    let mut request = ws_url
        .into_client_request()
        .map_err(|e| ClientError::WebSocket(Box::new(e)))?;
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|e| ClientError::invalid(e.to_string()))?,
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| ClientError::WebSocket(Box::new(e)))?;
    while let Some(message) = socket.next().await {
        match message.map_err(|e| ClientError::WebSocket(Box::new(e)))? {
            Message::Text(text) => match serde_json::from_str::<ServerEvent>(&text) {
                Ok(event) => on_event(event),
                Err(e) => tracing::warn!("unknown server event: {e}"),
            },
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}
