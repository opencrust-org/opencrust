use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use opencrust_agents::a2a::{
    A2AArtifact, A2AMessage, A2APart, A2ATask, AgentCapabilities, AgentCard, AgentSkill,
    CreateTaskRequest, TaskStatus,
};
use tracing::{info, warn};

use crate::state::SharedState;

fn complete_task(task: &mut A2ATask, response_text: String) -> bool {
    if task.status != TaskStatus::Working {
        return false;
    }

    task.status = TaskStatus::Completed;
    let response_parts = vec![A2APart::Text {
        text: response_text,
    }];
    task.messages.push(A2AMessage {
        role: "agent".to_string(),
        parts: response_parts.clone(),
    });
    task.artifacts.push(A2AArtifact {
        name: Some("response".to_string()),
        parts: response_parts,
        index: Some(0),
    });
    true
}

fn fail_task(task: &mut A2ATask) -> bool {
    if task.status != TaskStatus::Working {
        return false;
    }

    task.status = TaskStatus::Failed;
    true
}

/// GET /.well-known/agent.json — serve the agent card.
pub async fn agent_card(State(state): State<SharedState>) -> impl IntoResponse {
    let config = state.current_config();

    let skills: Vec<AgentSkill> = if !config.agents.is_empty() {
        config
            .agents
            .keys()
            .map(|name| AgentSkill {
                id: name.clone(),
                name: name.clone(),
                description: config
                    .agents
                    .get(name)
                    .and_then(|a| a.system_prompt.clone()),
                tags: vec![],
            })
            .collect()
    } else {
        vec![AgentSkill {
            id: "default".to_string(),
            name: "default".to_string(),
            description: config.agent.system_prompt.clone(),
            tags: vec![],
        }]
    };

    let card = AgentCard {
        name: "opencrust".to_string(),
        description: Some("OpenCrust AI agent".to_string()),
        url: format!("http://{}:{}", config.gateway.host, config.gateway.port),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        capabilities: AgentCapabilities {
            streaming: false,
            push_notifications: false,
        },
        skills,
    };

    Json(card)
}

/// POST /a2a/tasks — create a new task.
pub async fn create_task(
    State(state): State<SharedState>,
    Json(body): Json<CreateTaskRequest>,
) -> impl IntoResponse {
    let task_id = body.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Extract text from message parts
    let user_text: String = body
        .message
        .parts
        .iter()
        .filter_map(|p| match p {
            A2APart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    if user_text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "message must contain at least one text part" })),
        )
            .into_response();
    }

    // Input validation
    let guardrails = state.current_config().guardrails.clone();
    let user_text = opencrust_security::InputValidator::sanitize(&user_text);
    if opencrust_security::InputValidator::exceeds_length(&user_text, guardrails.max_input_chars) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("input rejected: message exceeds {} character limit", guardrails.max_input_chars)
            })),
        )
            .into_response();
    }
    if opencrust_security::InputValidator::check_prompt_injection(&user_text) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "input rejected: potential prompt injection detected"
            })),
        )
            .into_response();
    }

    // Rate limit (use task_id as user identity for A2A callers)
    let gateway_rate_limit = state.current_config().gateway.rate_limit.clone();
    if let Err(e) = state.check_user_rate_limit(&task_id, &gateway_rate_limit) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }

    // Token budget check
    let session_id_for_budget = format!("a2a:{task_id}");
    if let Err(e) = state
        .check_token_budget(&session_id_for_budget, &task_id, &guardrails)
        .await
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }

    // Create an initial task in "working" status
    let task = A2ATask {
        id: task_id.clone(),
        status: TaskStatus::Working,
        messages: vec![body.message],
        artifacts: vec![],
        metadata: body.metadata,
    };

    // Store the task
    state.a2a_tasks.insert(task_id.clone(), task);

    info!("A2A task created: {task_id}");

    // Process through agent runtime
    let session_id = format!("a2a:{task_id}");
    state
        .hydrate_session_history(&session_id, Some("a2a"), None)
        .await;
    let history = state.session_history(&session_id);
    let continuity_key = state.continuity_key(None);

    // Apply tool allowlist and per-session tool call budget
    state.agents.set_session_tool_config(
        &session_id,
        guardrails.allowed_tools.clone(),
        guardrails.session_tool_call_budget,
    );

    let result = state
        .agents
        .process_message_with_context(
            &session_id,
            &user_text,
            &history,
            continuity_key.as_deref(),
            None,
        )
        .await;

    match result {
        Ok(response_text) => {
            let response_text = opencrust_security::InputValidator::truncate_output(
                &response_text,
                guardrails.max_output_chars,
            );

            // Only a working task may transition to completed. Holding the map's
            // mutable guard makes this check atomic with a concurrent cancellation.
            let completed = state
                .a2a_tasks
                .get_mut(&task_id)
                .is_some_and(|mut task| complete_task(&mut task, response_text.clone()));

            if completed {
                state
                    .persist_turn(
                        &session_id,
                        Some("a2a"),
                        None,
                        &user_text,
                        &response_text,
                        None,
                    )
                    .await;
            }

            let task = state.a2a_tasks.get(&task_id).unwrap().clone();
            (StatusCode::OK, Json(serde_json::json!(task))).into_response()
        }
        Err(e) => {
            warn!("A2A task {task_id} failed: {e}");
            if let Some(mut task) = state.a2a_tasks.get_mut(&task_id) {
                let _ = fail_task(&mut task);
            }
            let task = state.a2a_tasks.get(&task_id).unwrap().clone();
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!(task)),
            )
                .into_response()
        }
    }
}

/// GET /a2a/tasks/:id — get task status.
pub async fn get_task(
    State(state): State<SharedState>,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    match state.a2a_tasks.get(&task_id) {
        Some(task) => (StatusCode::OK, Json(serde_json::json!(task.clone()))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "task not found" })),
        )
            .into_response(),
    }
}

/// POST /a2a/tasks/:id/cancel — cancel a task.
pub async fn cancel_task(
    State(state): State<SharedState>,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    match state.a2a_tasks.get_mut(&task_id) {
        Some(mut task) => {
            if task.status == TaskStatus::Completed || task.status == TaskStatus::Failed {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({ "error": "task already finished" })),
                )
                    .into_response();
            }
            task.status = TaskStatus::Canceled;
            info!("A2A task canceled: {task_id}");
            let task = task.clone();
            (StatusCode::OK, Json(serde_json::json!(task))).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "task not found" })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn task(status: TaskStatus) -> A2ATask {
        A2ATask {
            id: "task-1".to_string(),
            status,
            messages: vec![],
            artifacts: vec![],
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn completing_a_working_task_adds_the_response() {
        let mut task = task(TaskStatus::Working);

        assert!(complete_task(&mut task, "done".to_string()));
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(task.messages.len(), 1);
        assert_eq!(task.artifacts.len(), 1);
    }

    #[test]
    fn completion_does_not_overwrite_cancellation() {
        let mut task = task(TaskStatus::Canceled);

        assert!(!complete_task(&mut task, "late response".to_string()));
        assert_eq!(task.status, TaskStatus::Canceled);
        assert!(task.messages.is_empty());
        assert!(task.artifacts.is_empty());
    }

    #[test]
    fn failure_does_not_overwrite_cancellation() {
        let mut task = task(TaskStatus::Canceled);

        assert!(!fail_task(&mut task));
        assert_eq!(task.status, TaskStatus::Canceled);
    }
}
