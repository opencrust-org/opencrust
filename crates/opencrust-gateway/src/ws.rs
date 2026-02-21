use std::collections::HashMap;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::SinkExt;
use futures::stream::StreamExt;
use tokio::time::Instant;
use tracing::{info, warn};

use opencrust_agents::ChatMessage;

use crate::state::SharedState;

const MAX_WS_FRAME_BYTES: usize = 64 * 1024;
const MAX_WS_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_WS_TEXT_BYTES: usize = 32 * 1024;

/// Heartbeat: send ping every 30 seconds.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Close the connection if no pong received within 90 seconds.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

/// Per-WebSocket message rate limit: max messages per sliding window.
const WS_RATE_LIMIT_MAX: u32 = 30;
/// Sliding window duration for per-WebSocket rate limiting.
const WS_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// WebSocket upgrade handler.
pub async fn ws_handler(
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    State(state): State<SharedState>,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(configured_key) = &state.config.gateway.api_key {
        let token_from_query = params.get("token").or_else(|| params.get("api_key"));

        let token_from_header = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("Bearer ").unwrap_or(v));

        let token = token_from_query.map(|s| s.as_str()).or(token_from_header);

        // Constant-time comparison
        let valid = match token {
            Some(t) if t.len() == configured_key.len() => {
                t.bytes()
                    .zip(configured_key.bytes())
                    .fold(0, |acc, (a, b)| acc | (a ^ b))
                    == 0
            }
            _ => false,
        };

        if !valid {
            warn!("WebSocket connection rejected: invalid API key");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    ws.max_frame_size(MAX_WS_FRAME_BYTES)
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: SharedState) {
    let (mut sender, mut receiver) = socket.split();

    // Wait for the first message to decide: new session or resume.
    let session_id = match tokio::time::timeout(Duration::from_secs(10), receiver.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            if is_init_message(&text) {
                // Client is requesting a fresh session (no resume)
                let id = state.create_session();
                info!("new WebSocket connection (init): session={}", id);
                let welcome = serde_json::json!({
                    "type": "connected",
                    "session_id": id,
                });
                if sender
                    .send(Message::Text(welcome.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
                id
            } else if let Some(resume_id) = try_parse_resume(&text) {
                if state.resume_session(&resume_id) {
                    info!("resumed WebSocket session: {}", resume_id);

                    // Send resume-ack with history length
                    let history_len = state
                        .sessions
                        .get(&resume_id)
                        .map(|s| s.history.len())
                        .unwrap_or(0);
                    let ack = serde_json::json!({
                        "type": "resumed",
                        "session_id": resume_id,
                        "history_length": history_len,
                    });
                    if sender
                        .send(Message::Text(ack.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    resume_id
                } else {
                    // Session expired or doesn't exist — create fresh
                    let id = state.create_session();
                    info!("resume failed (expired), new session: {}", id);
                    let welcome = serde_json::json!({
                        "type": "connected",
                        "session_id": id,
                        "note": "previous session expired",
                    });
                    if sender
                        .send(Message::Text(welcome.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    id
                }
            } else {
                // First message is a regular chat message — create session, process it
                let id = state.create_session();
                info!("new WebSocket connection: session={}", id);
                let welcome = serde_json::json!({
                    "type": "connected",
                    "session_id": id,
                });
                if sender
                    .send(Message::Text(welcome.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }

                // Process this first message as a chat message
                if let Some(reply) = process_text_message(&text, &id, &state, &mut sender).await
                    && sender
                        .send(Message::Text(reply.to_string().into()))
                        .await
                        .is_err()
                {
                    state.disconnect_session(&id);
                    return;
                }
                id
            }
        }
        _ => {
            // Timeout or error reading first message — create session anyway
            let id = state.create_session();
            info!("new WebSocket connection: session={}", id);
            let welcome = serde_json::json!({
                "type": "connected",
                "session_id": id,
            });
            let _ = sender.send(Message::Text(welcome.to_string().into())).await;
            id
        }
    };

    // Main message loop with heartbeat
    let mut last_pong = Instant::now();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    // Don't send ping immediately
    heartbeat.tick().await;

    // Per-WebSocket sliding window rate limiter
    let mut msg_timestamps: std::collections::VecDeque<Instant> = std::collections::VecDeque::new();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                // Check pong timeout
                if last_pong.elapsed() > HEARTBEAT_TIMEOUT {
                    warn!("heartbeat timeout: session={}", session_id);
                    break;
                }
                // Send ping
                if sender.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        last_pong = Instant::now(); // text counts as activity
                        if let Some(mut session) = state.sessions.get_mut(&session_id) {
                            session.last_active = std::time::Instant::now();
                        }

                        // Per-WebSocket sliding window rate limit
                        let now = Instant::now();
                        msg_timestamps.retain(|ts| now.duration_since(*ts) < WS_RATE_LIMIT_WINDOW);
                        if msg_timestamps.len() >= WS_RATE_LIMIT_MAX as usize {
                            warn!("rate limited: session={}, msgs_in_window={}", session_id, msg_timestamps.len());
                            let err = serde_json::json!({
                                "type": "error",
                                "code": "rate_limited",
                                "message": "too many messages, please slow down",
                                "retry_after_secs": WS_RATE_LIMIT_WINDOW.as_secs(),
                            });
                            let _ = sender.send(Message::Text(err.to_string().into())).await;
                            continue;
                        }
                        msg_timestamps.push_back(now);

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

                        if let Some(reply) = process_text_message(&text, &session_id, &state, &mut sender).await
                            && sender
                                .send(Message::Text(reply.to_string().into()))
                                .await
                                .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_pong = Instant::now();
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket closed: session={}", session_id);
                        break;
                    }
                    Some(Err(e)) => {
                        warn!("WebSocket error: session={}, error={}", session_id, e);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
        }
    }

    // Mark disconnected (don't remove — allow resume within TTL)
    state.disconnect_session(&session_id);
    info!("session disconnected (resumable): {}", session_id);
}

/// Process a text message through validation and the agent runtime.
/// Returns the JSON reply value, or `None` if the message was rejected inline.
async fn process_text_message(
    text: &str,
    session_id: &str,
    state: &SharedState,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
) -> Option<serde_json::Value> {
    let (user_text, provider_id, model_override) = parse_user_message(text);

    // Input validation
    let user_text = opencrust_security::InputValidator::sanitize(&user_text);
    if opencrust_security::InputValidator::check_prompt_injection(&user_text) {
        warn!("prompt injection detected: session={}", session_id);
        let err = serde_json::json!({
            "type": "error",
            "session_id": session_id,
            "code": "prompt_injection_detected",
            "message": "input rejected: potential prompt injection detected",
        });
        let _ = sender.send(Message::Text(err.to_string().into())).await;
        return None;
    }

    // Ensure session exists and hydrate persisted history for web chat.
    state
        .hydrate_session_history(session_id, Some("web"), None)
        .await;
    let history: Vec<ChatMessage> = state.session_history(session_id);
    let continuity_key = state.continuity_key(None);

    // Route through agent runtime (with optional provider override)
    let reply = match state
        .agents
        .process_message_with_agent_config(
            session_id,
            &user_text,
            &history,
            continuity_key.as_deref(),
            None,
            provider_id.as_deref(),
            model_override.as_deref(),
            None,
            None,
        )
        .await
    {
        Ok(response_text) => {
            state
                .persist_turn(session_id, Some("web"), None, &user_text, &response_text)
                .await;

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

    Some(reply)
}

/// Try to parse a resume request: `{"type": "resume", "session_id": "..."}`.
fn is_init_message(raw: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("type")?.as_str().map(|s| s == "init"))
        .unwrap_or(false)
}

fn try_parse_resume(raw: &str) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    if v.get("type")?.as_str()? == "resume" {
        v.get("session_id")?.as_str().map(|s| s.to_string())
    } else {
        None
    }
}

fn text_message_too_large(len: usize) -> bool {
    len > MAX_WS_TEXT_BYTES
}

/// Try to extract `"content"` plus optional `"provider"` and `"model"` from JSON,
/// otherwise use the raw text as content with no overrides.
fn parse_user_message(raw: &str) -> (String, Option<String>, Option<String>) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(text) = v.get("content").and_then(|c| c.as_str())
    {
        let provider = v
            .get("provider")
            .and_then(|p| p.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let model = v
            .get("model")
            .and_then(|m| m.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        return (text.to_string(), provider, model);
    }
    (raw.to_string(), None, None)
}

#[cfg(test)]
mod tests {
    use super::{MAX_WS_TEXT_BYTES, parse_user_message, text_message_too_large, try_parse_resume};

    #[test]
    fn text_message_size_guard_uses_strict_upper_bound() {
        assert!(!text_message_too_large(MAX_WS_TEXT_BYTES));
        assert!(text_message_too_large(MAX_WS_TEXT_BYTES + 1));
    }

    #[test]
    fn parse_resume_request() {
        let json = r#"{"type": "resume", "session_id": "abc-123"}"#;
        assert_eq!(try_parse_resume(json), Some("abc-123".to_string()));
    }

    #[test]
    fn parse_non_resume_returns_none() {
        let json = r#"{"type": "message", "content": "hello"}"#;
        assert_eq!(try_parse_resume(json), None);
    }

    #[test]
    fn parse_invalid_json_returns_none() {
        assert_eq!(try_parse_resume("not json"), None);
    }

    #[test]
    fn parse_user_message_extracts_provider() {
        let json = r#"{"content": "hello", "provider": "anthropic"}"#;
        let (text, provider, model) = parse_user_message(json);
        assert_eq!(text, "hello");
        assert_eq!(provider, Some("anthropic".to_string()));
        assert_eq!(model, None);
    }

    #[test]
    fn parse_user_message_extracts_provider_and_model() {
        let json = r#"{"content": "hello", "provider": "ollama", "model": "kimi"}"#;
        let (text, provider, model) = parse_user_message(json);
        assert_eq!(text, "hello");
        assert_eq!(provider, Some("ollama".to_string()));
        assert_eq!(model, Some("kimi".to_string()));
    }

    #[test]
    fn parse_user_message_no_provider() {
        let json = r#"{"content": "hello"}"#;
        let (text, provider, model) = parse_user_message(json);
        assert_eq!(text, "hello");
        assert_eq!(provider, None);
        assert_eq!(model, None);
    }

    #[test]
    fn parse_user_message_plain_text() {
        let (text, provider, model) = parse_user_message("just plain text");
        assert_eq!(text, "just plain text");
        assert_eq!(provider, None);
        assert_eq!(model, None);
    }
}
