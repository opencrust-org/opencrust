use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use base64::Engine;
use opencrust_agents::{CodexAuthConfig, CodexProvider, parse_codex_id_token_claims};
use opencrust_security::credentials::vault_passphrase_available;
use ring::digest::{SHA256, digest};
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_http::services::ServeDir;
use url::form_urlencoded;
use uuid::Uuid;

use crate::a2a;
use crate::api;
use crate::state::{GoogleOAuthRuntimeConfig, SharedState};
use crate::ws;

/// Build the main application router with all routes.
pub fn build_router(
    state: SharedState,
    whatsapp_state: opencrust_channels::whatsapp::webhook::WhatsAppState,
) -> Router {
    // Per-IP rate limit from config (default: 1 req/sec, burst 60).
    let rl = &state.config.gateway.rate_limit;
    let governor_conf = GovernorConfigBuilder::default()
        .per_second(rl.per_second)
        .burst_size(rl.burst_size)
        .finish()
        .expect("governor config should be valid");
    let governor_limiter = governor_conf.limiter().clone();
    let governor_layer = GovernorLayer::new(governor_conf);

    // Spawn a background task to clean up rate-limiter state for inactive IPs.
    tokio::spawn(async move {
        let interval = std::time::Duration::from_secs(60);
        loop {
            tokio::time::sleep(interval).await;
            governor_limiter.retain_recent();
        }
    });

    let whatsapp_routes = Router::new()
        .route(
            "/webhooks/whatsapp",
            get(opencrust_channels::whatsapp::webhook::whatsapp_verify)
                .post(opencrust_channels::whatsapp::webhook::whatsapp_webhook),
        )
        .with_state(whatsapp_state);

    let protected_integration_routes = Router::new()
        .route(
            "/api/integrations/google",
            get(get_google_integration).post(set_google_integration),
        )
        .route(
            "/api/integrations/google/config",
            get(get_google_integration_config).post(set_google_integration_config),
        )
        .route(
            "/api/integrations/google/diagnostics",
            get(get_google_integration_diagnostics),
        )
        .route(
            "/api/integrations/google/connect",
            get(start_google_integration_connect),
        )
        .route(
            "/api/integrations/google/connect-url",
            get(get_google_integration_connect_url),
        )
        .route(
            "/api/integrations/google/disconnect",
            post(disconnect_google_integration),
        )
        .route("/api/security/vault", get(get_vault_status))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_gateway_api_key,
        ));

    Router::new()
        .route("/", get(web_chat))
        .route("/health", get(health))
        .route("/ws", get(ws::ws_handler))
        .route("/api/status", get(status))
        .route("/api/auth-check", get(auth_check))
        .route(
            "/api/sessions",
            get(api::list_sessions).post(api::create_session),
        )
        .route("/api/sessions/{id}/messages", post(api::send_message))
        .route("/api/sessions/{id}/history", get(api::session_history))
        .route("/api/providers", get(list_providers).post(add_provider))
        .route(
            "/api/providers/codex/oauth/start",
            get(start_codex_provider_oauth),
        )
        .route(
            "/api/providers/codex/oauth/callback",
            get(handle_codex_provider_oauth_callback),
        )
        .route(
            "/api/providers/codex/oauth/complete",
            post(complete_codex_provider_oauth),
        )
        .route("/auth/callback", get(handle_codex_provider_oauth_callback))
        .route(
            "/api/integrations/google/callback",
            get(handle_google_integration_callback),
        )
        .route("/api/mcp", get(list_mcp_servers))
        // A2A protocol endpoints
        .route("/.well-known/agent.json", get(a2a::agent_card))
        .route("/a2a/tasks", post(a2a::create_task))
        .route("/a2a/tasks/{id}", get(a2a::get_task))
        .route("/a2a/tasks/{id}/cancel", post(a2a::cancel_task))
        .nest_service("/assets", ServeDir::new("assets"))
        .merge(protected_integration_routes)
        .merge(whatsapp_routes)
        .with_state(state)
        .layer(governor_layer)
}

async fn health() -> &'static str {
    "ok"
}

async fn web_chat() -> Html<String> {
    // Hot-reload during local development if the source file is present
    if let Ok(content) = std::fs::read_to_string("crates/opencrust-gateway/src/webchat.html") {
        return Html(content);
    }

    // Fall back to the statically compiled version for release binaries
    Html(include_str!("webchat.html").to_string())
}

async fn status(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> axum::Json<serde_json::Value> {
    let mut channels: Vec<String> = state
        .channels
        .list()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    // Include channels registered via sender handles (the primary source).
    for entry in state.channel_senders.iter() {
        let ct = entry.key().clone();
        if !channels.contains(&ct) {
            channels.push(ct);
        }
    }

    // Gather LLM provider info from config
    let llm: serde_json::Value = state
        .config
        .llm
        .iter()
        .map(|(name, cfg)| {
            let mut info = serde_json::json!({ "provider": cfg.provider });
            if let Some(m) = &cfg.model {
                info["model"] = serde_json::Value::String(m.clone());
            }
            (name.clone(), info)
        })
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into();

    // Check for available update (from cached check file)
    let latest_version = read_cached_latest_version();

    let mut resp = serde_json::json!({
        "status": "running",
        "version": env!("CARGO_PKG_VERSION"),
        "channels": channels,
        "sessions": state.sessions.len(),
        "llm": llm,
    });
    if let Some(latest) = latest_version {
        let current = env!("CARGO_PKG_VERSION");
        if latest.trim_start_matches('v') != current {
            resp["latest_version"] = serde_json::Value::String(latest);
        }
    }

    axum::Json(resp)
}

async fn auth_check(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "auth_required": state.config.gateway.api_key.is_some(),
    }))
}

async fn require_gateway_api_key(
    axum::extract::State(state): axum::extract::State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> axum::response::Response {
    let Some(configured_key) = state.config.gateway.api_key.as_deref() else {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": "Gateway API key is required for integration endpoints. Set OPENCRUST_GATEWAY_API_KEY.",
            })),
        )
            .into_response();
    };

    let token_from_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.strip_prefix("Bearer ").unwrap_or(value).to_string());

    let token_from_query = req.uri().query().and_then(|query| {
        form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == "token" || key == "api_key")
            .map(|(_, value)| value.into_owned())
    });

    let valid = token_from_header
        .or(token_from_query)
        .as_deref()
        .map(|token| constant_time_token_eq(token, configured_key))
        .unwrap_or(false);

    if valid {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": "Invalid or missing gateway API key.",
            })),
        )
            .into_response()
    }
}

fn constant_time_token_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// GET /api/security/vault — report whether secure vault persistence is available this run.
async fn get_vault_status() -> axum::Json<serde_json::Value> {
    let vault_path = crate::bootstrap::default_vault_path();
    let vault_exists = vault_path
        .as_ref()
        .map(|path| opencrust_security::CredentialVault::exists(path))
        .unwrap_or(false);
    let unlocked = vault_path
        .as_ref()
        .map(|path| vault_passphrase_available(path))
        .unwrap_or(false);

    axum::Json(serde_json::json!({
        "vault_exists": vault_exists,
        "unlocked": unlocked,
    }))
}

/// GET /api/integrations/google — current Google integration state.
async fn get_google_integration(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> axum::Json<serde_json::Value> {
    axum::Json(google_integration_status_json(&state))
}

#[derive(serde::Deserialize)]
struct SetGoogleIntegrationRequest {
    connected: bool,
}

/// POST /api/integrations/google — toggle Google integration connection state.
async fn set_google_integration(
    axum::extract::State(state): axum::extract::State<SharedState>,
    axum::Json(body): axum::Json<SetGoogleIntegrationRequest>,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    // Connecting requires full OAuth flow via /api/integrations/google/connect.
    if body.connected {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": "Use /api/integrations/google/connect for OAuth connect flow",
            })),
        );
    }

    state.set_google_workspace_connected(body.connected);
    (
        axum::http::StatusCode::OK,
        axum::Json(google_integration_status_json(&state)),
    )
}

const GOOGLE_OAUTH_SCOPES_BASE: &[&str] = &[
    "openid",
    "email",
    "profile",
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/calendar.readonly",
    "https://www.googleapis.com/auth/drive.metadata.readonly",
];
const GOOGLE_OAUTH_SCOPE_GMAIL_SEND: &str = "https://www.googleapis.com/auth/gmail.send";
const GOOGLE_OAUTH_STATE_TTL_SECS: u64 = 600;
const CODEX_OAUTH_ISSUER: &str = "https://auth.openai.com";
const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_STATE_TTL_SECS: u64 = 600;
const CODEX_OAUTH_LOOPBACK_HOST: &str = "localhost";
const CODEX_OAUTH_LOOPBACK_PORT: u16 = 1455;
const CODEX_OAUTH_CALLBACK_PATH: &str = "/auth/callback";
const CODEX_OAUTH_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

fn google_gmail_send_scope_enabled() -> bool {
    std::env::var("OPENCRUST_GOOGLE_ENABLE_GMAIL_SEND_SCOPE")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn google_oauth_scopes() -> Vec<&'static str> {
    let mut scopes = GOOGLE_OAUTH_SCOPES_BASE.to_vec();
    if google_gmail_send_scope_enabled() {
        scopes.push(GOOGLE_OAUTH_SCOPE_GMAIL_SEND);
    }
    scopes
}

fn google_oauth_scope_string() -> String {
    google_oauth_scopes().join(" ")
}

#[derive(Debug, Clone)]
struct GoogleOAuthConfig {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    source: GoogleOAuthConfigSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoogleOAuthConfigSource {
    Runtime,
    EnvOrVault,
}

#[derive(serde::Deserialize)]
struct GoogleOAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(serde::Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct GoogleUserInfo {
    email: Option<String>,
}

#[derive(serde::Deserialize)]
struct CodexOAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(serde::Deserialize)]
struct CodexOAuthTokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

#[derive(serde::Deserialize)]
struct CodexOAuthManualCompleteRequest {
    redirect_url: String,
}

fn generate_codex_pkce() -> (String, String) {
    let verifier_bytes = Uuid::new_v4().as_bytes().repeat(4);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(digest(&SHA256, verifier.as_bytes()).as_ref());
    (verifier, challenge)
}

fn origin_from_headers(headers: &axum::http::HeaderMap, fallback_host: &str) -> String {
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or("http");
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(axum::http::header::HOST))
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback_host);
    format!("{proto}://{host}")
}

fn codex_redirect_uri() -> String {
    format!(
        "http://{CODEX_OAUTH_LOOPBACK_HOST}:{CODEX_OAUTH_LOOPBACK_PORT}{CODEX_OAUTH_CALLBACK_PATH}"
    )
}

fn codex_authorize_url(state: &SharedState, headers: &axum::http::HeaderMap) -> String {
    let redirect_uri = codex_redirect_uri();
    let opener_origin = origin_from_headers(headers, "127.0.0.1:3888");
    let state_token = Uuid::new_v4().to_string();
    let (code_verifier, code_challenge) = generate_codex_pkce();
    state.issue_codex_oauth_state(
        state_token.clone(),
        code_verifier,
        redirect_uri.clone(),
        opener_origin,
    );

    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", CODEX_OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", CODEX_OAUTH_SCOPE)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", &state_token)
        .append_pair("originator", "codex_cli_rs")
        .finish();
    format!("{CODEX_OAUTH_ISSUER}/oauth/authorize?{query}")
}

/// GET /api/integrations/google/connect — start Google OAuth consent flow.
async fn start_google_integration_connect(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> impl IntoResponse {
    match google_authorize_url(&state) {
        Ok(url) => Redirect::temporary(&url).into_response(),
        Err(message) => (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        )
            .into_response(),
    }
}

/// GET /api/integrations/google/connect-url — preflight connect and return OAuth URL.
async fn get_google_integration_connect_url(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    match google_authorize_url(&state) {
        Ok(url) => (
            axum::http::StatusCode::OK,
            axum::Json(serde_json::json!({
                "status": "ok",
                "url": url,
            })),
        ),
        Err(message) => (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        ),
    }
}

async fn start_codex_provider_oauth(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    Redirect::temporary(&codex_authorize_url(&state, &headers)).into_response()
}

fn google_authorize_url(state: &SharedState) -> Result<String, String> {
    let Some(oauth) = google_oauth_config(state) else {
        return Err(
            "Google OAuth is not configured. Set GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET."
                .to_string(),
        );
    };

    if !is_valid_google_client_id(&oauth.client_id) {
        return Err("Configured Google Client ID format is invalid. It should look like: 1234567890-abcdef.apps.googleusercontent.com".to_string());
    }

    if !is_valid_redirect_uri(&oauth.redirect_uri) {
        return Err("Configured redirect URI is invalid. Use an absolute URL like http://127.0.0.1:3000/api/integrations/google/callback".to_string());
    }

    let effective_redirect_uri = effective_google_redirect_uri(state, &oauth.redirect_uri);
    let state_token = state.issue_google_oauth_state();
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &oauth.client_id)
        .append_pair("redirect_uri", &effective_redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &google_oauth_scope_string())
        .append_pair("access_type", "offline")
        .append_pair("include_granted_scopes", "true")
        .append_pair("prompt", "consent")
        .append_pair("state", &state_token)
        .finish();

    Ok(format!(
        "https://accounts.google.com/o/oauth2/v2/auth?{query}"
    ))
}

fn is_valid_redirect_uri(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }

    match url::Url::parse(trimmed) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
        Err(_) => false,
    }
}

fn redirect_origin(value: &str) -> Option<String> {
    url::Url::parse(value).ok().and_then(|parsed| {
        let host = parsed.host_str()?;
        let mut origin = format!("{}://{}", parsed.scheme(), host);
        if let Some(port) = parsed.port() {
            origin.push(':');
            origin.push_str(&port.to_string());
        }
        Some(origin)
    })
}

fn effective_google_redirect_uri(state: &SharedState, configured_redirect_uri: &str) -> String {
    let configured = configured_redirect_uri.trim();
    if configured.is_empty() {
        return default_google_redirect_uri(state);
    }

    configured.to_string()
}

fn google_client_secret_kind(value: &str) -> &'static str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "missing";
    }

    if trimmed.contains("BEGIN PRIVATE KEY")
        || trimmed.contains("\"type\": \"service_account\"")
        || trimmed.contains("\"private_key\"")
    {
        return "service_account_key";
    }

    if trimmed.starts_with("GOCSPX-") {
        return "oauth_client_secret";
    }

    "unknown"
}

fn invalid_client_hint(details: &str) -> String {
    if details.contains("invalid_client") {
        return " Ensure this is an OAuth Client ID/Secret for a Google Web application (not a service account key), and that the redirect URI exactly matches Google Cloud settings.".to_string();
    }
    String::new()
}

async fn parse_google_error(context: &str, resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    let details = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| {
            json.get("error_description")
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
                .or_else(|| {
                    json.get("error")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string)
                })
                .or_else(|| {
                    json.get("error")
                        .and_then(|v| v.get("message"))
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string)
                })
        })
        .unwrap_or_else(|| body.trim().to_string());

    let message = if details.is_empty() {
        format!("{context} (HTTP {status})")
    } else {
        format!("{context} (HTTP {status}): {details}")
    };
    format!("{message}{}", invalid_client_hint(&details))
}

async fn handle_codex_provider_oauth_callback(
    State(state): State<SharedState>,
    Query(query): Query<CodexOAuthCallbackQuery>,
) -> impl IntoResponse {
    let target_origin = query
        .state
        .as_deref()
        .and_then(|state_token| state.codex_oauth_target_origin(state_token));
    match complete_codex_oauth(state, query).await {
        Ok(message) => codex_oauth_popup_result(true, &message, target_origin.as_deref()),
        Err(error) => codex_oauth_popup_result(false, &error, target_origin.as_deref()),
    }
}

async fn complete_codex_oauth(
    state: SharedState,
    query: CodexOAuthCallbackQuery,
) -> Result<String, String> {
    if let Some(error) = query.error {
        let details = query.error_description.unwrap_or(error);
        return Err(format!("Codex authorization failed: {details}"));
    }

    let Some(code) = query.code else {
        return Err("Missing OAuth code in callback".to_string());
    };
    let Some(state_token) = query.state else {
        return Err("Missing OAuth state in callback".to_string());
    };

    let Some(pending) = state.consume_codex_oauth_state(
        &state_token,
        Duration::from_secs(CODEX_OAUTH_STATE_TTL_SECS),
    ) else {
        return Err("OAuth state invalid or expired".to_string());
    };

    let http = reqwest::Client::new();
    let token_response = match http
        .post(format!("{CODEX_OAUTH_ISSUER}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", pending.redirect_uri.as_str()),
            ("client_id", CODEX_OAUTH_CLIENT_ID),
            ("code_verifier", pending.code_verifier.as_str()),
        ])
        .send()
        .await
    {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("Codex token exchange failed ({status}): {body}"));
            }
            match resp.json::<CodexOAuthTokenResponse>().await {
                Ok(token) => token,
                Err(err) => {
                    return Err(format!("Failed to parse Codex token response: {err}"));
                }
            }
        }
        Err(err) => {
            return Err(format!("Codex token exchange failed: {err}"));
        }
    };

    let claims = parse_codex_id_token_claims(&token_response.id_token).ok();
    let account_id = claims.as_ref().and_then(|claims| claims.account_id.clone());
    let email = claims.as_ref().and_then(|claims| claims.email.clone());

    persist_secret("CODEX_ACCESS_TOKEN", &token_response.access_token);
    persist_secret("CODEX_REFRESH_TOKEN", &token_response.refresh_token);
    persist_secret("CODEX_ID_TOKEN", &token_response.id_token);
    if let Some(account_id) = account_id.as_deref() {
        persist_secret("CODEX_ACCOUNT_ID", account_id);
    }
    if let Err(err) = crate::bootstrap::upsert_codex_config_entry() {
        tracing::warn!("failed to persist codex provider config entry: {err}");
    }

    let provider = CodexProvider::new(
        CodexAuthConfig {
            access_token: Some(token_response.access_token),
            refresh_token: Some(token_response.refresh_token),
            account_id: account_id.clone(),
            id_token: Some(token_response.id_token),
        },
        None,
        None,
    );
    state.agents.register_provider(Arc::new(provider));
    state.agents.set_default_provider_id("codex");

    let success_message = match email {
        Some(email) => format!("Codex connected for {email}."),
        None => "Codex connected.".to_string(),
    };
    Ok(success_message)
}

pub fn build_codex_loopback_router(state: SharedState) -> Router {
    Router::new()
        .route(
            CODEX_OAUTH_CALLBACK_PATH,
            get(
                |State(state): State<SharedState>, Query(query): Query<CodexOAuthCallbackQuery>| async move {
                    let target_origin = query
                        .state
                        .as_deref()
                        .and_then(|state_token| state.codex_oauth_target_origin(state_token));
                    match complete_codex_oauth(state, query).await {
                        Ok(message) => codex_oauth_popup_result(true, &message, target_origin.as_deref()),
                        Err(error) => codex_oauth_popup_result(false, &error, target_origin.as_deref()),
                    }
                },
            ),
        )
        .with_state(state)
}

async fn complete_codex_provider_oauth(
    State(state): State<SharedState>,
    axum::Json(body): axum::Json<CodexOAuthManualCompleteRequest>,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let parsed = match url::Url::parse(body.redirect_url.trim()) {
        Ok(url) => url,
        Err(err) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "status": "error",
                    "message": format!("Invalid redirect URL: {err}"),
                })),
            );
        }
    };

    let params: std::collections::HashMap<String, String> =
        parsed.query_pairs().into_owned().collect();
    let query = CodexOAuthCallbackQuery {
        code: params.get("code").cloned(),
        state: params.get("state").cloned(),
        error: params.get("error").cloned(),
        error_description: params.get("error_description").cloned(),
    };

    match complete_codex_oauth(state, query).await {
        Ok(message) => (
            axum::http::StatusCode::OK,
            axum::Json(serde_json::json!({
                "status": "ok",
                "message": message,
            })),
        ),
        Err(message) => (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        ),
    }
}

/// GET /api/integrations/google/callback — OAuth callback for Google connect.
async fn handle_google_integration_callback(
    axum::extract::State(state): axum::extract::State<SharedState>,
    Query(query): Query<GoogleOAuthCallbackQuery>,
) -> impl IntoResponse {
    if let Some(error) = query.error {
        let details = query.error_description.unwrap_or(error);
        return oauth_popup_result(false, &format!("Google authorization failed: {details}"))
            .into_response();
    }

    let Some(code) = query.code else {
        return oauth_popup_result(false, "Missing OAuth code in callback").into_response();
    };

    let Some(state_token) = query.state else {
        return oauth_popup_result(false, "Missing OAuth state in callback").into_response();
    };

    if !state.consume_google_oauth_state(
        &state_token,
        Duration::from_secs(GOOGLE_OAUTH_STATE_TTL_SECS),
    ) {
        return oauth_popup_result(false, "OAuth state invalid or expired").into_response();
    }

    let Some(oauth) = google_oauth_config(&state) else {
        return oauth_popup_result(
            false,
            "Google OAuth is not configured. Set GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET.",
        )
        .into_response();
    };

    if !is_valid_google_client_id(&oauth.client_id) {
        return oauth_popup_result(
            false,
            "Configured Google Client ID format is invalid. Use a Web OAuth client ID like: 1234567890-abcdef.apps.googleusercontent.com",
        )
        .into_response();
    }

    let http = reqwest::Client::new();
    let effective_redirect_uri = effective_google_redirect_uri(&state, &oauth.redirect_uri);
    let token = match http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", code.as_str()),
            ("client_id", oauth.client_id.as_str()),
            ("client_secret", oauth.client_secret.as_str()),
            ("redirect_uri", effective_redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<GoogleTokenResponse>().await {
            Ok(token) => token,
            Err(err) => {
                return oauth_popup_result(
                    false,
                    &format!("Failed to parse Google token response: {err}"),
                )
                .into_response();
            }
        },
        Ok(resp) => {
            let details = parse_google_error("Google token exchange failed", resp).await;
            return oauth_popup_result(false, &details).into_response();
        }
        Err(err) => {
            return oauth_popup_result(false, &format!("Google token request failed: {err}"))
                .into_response();
        }
    };

    let email = match http
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .bearer_auth(&token.access_token)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp
            .json::<GoogleUserInfo>()
            .await
            .ok()
            .and_then(|info| info.email),
        Ok(_) => None,
        Err(_) => None,
    };

    if let Some(refresh_token) = token.refresh_token.as_deref() {
        crate::google_secrets::set_runtime_secret("GOOGLE_WORKSPACE_REFRESH_TOKEN", refresh_token);
        persist_api_key("GOOGLE_WORKSPACE_REFRESH_TOKEN", refresh_token);
    }

    state.set_google_workspace_identity(email.clone());

    let success_message = if let Some(email) = email {
        format!("Google Workspace connected as {email}.")
    } else {
        "Google Workspace connected.".to_string()
    };

    oauth_popup_result(true, &success_message).into_response()
}

/// POST /api/integrations/google/disconnect — clear current Google connection state.
async fn disconnect_google_integration(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    crate::google_secrets::remove_runtime_secret("GOOGLE_WORKSPACE_REFRESH_TOKEN");
    persist_api_key("GOOGLE_WORKSPACE_REFRESH_TOKEN", "");
    state.set_google_workspace_connected(false);
    (
        axum::http::StatusCode::OK,
        axum::Json(google_integration_status_json(&state)),
    )
}

#[derive(serde::Deserialize)]
struct SetGoogleIntegrationConfigRequest {
    client_id: String,
    client_secret: String,
    redirect_uri: Option<String>,
}

/// GET /api/integrations/google/config — current OAuth client config metadata.
async fn get_google_integration_config(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> axum::Json<serde_json::Value> {
    let current = google_oauth_config(&state);
    let configured = current.is_some();
    let (client_id, has_secret, redirect_uri, source) = match current.as_ref() {
        Some(cfg) => (
            Some(mask_client_id(&cfg.client_id)),
            true,
            Some(effective_google_redirect_uri(&state, &cfg.redirect_uri)),
            match cfg.source {
                GoogleOAuthConfigSource::Runtime => "runtime",
                GoogleOAuthConfigSource::EnvOrVault => "env_or_vault",
            },
        ),
        None => (
            None,
            false,
            Some(default_google_redirect_uri(&state)),
            "none",
        ),
    };

    axum::Json(serde_json::json!({
        "configured": configured,
        "client_id": client_id,
        "has_client_secret": has_secret,
        "redirect_uri": redirect_uri,
        "source": source,
    }))
}

/// GET /api/integrations/google/diagnostics — fixed, explicit OAuth diagnostics.
async fn get_google_integration_diagnostics(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> axum::Json<serde_json::Value> {
    let current = google_oauth_config(&state);
    let mut issues: Vec<String> = Vec::new();
    let required_oauth_scopes = google_oauth_scopes();

    let (
        configured,
        source,
        client_id_valid,
        redirect_uri,
        redirect_uri_valid,
        authorized_js_origin,
        secret_kind,
    ) = if let Some(cfg) = current.as_ref() {
        let client_id = cfg.client_id.trim();
        let redirect_uri = effective_google_redirect_uri(&state, &cfg.redirect_uri);
        let client_id_valid = is_valid_google_client_id(client_id);
        if !client_id_valid {
            issues.push("Client ID is invalid. Use a Web OAuth Client ID like digits-random.apps.googleusercontent.com.".to_string());
        }
        if client_id.contains("***") {
            issues.push("Client ID appears masked/truncated. Paste the full value.".to_string());
        }

        let redirect_uri_valid = is_valid_redirect_uri(&redirect_uri);
        if !redirect_uri_valid {
            issues.push("Redirect URI is invalid. Use an absolute http(s) URL.".to_string());
        }

        let secret_kind = google_client_secret_kind(&cfg.client_secret);
        if secret_kind == "service_account_key" {
            issues.push("Client Secret appears to be a service-account key. Use OAuth Web App Client Secret.".to_string());
        }

        (
            true,
            match cfg.source {
                GoogleOAuthConfigSource::Runtime => "runtime",
                GoogleOAuthConfigSource::EnvOrVault => "env_or_vault",
            },
            client_id_valid,
            redirect_uri.clone(),
            redirect_uri_valid,
            redirect_origin(&redirect_uri),
            secret_kind,
        )
    } else {
        issues.push("OAuth config missing. Set Client ID and Client Secret first.".to_string());
        let redirect_uri = default_google_redirect_uri(&state);
        (
            false,
            "none",
            false,
            redirect_uri.clone(),
            is_valid_redirect_uri(&redirect_uri),
            redirect_origin(&redirect_uri),
            "missing",
        )
    };

    axum::Json(serde_json::json!({
        "configured": configured,
        "source": source,
        "client_id_valid": client_id_valid,
        "redirect_uri": redirect_uri,
        "redirect_uri_valid": redirect_uri_valid,
        "authorized_redirect_uri": redirect_uri,
        "authorized_js_origin": authorized_js_origin,
        "required_oauth_scopes": required_oauth_scopes,
        "secret_kind": secret_kind,
        "issues": issues,
    }))
}

/// POST /api/integrations/google/config — set runtime OAuth client config.
async fn set_google_integration_config(
    axum::extract::State(state): axum::extract::State<SharedState>,
    axum::Json(body): axum::Json<SetGoogleIntegrationConfigRequest>,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let client_id = body.client_id.trim();
    let client_secret = body.client_secret.trim();

    if client_id.is_empty() || client_secret.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": "client_id and client_secret are required",
            })),
        );
    }

    if !is_valid_google_client_id(client_id) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": "Invalid Google Client ID format. Use a Web OAuth client ID like: 1234567890-abcdef.apps.googleusercontent.com",
            })),
        );
    }

    if client_id.contains("***") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": "Client ID appears masked/truncated. Paste the full Client ID value.",
            })),
        );
    }

    if google_client_secret_kind(client_secret) == "service_account_key" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": "Client Secret looks like a service-account private key. Use OAuth Web Application credentials (Client ID + Client Secret).",
            })),
        );
    }

    let redirect_uri = body
        .redirect_uri
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);

    if let Some(uri) = redirect_uri.as_deref()
        && !is_valid_redirect_uri(uri)
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": "Invalid redirect_uri. Use an absolute URL like http://127.0.0.1:3000/api/integrations/google/callback",
            })),
        );
    }

    // Normalize loopback callback URIs to this gateway's active port.
    let normalized_redirect_uri = redirect_uri
        .as_deref()
        .map(|uri| effective_google_redirect_uri(&state, uri));

    state.set_google_oauth_runtime_config(GoogleOAuthRuntimeConfig {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        redirect_uri: normalized_redirect_uri.clone(),
    });

    // Keep runtime credentials immediately available in-process.
    crate::google_secrets::set_runtime_secret("GOOGLE_CLIENT_ID", client_id);
    crate::google_secrets::set_runtime_secret("GOOGLE_CLIENT_SECRET", client_secret);
    let effective_redirect_uri = normalized_redirect_uri
        .clone()
        .unwrap_or_else(|| default_google_redirect_uri(&state));
    crate::google_secrets::set_runtime_secret("GOOGLE_REDIRECT_URI", &effective_redirect_uri);

    // Best effort persistence for next restart.
    let persisted_client_id = persist_api_key("GOOGLE_CLIENT_ID", client_id);
    let persisted_client_secret = persist_api_key("GOOGLE_CLIENT_SECRET", client_secret);
    let persisted_redirect_uri = if let Some(uri) = &normalized_redirect_uri {
        persist_api_key("GOOGLE_REDIRECT_URI", uri)
    } else {
        // Blank redirect means "use default derived from gateway host/port",
        // so clear any previously persisted override.
        persist_api_key("GOOGLE_REDIRECT_URI", "")
    };
    let persisted = persisted_client_id && persisted_client_secret && persisted_redirect_uri;

    (
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({
            "status": "ok",
            "configured": true,
            "client_id": mask_client_id(client_id),
            "redirect_uri": normalized_redirect_uri.unwrap_or_else(|| default_google_redirect_uri(&state)),
            "source": "runtime",
            "persisted": persisted,
            "message": if persisted {
                "OAuth configuration saved and persisted."
            } else {
                "OAuth configuration saved in memory, but not persisted. This deployment needs secure vault unlock (OS keychain or env passphrase)."
            },
        })),
    )
}

/// Known provider types that can be added at runtime.
const KNOWN_PROVIDERS: &[(&str, &str, bool)] = &[
    ("anthropic", "Anthropic", true),
    ("openai", "OpenAI", true),
    ("codex", "Codex OAuth", false),
    ("deepseek", "DeepSeek", true),
    ("mistral", "Mistral", true),
    ("sansa", "Sansa", true),
    ("gemini", "Google Gemini", true),
    ("falcon", "Falcon", true),
    ("jais", "Jais", true),
    ("qwen", "Qwen", true),
    ("yi", "Yi", true),
    ("cohere", "Cohere", true),
    ("minimax", "MiniMax", true),
    ("moonshot", "Moonshot K2", true),
    ("ollama", "Ollama", false),
];

/// GET /api/providers — list known provider types with activation status.
async fn list_providers(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> axum::Json<serde_json::Value> {
    let active_ids = state.agents.provider_ids();
    let default_id = state.agents.default_provider_id();

    let mut providers: Vec<serde_json::Value> = Vec::with_capacity(KNOWN_PROVIDERS.len());
    for (id, display, needs_key) in KNOWN_PROVIDERS {
        let active = active_ids.contains(&id.to_string());
        let mut model = None;
        let mut models = Vec::new();

        if active && let Some(provider) = state.agents.get_provider(id) {
            model = provider.configured_model().map(|m| m.to_string());
            match provider.available_models().await {
                Ok(mut available) => {
                    available.retain(|m| !m.trim().is_empty());
                    available.sort();
                    available.dedup();
                    models = available;
                }
                Err(err) => {
                    tracing::warn!("failed to list models for provider {}: {}", id, err);
                }
            }
        }

        if let Some(selected) = model.as_ref()
            && !models.iter().any(|m| m == selected)
        {
            models.insert(0, selected.clone());
        }

        providers.push(serde_json::json!({
            "id": id,
            "display_name": display,
            "active": active,
            "is_default": default_id.as_deref() == Some(*id),
            "needs_api_key": *needs_key,
            "model": model,
            "models": models,
        }));
    }

    axum::Json(serde_json::json!({ "providers": providers }))
}

#[derive(serde::Deserialize)]
struct AddProviderRequest {
    provider_type: String,
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    set_default: Option<bool>,
}

/// POST /api/providers — add a new LLM provider at runtime.
async fn add_provider(
    axum::extract::State(state): axum::extract::State<SharedState>,
    axum::Json(body): axum::Json<AddProviderRequest>,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let provider_type = body.provider_type.as_str();

    // Check if this provider type already exists
    let existing = state.agents.provider_ids();
    if existing.contains(&provider_type.to_string()) {
        // If requesting set_default, just switch
        if body.set_default == Some(true) {
            state.agents.set_default_provider_id(provider_type);
            return (
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({
                    "status": "ok",
                    "message": format!("switched default provider to {provider_type}"),
                })),
            );
        }
        return (
            axum::http::StatusCode::CONFLICT,
            axum::Json(serde_json::json!({
                "status": "error",
                "message": format!("provider '{provider_type}' is already active"),
            })),
        );
    }

    // Build and register the provider
    match provider_type {
        "anthropic" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for anthropic",
                    })),
                );
            };
            let provider = opencrust_agents::AnthropicProvider::new(
                key.clone(),
                body.model.clone(),
                body.base_url.clone(),
            );
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("ANTHROPIC_API_KEY", key);
        }
        "openai" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for openai",
                    })),
                );
            };
            let provider = opencrust_agents::OpenAiProvider::new(
                key.clone(),
                body.model.clone(),
                body.base_url.clone(),
            );
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("OPENAI_API_KEY", key);
        }
        "codex" => {
            let access_token = body
                .api_key
                .clone()
                .or_else(|| {
                    crate::bootstrap::default_vault_path().and_then(|vault_path| {
                        opencrust_security::try_vault_get(&vault_path, "CODEX_ACCESS_TOKEN")
                    })
                })
                .or_else(|| std::env::var("CODEX_ACCESS_TOKEN").ok());
            let refresh_token = crate::bootstrap::default_vault_path()
                .and_then(|vault_path| {
                    opencrust_security::try_vault_get(&vault_path, "CODEX_REFRESH_TOKEN")
                })
                .or_else(|| std::env::var("CODEX_REFRESH_TOKEN").ok());
            let account_id = crate::bootstrap::default_vault_path()
                .and_then(|vault_path| {
                    opencrust_security::try_vault_get(&vault_path, "CODEX_ACCOUNT_ID")
                })
                .or_else(|| std::env::var("CODEX_ACCOUNT_ID").ok());
            let id_token = crate::bootstrap::default_vault_path()
                .and_then(|vault_path| {
                    opencrust_security::try_vault_get(&vault_path, "CODEX_ID_TOKEN")
                })
                .or_else(|| std::env::var("CODEX_ID_TOKEN").ok());

            if access_token.is_none() && refresh_token.is_none() {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "codex oauth credentials not found; use Connect with Codex first",
                    })),
                );
            }

            let provider = CodexProvider::new(
                CodexAuthConfig {
                    access_token,
                    refresh_token,
                    account_id,
                    id_token,
                },
                body.model.clone(),
                body.base_url.clone(),
            );
            state.agents.register_provider(Arc::new(provider));
        }
        "sansa" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for sansa",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.sansaml.com".to_string()));
            let model = body
                .model
                .clone()
                .or_else(|| Some("sansa-auto".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("sansa");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("SANSA_API_KEY", key);
        }
        "deepseek" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for deepseek",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.deepseek.com".to_string()));
            let model = body
                .model
                .clone()
                .or_else(|| Some("deepseek-chat".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("deepseek");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("DEEPSEEK_API_KEY", key);
        }
        "mistral" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for mistral",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.mistral.ai".to_string()));
            let model = body
                .model
                .clone()
                .or_else(|| Some("mistral-large-latest".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("mistral");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("MISTRAL_API_KEY", key);
        }
        "gemini" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for gemini",
                    })),
                );
            };
            let base_url = body.base_url.clone().or_else(|| {
                Some("https://generativelanguage.googleapis.com/v1beta/openai/".to_string())
            });
            let model = body
                .model
                .clone()
                .or_else(|| Some("gemini-2.5-flash".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("gemini");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("GEMINI_API_KEY", key);
        }
        "falcon" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for falcon",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.ai71.ai/v1".to_string()));
            let model = body
                .model
                .clone()
                .or_else(|| Some("tiiuae/falcon-180b-chat".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("falcon");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("FALCON_API_KEY", key);
        }
        "jais" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for jais",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.core42.ai/v1".to_string()));
            let model = body
                .model
                .clone()
                .or_else(|| Some("jais-adapted-70b-chat".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("jais");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("JAIS_API_KEY", key);
        }
        "qwen" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for qwen",
                    })),
                );
            };
            let base_url = body.base_url.clone().or_else(|| {
                Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1".to_string())
            });
            let model = body.model.clone().or_else(|| Some("qwen-plus".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("qwen");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("QWEN_API_KEY", key);
        }
        "yi" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for yi",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.lingyiwanwu.com/v1".to_string()));
            let model = body.model.clone().or_else(|| Some("yi-large".to_string()));
            let provider =
                opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url).with_name("yi");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("YI_API_KEY", key);
        }
        "cohere" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for cohere",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.cohere.com/compatibility/v1".to_string()));
            let model = body
                .model
                .clone()
                .or_else(|| Some("command-r-plus".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("cohere");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("COHERE_API_KEY", key);
        }
        "minimax" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for minimax",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.minimaxi.chat/v1".to_string()));
            let model = body
                .model
                .clone()
                .or_else(|| Some("MiniMax-Text-01".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("minimax");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("MINIMAX_API_KEY", key);
        }
        "moonshot" => {
            let Some(key) = &body.api_key else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "status": "error",
                        "message": "api_key is required for moonshot",
                    })),
                );
            };
            let base_url = body
                .base_url
                .clone()
                .or_else(|| Some("https://api.moonshot.cn/v1".to_string()));
            let model = body
                .model
                .clone()
                .or_else(|| Some("kimi-k2-0711-preview".to_string()));
            let provider = opencrust_agents::OpenAiProvider::new(key.clone(), model, base_url)
                .with_name("moonshot");
            state.agents.register_provider(Arc::new(provider));
            persist_api_key("MOONSHOT_API_KEY", key);
        }
        "ollama" => {
            let provider =
                opencrust_agents::OllamaProvider::new(body.model.clone(), body.base_url.clone());
            state.agents.register_provider(Arc::new(provider));
        }
        other => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "status": "error",
                    "message": format!("unknown provider type: {other}"),
                })),
            );
        }
    }

    if body.set_default == Some(true) {
        state.agents.set_default_provider_id(provider_type);
    }

    (
        axum::http::StatusCode::CREATED,
        axum::Json(serde_json::json!({
            "status": "ok",
            "message": format!("provider '{provider_type}' activated"),
        })),
    )
}

/// Best-effort: persist an API key in the vault.
fn persist_api_key(vault_key: &str, value: &str) -> bool {
    persist_secret(vault_key, value)
}

fn persist_secret(vault_key: &str, value: &str) -> bool {
    if let Some(vault_path) = crate::bootstrap::default_vault_path()
        && opencrust_security::try_vault_set(&vault_path, vault_key, value)
    {
        return true;
    }
    if vault_key.starts_with("CODEX_") {
        return crate::bootstrap::persist_auth_json_secret(vault_key, value);
    }
    false
}

fn google_integration_status_json(state: &SharedState) -> serde_json::Value {
    let oauth = google_oauth_config(state);
    let configured = oauth.is_some();
    let connected = state.google_workspace_connected() || google_refresh_token_available();
    serde_json::json!({
        "id": "google_workspace",
        "connected": connected,
        "email": state.google_workspace_email(),
        "auth_configured": configured,
        "auth_source": oauth.map(|cfg| match cfg.source {
            GoogleOAuthConfigSource::Runtime => "runtime",
            GoogleOAuthConfigSource::EnvOrVault => "env_or_vault",
        }),
    })
}

fn google_oauth_config(state: &SharedState) -> Option<GoogleOAuthConfig> {
    if let Some(runtime) = state.google_oauth_runtime_config()
        && !runtime.client_id.trim().is_empty()
        && !runtime.client_secret.trim().is_empty()
    {
        let redirect_uri = runtime
            .redirect_uri
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| default_google_redirect_uri(state));

        return Some(GoogleOAuthConfig {
            client_id: runtime.client_id,
            client_secret: runtime.client_secret,
            redirect_uri,
            source: GoogleOAuthConfigSource::Runtime,
        });
    }

    let client_id = google_oauth_secret("GOOGLE_CLIENT_ID")?;
    let client_secret = google_oauth_secret("GOOGLE_CLIENT_SECRET")?;
    if client_id.trim().is_empty() || client_secret.trim().is_empty() {
        return None;
    }
    let redirect_uri = google_oauth_secret("GOOGLE_REDIRECT_URI")
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default_google_redirect_uri(state));

    Some(GoogleOAuthConfig {
        client_id,
        client_secret,
        redirect_uri,
        source: GoogleOAuthConfigSource::EnvOrVault,
    })
}

fn google_oauth_secret(key: &str) -> Option<String> {
    crate::google_secrets::get_runtime_secret(key)
        .or_else(|| {
            crate::bootstrap::default_vault_path()
                .and_then(|path| opencrust_security::try_vault_get(&path, key))
        })
        .or_else(|| std::env::var(key).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .filter(|v| !looks_like_placeholder_secret(v))
}

fn looks_like_placeholder_secret(value: &str) -> bool {
    let trimmed = value.trim();
    (trimmed.starts_with("your_") && trimmed.ends_with("_here"))
        || trimmed == "set_a_long_random_passphrase_here"
}

fn google_refresh_token_available() -> bool {
    google_oauth_secret("GOOGLE_WORKSPACE_REFRESH_TOKEN").is_some()
}

fn default_google_redirect_uri(state: &SharedState) -> String {
    let host = match state.config.gateway.host.trim() {
        "localhost" => "localhost",
        _ => "127.0.0.1",
    };
    format!(
        "http://{host}:{}/api/integrations/google/callback",
        state.config.gateway.port
    )
}

fn mask_client_id(client_id: &str) -> String {
    let trimmed = client_id.trim();
    if trimmed.len() <= 10 {
        return "***".to_string();
    }
    format!("{}***{}", &trimmed[..6], &trimmed[trimmed.len() - 4..])
}

fn is_valid_google_client_id(value: &str) -> bool {
    let trimmed = value.trim();
    let has_domain = trimmed.ends_with(".apps.googleusercontent.com");
    let has_dash = trimmed.contains('-');
    let parts: Vec<&str> = trimmed.splitn(2, '-').collect();
    let numeric_prefix = parts
        .first()
        .map(|p| p.chars().all(|c| c.is_ascii_digit()) && p.len() >= 6)
        .unwrap_or(false);
    has_domain && has_dash && numeric_prefix
}

fn oauth_popup_result_with_event(
    success: bool,
    message: &str,
    event_type: &str,
    success_title: &str,
    failure_title: &str,
    target_origin: Option<&str>,
) -> Html<String> {
    let escaped_message = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let json_message = serde_json::to_string(message).unwrap_or_else(|_| "\"\"".to_string());
    let title = if success {
        success_title
    } else {
        failure_title
    };
    let status = if success { "success" } else { "error" };
    let payload = if success { "true" } else { "false" };
    let target_origin_json =
        serde_json::to_string(&target_origin).unwrap_or_else(|_| "null".to_string());

    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <style>
    body {{
      margin: 0;
      min-height: 100vh;
      display: grid;
      place-items: center;
      font-family: "Segoe UI", sans-serif;
      background: #f4efe6;
      color: #2f2416;
    }}
    .panel {{
      max-width: 480px;
      margin: 24px;
      padding: 24px;
      border-radius: 14px;
      background: #fffaf1;
      border: 1px solid #d8c3a5;
      box-shadow: 0 12px 28px rgba(73, 49, 22, 0.18);
    }}
    h1 {{
      margin: 0 0 8px;
      font-size: 1.2rem;
    }}
    p {{
      margin: 0 0 14px;
      line-height: 1.45;
    }}
    .{status} {{
      color: #2d7e4d;
    }}
    .error {{
      color: #9b3b24;
    }}
  </style>
</head>
<body>
  <div class="panel">
    <h1 class="{status}">{title}</h1>
    <p>{escaped_message}</p>
    <p>You can close this window.</p>
  </div>
  <script>
    try {{
      if (window.opener && !window.opener.closed) {{
        const targetOrigin = {target_origin_json} ?? window.location.origin;
        window.opener.postMessage({{ type: {event_type:?}, success: {payload}, message: {json_message} }}, targetOrigin);
      }}
    }} catch (e) {{}}
    setTimeout(() => window.close(), 600);
  </script>
</body>
</html>"#
    ))
}

fn oauth_popup_result(success: bool, message: &str) -> Html<String> {
    oauth_popup_result_with_event(
        success,
        message,
        "opencrust.google.oauth",
        "Google Connected",
        "Google Connection Failed",
        None,
    )
}

fn codex_oauth_popup_result(
    success: bool,
    message: &str,
    target_origin: Option<&str>,
) -> Html<String> {
    oauth_popup_result_with_event(
        success,
        message,
        "opencrust.codex.oauth",
        "Codex Connected",
        "Codex Connection Failed",
        target_origin,
    )
}

/// GET /api/mcp — list connected MCP servers with tool counts and status.
async fn list_mcp_servers(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> axum::Json<serde_json::Value> {
    let servers = if let Some(mgr) = &state.mcp_manager_arc {
        let list = mgr.list_servers().await;
        list.into_iter()
            .map(|(name, tool_count, connected)| {
                serde_json::json!({
                    "name": name,
                    "tools": tool_count,
                    "connected": connected,
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    axum::Json(serde_json::json!({ "servers": servers }))
}

/// Read the cached latest version from ~/.opencrust/update-check.json.
fn read_cached_latest_version() -> Option<String> {
    let path = opencrust_config::ConfigLoader::default_config_dir().join("update-check.json");
    let contents = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&contents).ok()?;
    v.get("latest_version")?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::{Mutex, OnceLock};

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use opencrust_agents::AgentRuntime;
    use opencrust_channels::ChannelRegistry;
    use opencrust_config::AppConfig;
    use tower::ServiceExt;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build")
            .block_on(future)
    }

    fn test_state(api_key: Option<&str>) -> SharedState {
        let mut config = AppConfig::default();
        config.gateway.api_key = api_key.map(ToString::to_string);
        Arc::new(crate::state::AppState::new(
            config,
            AgentRuntime::new(),
            ChannelRegistry::new(),
        ))
    }

    fn protected_router(state: SharedState) -> Router {
        Router::new()
            .route("/protected", get(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_gateway_api_key,
            ))
            .with_state(state)
    }

    #[test]
    fn middleware_rejects_when_no_gateway_key_configured() {
        let state = test_state(None);
        let router = protected_router(state);
        let resp = block_on(
            router.oneshot(
                Request::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .expect("request should complete");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn middleware_rejects_wrong_key_accepts_valid_header_and_query() {
        let state = test_state(Some("secret-token"));
        let router = protected_router(state);

        let missing = block_on(
            router.clone().oneshot(
                Request::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .expect("request should complete");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let wrong = block_on(
            router.clone().oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(axum::http::header::AUTHORIZATION, "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .expect("request should complete");
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let header_ok = block_on(
            router.clone().oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(axum::http::header::AUTHORIZATION, "Bearer secret-token")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .expect("request should complete");
        assert_eq!(header_ok.status(), StatusCode::OK);

        let query_ok = block_on(
            router.clone().oneshot(
                Request::builder()
                    .uri("/protected?token=secret-token")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .expect("request should complete");
        assert_eq!(query_ok.status(), StatusCode::OK);
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_token_eq("abc123", "abc123"));
        assert!(!constant_time_token_eq("abc123", "abc124"));
        assert!(!constant_time_token_eq("abc123", "abc1234"));
    }

    #[test]
    fn valid_google_client_id() {
        assert!(is_valid_google_client_id(
            "1234567890-abcdef.apps.googleusercontent.com"
        ));
        assert!(!is_valid_google_client_id("not-a-client-id"));
        assert!(!is_valid_google_client_id(
            "abc123-abcdef.apps.googleusercontent.com"
        ));
    }

    #[test]
    fn valid_redirect_uri() {
        assert!(is_valid_redirect_uri("http://127.0.0.1:3888/callback"));
        assert!(is_valid_redirect_uri("https://example.com/callback"));
        assert!(!is_valid_redirect_uri(""));
        assert!(!is_valid_redirect_uri("not a url"));
    }

    #[test]
    fn mask_client_id_works() {
        assert_eq!(mask_client_id("1234567890"), "***");
        assert_eq!(
            mask_client_id("1234567890-abcdef.apps.googleusercontent.com"),
            "123456***.com"
        );
    }

    #[test]
    fn default_redirect_uri_uses_gateway_port() {
        let mut config = AppConfig::default();
        config.gateway.host = "localhost".to_string();
        config.gateway.port = 4555;
        let state = Arc::new(crate::state::AppState::new(
            config,
            AgentRuntime::new(),
            ChannelRegistry::new(),
        ));
        assert_eq!(
            default_google_redirect_uri(&state),
            "http://localhost:4555/api/integrations/google/callback"
        );
    }

    #[test]
    fn gmail_send_scope_defaults_false() {
        let _guard = env_lock().lock().expect("env lock");
        let old = std::env::var("OPENCRUST_GOOGLE_ENABLE_GMAIL_SEND_SCOPE").ok();

        // SAFETY: test-only env mutation.
        unsafe { std::env::remove_var("OPENCRUST_GOOGLE_ENABLE_GMAIL_SEND_SCOPE") };
        assert!(!google_gmail_send_scope_enabled());

        // SAFETY: test-only env mutation.
        unsafe { std::env::set_var("OPENCRUST_GOOGLE_ENABLE_GMAIL_SEND_SCOPE", "true") };
        assert!(google_gmail_send_scope_enabled());

        match old {
            Some(v) => unsafe { std::env::set_var("OPENCRUST_GOOGLE_ENABLE_GMAIL_SEND_SCOPE", v) },
            None => unsafe { std::env::remove_var("OPENCRUST_GOOGLE_ENABLE_GMAIL_SEND_SCOPE") },
        }
    }

    #[test]
    fn placeholder_secret_detection() {
        assert!(looks_like_placeholder_secret(
            "your_google_client_secret_here"
        ));
        assert!(looks_like_placeholder_secret(
            "set_a_long_random_passphrase_here"
        ));
        assert!(!looks_like_placeholder_secret("real-secret"));
    }

    #[test]
    fn oauth_popup_escapes_html() {
        let html = oauth_popup_result(true, "<script>alert('xss')</script>").0;
        assert!(html.contains("&lt;script&gt;"));
        // The escaped message appears inside <p> tags — no raw script injection.
        assert!(
            html.contains("<p>&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;</p>")
                || html.contains("<p>&lt;script&gt;alert('xss')&lt;/script&gt;</p>")
        );
    }
}
