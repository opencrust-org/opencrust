use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::Stream;
use futures::stream;
use opencrust_common::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{instrument, warn};

use crate::providers::{
    ChatMessage, ChatRole, ContentBlock, LlmProvider, LlmRequest, LlmResponse, MessagePart,
    StreamEvent, Usage,
};

const DEFAULT_MODEL: &str = "gpt-5.3-codex";
const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const MODELS_CLIENT_VERSION: &str = "0.1.0";
const OAUTH_ISSUER: &str = "https://auth.openai.com";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

#[derive(Debug, Clone)]
pub struct CodexAuthConfig {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub id_token: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexOAuthClaims {
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone)]
struct CodexAuthState {
    access_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
    id_token: Option<String>,
}

pub struct CodexProvider {
    client: reqwest::Client,
    model: String,
    base_url: String,
    auth: Arc<RwLock<CodexAuthState>>,
}

impl CodexProvider {
    pub fn new(auth: CodexAuthConfig, model: Option<String>, base_url: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            auth: Arc::new(RwLock::new(CodexAuthState {
                access_token: auth.access_token.filter(|v| !v.trim().is_empty()),
                refresh_token: auth.refresh_token.filter(|v| !v.trim().is_empty()),
                account_id: auth.account_id.filter(|v| !v.trim().is_empty()),
                id_token: auth.id_token.filter(|v| !v.trim().is_empty()),
            })),
        }
    }

    fn responses_endpoint(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    fn models_endpoint(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    fn read_auth(&self) -> CodexAuthState {
        self.auth.read().unwrap().clone()
    }

    async fn ensure_access_token(&self) -> Result<CodexAuthState> {
        let auth = self.read_auth();
        if auth.access_token.is_some() {
            return Ok(auth);
        }
        self.refresh_access_token().await
    }

    async fn refresh_access_token(&self) -> Result<CodexAuthState> {
        let refresh_token = self
            .read_auth()
            .refresh_token
            .ok_or_else(|| Error::Agent("codex oauth refresh token missing".to_string()))?;

        let response = self
            .client
            .post(format!("{OAUTH_ISSUER}/oauth/token"))
            .header("content-type", "application/json")
            .json(&serde_json::json!({
                "client_id": OAUTH_CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            }))
            .send()
            .await
            .map_err(|e| Error::Agent(format!("codex token refresh failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "codex token refresh failed ({status}): {body}"
            )));
        }

        let refresh = response.json::<RefreshResponse>().await.map_err(|e| {
            Error::Agent(format!("failed to parse codex token refresh response: {e}"))
        })?;

        let claims = refresh
            .id_token
            .as_deref()
            .and_then(|token| parse_codex_id_token_claims(token).ok());

        let mut guard = self.auth.write().unwrap();
        guard.access_token = refresh.access_token.or_else(|| guard.access_token.clone());
        if let Some(next_refresh) = refresh.refresh_token {
            guard.refresh_token = Some(next_refresh);
        }
        if let Some(id_token) = refresh.id_token {
            guard.id_token = Some(id_token);
        }
        if guard.account_id.is_none() {
            guard.account_id = claims.and_then(|c| c.account_id);
        }
        Ok(guard.clone())
    }

    async fn send_responses_request(
        &self,
        request: &LlmRequest,
        stream: bool,
    ) -> Result<reqwest::Response> {
        let mut auth = self.ensure_access_token().await?;
        let body = self.build_request(request, stream);

        for attempt in 0..2 {
            let mut builder = self
                .client
                .post(self.responses_endpoint())
                .header("content-type", "application/json")
                .bearer_auth(
                    auth.access_token
                        .as_deref()
                        .ok_or_else(|| Error::Agent("codex access token missing".to_string()))?,
                )
                .json(&body);

            if let Some(account_id) = auth.account_id.as_deref() {
                builder = builder.header("chatgpt-account-id", account_id);
            }
            if stream {
                builder = builder.header("accept", "text/event-stream");
            }

            let response = builder
                .send()
                .await
                .map_err(|e| Error::Agent(format!("codex request failed: {e}")))?;

            if response.status() != reqwest::StatusCode::UNAUTHORIZED || attempt == 1 {
                return Ok(response);
            }

            auth = self.refresh_access_token().await?;
        }

        Err(Error::Agent(
            "codex request failed after refresh retry".to_string(),
        ))
    }

    fn build_request(&self, request: &LlmRequest, stream: bool) -> Value {
        let model = if request.model.trim().is_empty() {
            self.model.clone()
        } else {
            request.model.trim().to_string()
        };

        let mut input = Vec::new();
        for msg in &request.messages {
            self.push_message_items(&mut input, msg);
        }

        let tools = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                })
            })
            .collect::<Vec<_>>();

        serde_json::json!({
            "model": model,
            "instructions": request.system.clone().unwrap_or_default(),
            "input": input,
            "tools": tools,
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "reasoning": serde_json::Value::Null,
            "store": false,
            "stream": stream,
            "include": Vec::<String>::new(),
        })
    }

    fn push_message_items(&self, input: &mut Vec<Value>, msg: &ChatMessage) {
        match (&msg.role, &msg.content) {
            (ChatRole::System, _) => {}
            (ChatRole::User, MessagePart::Text(text)) => {
                input.push(message_item("user", vec![input_text(text)]));
            }
            (ChatRole::User, MessagePart::Parts(parts)) => {
                let mut content = Vec::new();
                let mut has_tool_results = false;
                for part in parts {
                    match part {
                        ContentBlock::Text { text } => content.push(input_text(text)),
                        ContentBlock::Image { url } => content.push(input_image(url)),
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                        } => {
                            has_tool_results = true;
                            input.push(serde_json::json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": content,
                            }));
                        }
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    input.push(message_item("user", content));
                } else if !has_tool_results {
                    input.push(message_item("user", Vec::new()));
                }
            }
            (ChatRole::Assistant, MessagePart::Text(text)) => {
                input.push(message_output_item("assistant", vec![output_text(text)]));
            }
            (ChatRole::Assistant, MessagePart::Parts(parts)) => {
                let mut content = Vec::new();
                for part in parts {
                    match part {
                        ContentBlock::Text { text } => content.push(output_text(text)),
                        ContentBlock::ToolUse {
                            id,
                            name,
                            input: args,
                        } => {
                            input.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string()),
                            }));
                        }
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    input.push(message_output_item("assistant", content));
                }
            }
            (ChatRole::Tool, MessagePart::Text(text)) => {
                input.push(message_item("tool", vec![input_text(text)]));
            }
            (ChatRole::Tool, MessagePart::Parts(_)) => {}
        }
    }

    fn parse_usage(response: &Value) -> Option<Usage> {
        let usage = response.get("usage")?;
        let input_tokens = usage.get("input_tokens")?.as_u64()? as u32;
        let output_tokens = usage.get("output_tokens")?.as_u64()? as u32;
        Some(Usage {
            input_tokens,
            output_tokens,
        })
    }

    fn parse_output_item(item: &Value) -> Vec<ContentBlock> {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                item.get("content")
                    .and_then(Value::as_array)
                    .map(|content| {
                        content
                            .iter()
                            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                                Some("output_text") => part
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .map(|text| ContentBlock::Text {
                                        text: text.to_string(),
                                    }),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            }
            Some("function_call") => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let input = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                if call_id.is_empty() || name.is_empty() {
                    Vec::new()
                } else {
                    vec![ContentBlock::ToolUse {
                        id: call_id,
                        name,
                        input,
                    }]
                }
            }
            _ => Vec::new(),
        }
    }

    fn parse_non_streaming_response(body: &Value) -> LlmResponse {
        let output = body
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(Self::parse_output_item)
            .collect::<Vec<_>>();

        LlmResponse {
            content: output,
            model: body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_MODEL)
                .to_string(),
            usage: Self::parse_usage(body),
            stop_reason: body
                .get("status")
                .and_then(Value::as_str)
                .map(|s| s.to_string()),
        }
    }

    fn response_from_stream_events(
        &self,
        request: &LlmRequest,
        events: &[StreamEvent],
    ) -> LlmResponse {
        let model = if request.model.trim().is_empty() {
            self.model.clone()
        } else {
            request.model.trim().to_string()
        };

        let mut content = Vec::new();
        let mut text = String::new();
        let mut usage = None;
        let mut stop_reason = None;
        let mut pending_tool: Option<(String, String, String)> = None;

        for event in events {
            match event {
                StreamEvent::TextDelta(delta) => {
                    text.push_str(delta);
                }
                StreamEvent::ToolUseStart { id, name, .. } => {
                    if !text.is_empty() {
                        content.push(ContentBlock::Text {
                            text: std::mem::take(&mut text),
                        });
                    }
                    pending_tool = Some((id.clone(), name.clone(), String::new()));
                }
                StreamEvent::InputJsonDelta(delta) => {
                    if let Some((_, _, arguments)) = pending_tool.as_mut() {
                        arguments.push_str(delta);
                    }
                }
                StreamEvent::ContentBlockStop { .. } => {
                    if let Some((id, name, arguments)) = pending_tool.take() {
                        let input = serde_json::from_str(&arguments)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        content.push(ContentBlock::ToolUse { id, name, input });
                    }
                }
                StreamEvent::MessageDelta {
                    stop_reason: next_stop_reason,
                    usage: next_usage,
                } => {
                    if next_stop_reason.is_some() {
                        stop_reason = next_stop_reason.clone();
                    }
                    if next_usage.is_some() {
                        usage = next_usage.clone();
                    }
                }
                StreamEvent::MessageStop => {}
            }
        }

        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
        if let Some((id, name, arguments)) = pending_tool.take() {
            let input = serde_json::from_str(&arguments).unwrap_or_else(|_| serde_json::json!({}));
            content.push(ContentBlock::ToolUse { id, name, input });
        }

        LlmResponse {
            content,
            model,
            usage,
            stop_reason,
        }
    }
}

#[async_trait]
impl LlmProvider for CodexProvider {
    fn provider_id(&self) -> &str {
        "codex"
    }

    fn configured_model(&self) -> Option<&str> {
        Some(&self.model)
    }

    #[instrument(skip(self, request), fields(model))]
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let response = self.send_responses_request(request, false).await?;
        let status = response.status();
        let body = response
            .json::<Value>()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse codex response body: {e}")))?;

        if !status.is_success() {
            return Err(Error::Agent(format!(
                "codex request failed ({status}): {body}"
            )));
        }

        Ok(Self::parse_non_streaming_response(&body))
    }

    async fn stream_complete(
        &self,
        request: &LlmRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let response = self.send_responses_request(request, true).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "codex streaming request failed ({status}): {body}"
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Agent(format!("failed to read codex SSE body: {e}")))?;
        let events = parse_sse_events(bytes.as_ref())?;
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }

    async fn available_models(&self) -> Result<Vec<String>> {
        let auth = self.ensure_access_token().await?;
        let token = auth
            .access_token
            .ok_or_else(|| Error::Agent("codex access token missing".to_string()))?;
        let mut request = self
            .client
            .get(format!(
                "{}?client_version={MODELS_CLIENT_VERSION}",
                self.models_endpoint()
            ))
            .bearer_auth(token);
        if let Some(account_id) = auth.account_id {
            request = request.header("chatgpt-account-id", account_id);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Error::Agent(format!("failed to load codex models: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "failed to load codex models ({status}): {body}"
            )));
        }

        let body = response
            .json::<Value>()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse codex models response: {e}")))?;

        Ok(body
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|model| model.get("slug").and_then(Value::as_str))
            .map(|slug| slug.to_string())
            .collect())
    }

    async fn health_check(&self) -> Result<bool> {
        Ok(self.ensure_access_token().await.is_ok())
    }
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

fn message_item(role: &str, content: Vec<Value>) -> Value {
    serde_json::json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

fn message_output_item(role: &str, content: Vec<Value>) -> Value {
    serde_json::json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

fn input_text(text: &str) -> Value {
    serde_json::json!({
        "type": "input_text",
        "text": text,
    })
}

fn output_text(text: &str) -> Value {
    serde_json::json!({
        "type": "output_text",
        "text": text,
    })
}

fn input_image(url: &str) -> Value {
    serde_json::json!({
        "type": "input_image",
        "image_url": url,
    })
}

pub fn parse_codex_id_token_claims(token: &str) -> std::result::Result<CodexOAuthClaims, String> {
    use base64::Engine;

    let mut parts = token.split('.');
    let (_header, payload, _sig) = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => (h, p, s),
        _ => return Err("invalid id token format".to_string()),
    };

    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| format!("failed to decode id token payload: {e}"))?;
    let payload: Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| format!("failed to parse id token payload: {e}"))?;

    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object);
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);

    Ok(CodexOAuthClaims {
        email: payload
            .get("email")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .or_else(|| {
                profile
                    .and_then(|profile| profile.get("email"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
            }),
        account_id: auth
            .and_then(|auth| auth.get("chatgpt_account_id"))
            .and_then(Value::as_str)
            .map(|s| s.to_string()),
        user_id: auth
            .and_then(|auth| auth.get("chatgpt_user_id").or_else(|| auth.get("user_id")))
            .and_then(Value::as_str)
            .map(|s| s.to_string()),
        plan_type: auth
            .and_then(|auth| auth.get("chatgpt_plan_type"))
            .and_then(Value::as_str)
            .map(|s| s.to_string()),
    })
}

fn parse_sse_events(bytes: &[u8]) -> Result<Vec<StreamEvent>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Error::Agent(format!("invalid UTF-8 in codex SSE body: {e}")))?;
    let mut out = Vec::new();
    let mut saw_text_delta = false;

    for chunk in text.split("\n\n") {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }

        let mut event_name = None::<String>;
        let mut data_lines = Vec::new();
        for line in chunk.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event_name = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.trim_start().to_string());
            }
        }

        let Some(kind) = event_name else {
            continue;
        };

        let payload = if data_lines.is_empty() {
            None
        } else {
            let joined = data_lines.join("\n");
            Some(
                serde_json::from_str::<Value>(&joined)
                    .map_err(|e| Error::Agent(format!("failed to parse codex SSE payload: {e}")))?,
            )
        };

        match kind.as_str() {
            "response.output_text.delta" => {
                if let Some(delta) = payload
                    .as_ref()
                    .and_then(|v| v.get("delta"))
                    .and_then(Value::as_str)
                {
                    saw_text_delta = true;
                    out.push(StreamEvent::TextDelta(delta.to_string()));
                }
            }
            "response.output_item.done" => {
                let Some(item) = payload.as_ref().and_then(|v| v.get("item")) else {
                    continue;
                };
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        let id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let arguments = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_string();
                        if !id.is_empty() && !name.is_empty() {
                            out.push(StreamEvent::ToolUseStart { index: 0, id, name });
                            out.push(StreamEvent::InputJsonDelta(arguments));
                            out.push(StreamEvent::ContentBlockStop { index: 0 });
                        }
                    }
                    Some("message") if !saw_text_delta => {
                        if let Some(content) = item.get("content").and_then(Value::as_array) {
                            let text = content
                                .iter()
                                .filter(|part| {
                                    part.get("type").and_then(Value::as_str) == Some("output_text")
                                })
                                .filter_map(|part| part.get("text").and_then(Value::as_str))
                                .collect::<String>();
                            if !text.is_empty() {
                                out.push(StreamEvent::TextDelta(text));
                            }
                        }
                    }
                    _ => {}
                }
            }
            "response.failed" => {
                let details = payload
                    .as_ref()
                    .and_then(|v| v.get("response"))
                    .and_then(|v| v.get("error"))
                    .and_then(|v| v.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("codex response failed");
                return Err(Error::Agent(details.to_string()));
            }
            "response.completed" => {
                let usage = payload
                    .as_ref()
                    .and_then(|v| v.get("response"))
                    .and_then(CodexProvider::parse_usage);
                out.push(StreamEvent::MessageDelta {
                    stop_reason: Some("end_turn".to_string()),
                    usage,
                });
                out.push(StreamEvent::MessageStop);
            }
            _ => {}
        }
    }

    if !out
        .iter()
        .any(|event| matches!(event, StreamEvent::MessageStop))
    {
        warn!("codex SSE stream ended without response.completed");
        out.push(StreamEvent::MessageStop);
    }

    Ok(out)
}
