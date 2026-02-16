use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures::stream::StreamExt;
use futures::SinkExt;
use tracing::{info, warn};

use crate::state::SharedState;

/// WebSocket upgrade handler.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: SharedState) {
    let session_id = state.create_session();
    info!("new WebSocket connection: session={}", session_id);

    let (mut sender, mut receiver) = socket.split();

    // Send welcome message
    let welcome = serde_json::json!({
        "type": "connected",
        "session_id": session_id,
    });
    if sender
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Message loop
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                info!("received message: session={}, len={}", session_id, text.len());
                // TODO: Route to agent runtime
                let echo = serde_json::json!({
                    "type": "message",
                    "session_id": session_id,
                    "content": format!("echo: {}", text),
                });
                if sender
                    .send(Message::Text(echo.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket closed: session={}", session_id);
                break;
            }
            Err(e) => {
                warn!("WebSocket error: session={}, error={}", session_id, e);
                break;
            }
            _ => {}
        }
    }

    state.sessions.remove(&session_id);
    info!("session cleaned up: {}", session_id);
}
