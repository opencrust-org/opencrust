use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures::SinkExt;
use futures::stream::StreamExt;
use tracing::{info, warn};

use opencrust_agents::{ChatMessage, ChatRole, MessagePart};

use crate::state::SharedState;

const MAX_WS_FRAME_BYTES: usize = 64 * 1024;
const MAX_WS_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_WS_TEXT_BYTES: usize = 32 * 1024;

/// WebSocket upgrade handler.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    ws.max_frame_size(MAX_WS_FRAME_BYTES)
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state))
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
                let text_len = text.len();
                info!("received message: session={}, len={}", session_id, text_len);
                if text_message_too_large(text_len) {
                    warn!(
                        "dropping oversized ws text message: session={}, len={}, limit={}",
                        session_id, text_len, MAX_WS_TEXT_BYTES
                    );
                    let err = serde_json::json!({
                        "type": "error",
                        "code": "message_too_large",
                        "max_bytes": MAX_WS_TEXT_BYTES,
                    });
                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                    break;
                }
                let user_text = parse_user_text(&text);

                // Snapshot conversation history for this session
                let history: Vec<ChatMessage> = state
                    .sessions
                    .get(&session_id)
                    .map(|s| s.history.clone())
                    .unwrap_or_default();

                // Route through agent runtime
                let reply = match state
                    .agents
                    .process_message(&session_id, &user_text, &history)
                    .await
                {
                    Ok(response_text) => {
                        // Append user + assistant messages to session history
                        if let Some(mut session) = state.sessions.get_mut(&session_id) {
                            session.history.push(ChatMessage {
                                role: ChatRole::User,
                                content: MessagePart::Text(user_text),
                            });
                            session.history.push(ChatMessage {
                                role: ChatRole::Assistant,
                                content: MessagePart::Text(response_text.clone()),
                            });
                        }

                        serde_json::json!({
                            "type": "message",
                            "session_id": session_id,
                            "content": response_text,
                        })
                    }
                    Err(e) => {
                        warn!("agent error: session={}, error={}", session_id, e);
                        serde_json::json!({
                            "type": "error",
                            "session_id": session_id,
                            "code": "agent_error",
                            "message": e.to_string(),
                        })
                    }
                };

                if sender
                    .send(Message::Text(reply.to_string().into()))
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

fn text_message_too_large(len: usize) -> bool {
    len > MAX_WS_TEXT_BYTES
}

/// Try to extract a `"content"` field from JSON, otherwise use the raw text.
fn parse_user_text(raw: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(text) = v.get("content").and_then(|c| c.as_str())
    {
        return text.to_string();
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::{MAX_WS_TEXT_BYTES, text_message_too_large};

    #[test]
    fn text_message_size_guard_uses_strict_upper_bound() {
        assert!(!text_message_too_large(MAX_WS_TEXT_BYTES));
        assert!(text_message_too_large(MAX_WS_TEXT_BYTES + 1));
    }
}
