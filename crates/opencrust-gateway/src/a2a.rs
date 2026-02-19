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
            state
                .persist_turn(&session_id, Some("a2a"), None, &user_text, &response_text)
                .await;

            // Update task to completed with artifact
            if let Some(mut task) = state.a2a_tasks.get_mut(&task_id) {
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
            }

            let task = state.a2a_tasks.get(&task_id).unwrap().clone();
            (StatusCode::OK, Json(serde_json::json!(task))).into_response()
        }
        Err(e) => {
            warn!("A2A task {task_id} failed: {e}");
            if let Some(mut task) = state.a2a_tasks.get_mut(&task_id) {
                task.status = TaskStatus::Failed;
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
