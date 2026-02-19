use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use opencrust_agents::{ContentBlock, MessagePart};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::agent_router;
use crate::state::SharedState;

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    /// Optional named agent to use for this session.
    pub agent_id: Option<String>,
}

#[derive(Serialize)]
pub struct CreateSessionResponse {
    pub session_id: String,
    pub agent_id: Option<String>,
}

#[derive(Deserialize)]
pub struct SendMessageRequest {
    pub content: String,
    /// Optional named agent override for this message.
    pub agent_id: Option<String>,
}

#[derive(Serialize)]
pub struct SendMessageResponse {
    pub session_id: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub channel_id: Option<String>,
    pub connected: bool,
    pub history_length: usize,
}

/// POST /api/sessions — create a new session.
pub async fn create_session(
    State(state): State<SharedState>,
    Json(body): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let session_id = state.create_session();

    // If an agent_id was requested, tag it on the session metadata
    if let Some(ref agent_id) = body.agent_id
        && let Some(mut session) = state.sessions.get_mut(&session_id)
    {
        session.channel_id = Some(format!("api:{agent_id}"));
    }

    (
        StatusCode::CREATED,
        Json(CreateSessionResponse {
            session_id,
            agent_id: body.agent_id,
        }),
    )
}

/// POST /api/sessions/:id/messages — send a message to a session.
pub async fn send_message(
    State(state): State<SharedState>,
    Path(session_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> impl IntoResponse {
    if !state.sessions.contains_key(&session_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response();
    }

    // Hydrate history
    state
        .hydrate_session_history(&session_id, Some("api"), None)
        .await;
    let history = state.session_history(&session_id);
    let continuity_key = state.continuity_key(None);

    // Resolve named agent config
    let config = state.current_config();
    let agent_config = agent_router::resolve(&config, body.agent_id.as_deref(), None);

    let result = if let Some(ac) = agent_config {
        state
            .agents
            .process_message_with_agent_config(
                &session_id,
                &body.content,
                &history,
                continuity_key.as_deref(),
                None,
                ac.provider.as_deref(),
                ac.system_prompt.as_deref(),
                ac.max_tokens,
            )
            .await
    } else {
        state
            .agents
            .process_message_with_context(
                &session_id,
                &body.content,
                &history,
                continuity_key.as_deref(),
                None,
            )
            .await
    };

    match result {
        Ok(response_text) => {
            state
                .persist_turn(
                    &session_id,
                    Some("api"),
                    None,
                    &body.content,
                    &response_text,
                )
                .await;
            (
                StatusCode::OK,
                Json(serde_json::json!(SendMessageResponse {
                    session_id,
                    content: response_text,
                })),
            )
                .into_response()
        }
        Err(e) => {
            warn!("agent error in API session {session_id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/sessions/:id/history — get session history.
pub async fn session_history(
    State(state): State<SharedState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    if !state.sessions.contains_key(&session_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response();
    }

    state
        .hydrate_session_history(&session_id, Some("api"), None)
        .await;
    let history = state.session_history(&session_id);
    let messages: Vec<serde_json::Value> = history
        .iter()
        .map(|m| {
            let text = match &m.content {
                MessagePart::Text(s) => s.clone(),
                MessagePart::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            serde_json::json!({
                "role": format!("{:?}", m.role).to_lowercase(),
                "content": text,
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "messages": messages })),
    )
        .into_response()
}

/// GET /api/sessions — list active sessions.
pub async fn list_sessions(State(state): State<SharedState>) -> impl IntoResponse {
    let sessions: Vec<SessionInfo> = state
        .sessions
        .iter()
        .map(|entry| SessionInfo {
            session_id: entry.id.clone(),
            channel_id: entry.channel_id.clone(),
            connected: entry.connected,
            history_length: entry.history.len(),
        })
        .collect();

    Json(serde_json::json!({ "sessions": sessions }))
}
