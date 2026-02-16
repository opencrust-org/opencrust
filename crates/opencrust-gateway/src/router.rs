use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::Router;

use crate::state::SharedState;
use crate::ws;

/// Build the main application router with all routes.
pub fn build_router(state: SharedState) -> Router {
    let api_router = Router::new()
        .route("/status", get(status))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::ws_handler))
        .nest("/api", api_router)
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn auth_middleware(
    State(state): State<SharedState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());

    let authorized = if let Some(api_key) = &state.config.gateway.api_key {
        match auth_header {
            Some(h) if h == api_key => true,
            Some(h) if h.strip_prefix("Bearer ") == Some(api_key) => true,
            _ => false,
        }
    } else {
        // By default, if no API key is configured, we allow access.
        // In a production environment, you might want to require an API key to be set.
        true
    };

    if authorized {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn status(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> axum::Json<serde_json::Value> {
    let channels = state.channels.list();
    axum::Json(serde_json::json!({
        "status": "running",
        "channels": channels,
        "sessions": state.sessions.len(),
    }))
}
