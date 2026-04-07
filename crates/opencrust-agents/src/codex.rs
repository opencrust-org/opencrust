use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::{collections::VecDeque, str};

use async_trait::async_trait;
use futures::{Stream, StreamExt};
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
    oauth_issuer: String,
    auth: Arc<RwLock<CodexAuthState>>,
}

impl CodexProvider {
    pub fn new(auth: CodexAuthConfig, model: Option<String>, base_url: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            oauth_issuer: OAUTH_ISSUER.to_string(),
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
            .post(format!(
                "{}/oauth/token",
                self.oauth_issuer.trim_end_matches('/')
            ))
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
                input.push(message_item("assistant", vec![output_text(text)]));
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
                    input.push(message_item("assistant", content));
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
        let response = self.send_responses_request(request, true).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "codex request failed ({status}): {body}"
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Agent(format!("failed to read codex SSE body: {e}")))?;
        let events = parse_sse_events(bytes.as_ref())?;
        Ok(self.response_from_stream_events(request, &events))
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

        let byte_stream: Pin<
            Box<dyn Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send>,
        > = Box::pin(response.bytes_stream());

        let event_stream = futures::stream::unfold(
            (
                byte_stream,
                String::new(),
                false,
                false,
                VecDeque::<Result<StreamEvent>>::new(),
            ),
            |(mut stream, mut buffer, mut saw_text_delta, mut saw_message_stop, mut pending)| async move {
                loop {
                    if let Some(next) = pending.pop_front() {
                        return Some((
                            next,
                            (stream, buffer, saw_text_delta, saw_message_stop, pending),
                        ));
                    }

                    if let Some(pos) = buffer.find("\n\n") {
                        let chunk = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();

                        match parse_sse_chunk(&chunk, &mut saw_text_delta) {
                            Ok(events) => {
                                for event in events {
                                    if matches!(event, StreamEvent::MessageStop) {
                                        saw_message_stop = true;
                                    }
                                    pending.push_back(Ok(event));
                                }
                                continue;
                            }
                            Err(err) => {
                                return Some((
                                    Err(err),
                                    (stream, buffer, saw_text_delta, true, pending),
                                ));
                            }
                        }
                    }

                    match stream.next().await {
                        Some(Ok(bytes)) => match str::from_utf8(&bytes) {
                            Ok(text) => buffer.push_str(text),
                            Err(e) => {
                                return Some((
                                    Err(Error::Agent(format!(
                                        "invalid UTF-8 in codex SSE body: {e}"
                                    ))),
                                    (stream, buffer, saw_text_delta, true, pending),
                                ));
                            }
                        },
                        Some(Err(e)) => {
                            return Some((
                                Err(Error::Agent(format!("failed to read codex SSE body: {e}"))),
                                (stream, buffer, saw_text_delta, true, pending),
                            ));
                        }
                        None if !saw_message_stop => {
                            warn!("codex SSE stream ended without response.completed");
                            saw_message_stop = true;
                            return Some((
                                Ok(StreamEvent::MessageStop),
                                (stream, buffer, saw_text_delta, saw_message_stop, pending),
                            ));
                        }
                        None => return None,
                    }
                }
            },
        );

        Ok(Box::pin(event_stream))
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
    let text = str::from_utf8(bytes)
        .map_err(|e| Error::Agent(format!("invalid UTF-8 in codex SSE body: {e}")))?;
    let mut out = Vec::new();
    let mut saw_text_delta = false;

    for chunk in text.split("\n\n") {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        out.extend(parse_sse_chunk(chunk, &mut saw_text_delta)?);
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

fn parse_sse_chunk(chunk: &str, saw_text_delta: &mut bool) -> Result<Vec<StreamEvent>> {
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
        return Ok(Vec::new());
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

    let mut out = Vec::new();
    match kind.as_str() {
        "response.output_text.delta" => {
            if let Some(delta) = payload
                .as_ref()
                .and_then(|v| v.get("delta"))
                .and_then(Value::as_str)
            {
                *saw_text_delta = true;
                out.push(StreamEvent::TextDelta(delta.to_string()));
            }
        }
        "response.output_item.done" => {
            let Some(item) = payload.as_ref().and_then(|v| v.get("item")) else {
                return Ok(out);
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
                Some("message") if !*saw_text_delta => {
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

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, body::Body, http::StatusCode, response::Response, routing::post};
    use base64::Engine;
    use bytes::Bytes;
    use std::net::SocketAddr;
    use std::{convert::Infallible, time::Duration};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    fn test_provider(auth: CodexAuthConfig) -> CodexProvider {
        CodexProvider {
            client: reqwest::Client::new(),
            model: DEFAULT_MODEL.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            oauth_issuer: OAUTH_ISSUER.to_string(),
            auth: Arc::new(RwLock::new(CodexAuthState {
                access_token: auth.access_token,
                refresh_token: auth.refresh_token,
                account_id: auth.account_id,
                id_token: auth.id_token,
            })),
        }
    }

    fn id_token_with_account(account_id: &str) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            r#"{{"https://api.openai.com/auth":{{"chatgpt_account_id":"{account_id}","chatgpt_user_id":"user-123","chatgpt_plan_type":"plus"}}}}"#
        ));
        format!("{header}.{payload}.signature")
    }

    async fn spawn_test_server(app: Router) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("test listener addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("run test server");
        });
        addr
    }

    #[test]
    fn parse_sse_events_parses_text_tool_and_usage() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"delta\":\"Hello\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"lookup\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":34}}}\n\n"
        );

        let events = parse_sse_events(body.as_bytes()).expect("parse SSE events");

        assert_eq!(events.len(), 6);
        assert!(matches!(&events[0], StreamEvent::TextDelta(text) if text == "Hello"));
        assert!(matches!(
            &events[1],
            StreamEvent::ToolUseStart { index: 0, id, name }
            if id == "call-1" && name == "lookup"
        ));
        assert!(matches!(
            &events[2],
            StreamEvent::InputJsonDelta(arguments) if arguments == "{\"city\":\"Paris\"}"
        ));
        assert!(matches!(
            &events[3],
            StreamEvent::ContentBlockStop { index: 0 }
        ));
        assert!(matches!(
            &events[4],
            StreamEvent::MessageDelta {
                stop_reason: Some(reason),
                usage: Some(Usage {
                    input_tokens: 12,
                    output_tokens: 34,
                }),
            } if reason == "end_turn"
        ));
        assert!(matches!(&events[5], StreamEvent::MessageStop));
    }

    #[test]
    fn parse_sse_events_uses_message_text_when_no_delta_arrives() {
        let body = concat!(
            "event: response.output_item.done\n",
            "data: {\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello \"},{\"type\":\"output_text\",\"text\":\"world\"}]}}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n"
        );

        let events = parse_sse_events(body.as_bytes()).expect("parse SSE events");

        assert!(matches!(
            &events[0],
            StreamEvent::TextDelta(text) if text == "Hello world"
        ));
        assert!(matches!(events.last(), Some(StreamEvent::MessageStop)));
    }

    #[test]
    fn parse_sse_events_returns_error_for_failed_response() {
        let body = concat!(
            "event: response.failed\n",
            "data: {\"response\":{\"error\":{\"message\":\"model exploded\"}}}\n\n"
        );

        let err = parse_sse_events(body.as_bytes()).expect_err("failed response should error");

        assert!(err.to_string().contains("model exploded"));
    }

    #[test]
    fn parse_sse_events_adds_message_stop_when_completed_missing() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"delta\":\"partial\"}\n\n"
        );

        let events = parse_sse_events(body.as_bytes()).expect("parse SSE events");

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::TextDelta(text) if text == "partial"));
        assert!(matches!(&events[1], StreamEvent::MessageStop));
    }

    #[tokio::test]
    async fn refresh_access_token_updates_tokens_and_account_id() {
        let next_id_token = id_token_with_account("acct-456");
        let app = Router::new().route(
            "/oauth/token",
            post({
                let next_id_token = next_id_token.clone();
                move || async move {
                    Json(serde_json::json!({
                        "access_token": "next-access",
                        "refresh_token": "next-refresh",
                        "id_token": next_id_token,
                    }))
                }
            }),
        );
        let addr = spawn_test_server(app).await;
        let provider = CodexProvider {
            oauth_issuer: format!("http://{addr}"),
            ..test_provider(CodexAuthConfig {
                access_token: Some("old-access".to_string()),
                refresh_token: Some("old-refresh".to_string()),
                account_id: None,
                id_token: None,
            })
        };

        let auth = provider
            .refresh_access_token()
            .await
            .expect("refresh should succeed");

        assert_eq!(auth.access_token.as_deref(), Some("next-access"));
        assert_eq!(auth.refresh_token.as_deref(), Some("next-refresh"));
        assert_eq!(auth.account_id.as_deref(), Some("acct-456"));
        assert_eq!(auth.id_token.as_deref(), Some(next_id_token.as_str()));

        let stored = provider.read_auth();
        assert_eq!(stored.access_token, auth.access_token);
        assert_eq!(stored.refresh_token, auth.refresh_token);
        assert_eq!(stored.account_id, auth.account_id);
        assert_eq!(stored.id_token, auth.id_token);
    }

    #[tokio::test]
    async fn refresh_access_token_surfaces_http_error_body() {
        let app = Router::new().route(
            "/oauth/token",
            post(|| async move { (StatusCode::UNAUTHORIZED, "invalid_grant") }),
        );
        let addr = spawn_test_server(app).await;
        let provider = CodexProvider {
            oauth_issuer: format!("http://{addr}"),
            ..test_provider(CodexAuthConfig {
                access_token: None,
                refresh_token: Some("stale-refresh".to_string()),
                account_id: None,
                id_token: None,
            })
        };

        let err = provider
            .refresh_access_token()
            .await
            .expect_err("refresh should fail");

        assert!(err.to_string().contains("401 Unauthorized"));
        assert!(err.to_string().contains("invalid_grant"));
    }

    #[tokio::test]
    async fn stream_complete_yields_events_before_body_finishes() {
        let app = Router::new().route(
            "/responses",
            post(|| async move {
                let (tx, rx) = mpsc::channel::<std::result::Result<Bytes, Infallible>>(4);
                tokio::spawn(async move {
                    tx.send(Ok(Bytes::from(concat!(
                        "event: response.output_text.delta\n",
                        "data: {\"delta\":\"Hello\"}\n\n"
                    ))))
                    .await
                    .expect("send first SSE chunk");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    tx.send(Ok(Bytes::from(concat!(
                        "event: response.completed\n",
                        "data: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n"
                    ))))
                    .await
                    .expect("send completed SSE chunk");
                });

                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(ReceiverStream::new(rx)))
                    .expect("build SSE response")
            }),
        );
        let addr = spawn_test_server(app).await;
        let provider = CodexProvider {
            base_url: format!("http://{addr}"),
            ..test_provider(CodexAuthConfig {
                access_token: Some("access-token".to_string()),
                refresh_token: None,
                account_id: None,
                id_token: None,
            })
        };
        let request = LlmRequest {
            model: String::new(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Text("ping".to_string()),
            }],
            system: None,
            max_tokens: Some(4),
            temperature: None,
            tools: Vec::new(),
        };

        let mut stream = provider
            .stream_complete(&request)
            .await
            .expect("stream should start");

        let first = tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .expect("first event should arrive before completion chunk")
            .expect("stream should yield first event")
            .expect("first event should be ok");
        assert!(matches!(first, StreamEvent::TextDelta(text) if text == "Hello"));

        let second = tokio::time::timeout(Duration::from_millis(250), stream.next())
            .await
            .expect("completion metadata should arrive")
            .expect("stream should yield second event")
            .expect("second event should be ok");
        assert!(matches!(
            second,
            StreamEvent::MessageDelta {
                stop_reason: Some(reason),
                usage: Some(Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                }),
            } if reason == "end_turn"
        ));

        let third = tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .expect("message stop should follow immediately")
            .expect("stream should yield message stop")
            .expect("message stop should be ok");
        assert!(matches!(third, StreamEvent::MessageStop));
    }
}
