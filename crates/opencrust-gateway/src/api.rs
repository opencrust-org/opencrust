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
    /// Optional model override for this message.
    pub model: Option<String>,
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
    // Look up session and extract any stored agent_id (set at session creation).
    let session_agent_id = match state.sessions.get(&session_id) {
        Some(s) => s
            .channel_id
            .as_deref()
            .and_then(|ch| ch.strip_prefix("api:"))
            .map(str::to_string),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "session not found" })),
            )
                .into_response();
        }
    };

    // Prefer explicit per-message agent_id, fall back to the one stored on the session.
    let effective_agent_id = body.agent_id.clone().or(session_agent_id);

    // Input validation
    let guardrails = state.current_config().guardrails.clone();
    let content = opencrust_security::InputValidator::sanitize(&body.content);
    if opencrust_security::InputValidator::exceeds_length(&content, guardrails.max_input_chars) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("input rejected: message exceeds {} character limit", guardrails.max_input_chars)
            })),
        )
            .into_response();
    }
    if opencrust_security::InputValidator::check_prompt_injection(&content) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "input rejected: potential prompt injection detected"
            })),
        )
            .into_response();
    }

    // Rate limit (use session_id as user identity for API sessions)
    let gateway_rate_limit = state.current_config().gateway.rate_limit.clone();
    if let Err(e) = state.check_user_rate_limit(&session_id, &gateway_rate_limit) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }

    // Token budget check
    if let Err(e) = state
        .check_token_budget(&session_id, &session_id, &guardrails)
        .await
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }

    // Apply tool allowlist and per-session tool call budget
    state.agents.set_session_tool_config(
        &session_id,
        guardrails.allowed_tools.clone(),
        guardrails.session_tool_call_budget,
    );

    // Hydrate history
    state
        .hydrate_session_history(&session_id, Some("api"), None)
        .await;
    let history = state.session_history(&session_id);
    let continuity_key = state.continuity_key(None);

    // Resolve named agent config
    let config = state.current_config();
    let agent_config = agent_router::resolve(&config, effective_agent_id.as_deref(), None);

    let result = if let Some(ac) = agent_config {
        // Apply per-agent tool whitelist (#300)
        if !ac.tools.is_empty() {
            state
                .agents
                .set_session_tool_config(&session_id, Some(ac.tools.clone()), None);
        }
        // Apply per-agent DNA override (#303)
        if let Some(dna_path) = &ac.dna_file {
            let content = std::fs::read_to_string(dna_path)
                .ok()
                .filter(|s| !s.trim().is_empty());
            state.agents.set_session_dna_override(&session_id, content);
        }
        // Apply per-agent skills override (#303)
        if let Some(skills_path) = &ac.skills_dir {
            let skills_block = crate::agent_overrides::load_skills_flat_block(skills_path);
            state
                .agents
                .set_session_skills_override(&session_id, skills_block);
        }
        state
            .agents
            .process_message_with_agent_config(
                &session_id,
                &content,
                &history,
                continuity_key.as_deref(),
                None,
                ac.provider.as_deref(),
                body.model.as_deref(),
                ac.system_prompt.as_deref(),
                ac.max_tokens,
                ac.max_context_tokens, // #302
            )
            .await
    } else if body.model.is_some() {
        state
            .agents
            .process_message_with_agent_config(
                &session_id,
                &content,
                &history,
                continuity_key.as_deref(),
                None,
                None,
                body.model.as_deref(),
                None,
                None,
                None,
            )
            .await
    } else {
        state
            .agents
            .process_message_with_context(
                &session_id,
                &content,
                &history,
                continuity_key.as_deref(),
                None,
            )
            .await
    };

    match result {
        Ok(response_text) => {
            let response_text = opencrust_security::InputValidator::truncate_output(
                &response_text,
                guardrails.max_output_chars,
            );
            state
                .persist_turn(
                    &session_id,
                    Some("api"),
                    None,
                    &content,
                    &response_text,
                    None,
                )
                .await;
            if let Some((input, output, provider, model)) =
                state.agents.take_session_usage(&session_id)
            {
                state
                    .persist_usage(&session_id, &provider, &model, input, output)
                    .await;
            }
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
/// Loads from persistent storage if the session is not in memory (e.g. after server restart).
pub async fn session_history(
    State(state): State<SharedState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
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
