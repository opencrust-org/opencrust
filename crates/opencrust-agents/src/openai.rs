use std::pin::Pin;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::Stream;
use opencrust_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};

use crate::providers::{
    ChatMessage, ChatRole, ContentBlock, LlmProvider, LlmRequest, LlmResponse, MessagePart,
    StreamEvent, Usage,
};

const DEFAULT_MODEL: &str = "gpt-4o";
const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// OpenAI Chat Completions provider.
/// Also works with OpenAI-compatible APIs (Azure, local models) via `base_url`.
pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(
        api_key: impl Into<String>,
        model: Option<String>,
        base_url: Option<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }

    fn build_request(&self, request: &LlmRequest) -> OpenAiRequest {
        let model = if request.model.is_empty() {
            self.model.clone()
        } else {
            request.model.clone()
        };

        let mut messages: Vec<OpenAiMessage> = Vec::new();

        // System message from the request
        if let Some(system) = &request.system {
            messages.push(OpenAiMessage {
                role: "system".to_string(),
                content: Some(system.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // Convert chat messages
        for msg in &request.messages {
            match (&msg.role, &msg.content) {
                // User messages with tool results expand to multiple "tool" messages
                (ChatRole::User, MessagePart::Parts(blocks))
                    if blocks
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolResult { .. })) =>
                {
                    for block in blocks {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                        } = block
                        {
                            messages.push(OpenAiMessage {
                                role: "tool".to_string(),
                                content: Some(content.clone()),
                                tool_calls: None,
                                tool_call_id: Some(tool_use_id.clone()),
                            });
                        }
                    }
                }
                // Assistant messages with tool_use blocks
                (ChatRole::Assistant, MessagePart::Parts(blocks)) => {
                    let text_content: String = blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let tool_calls: Vec<OpenAiToolCall> = blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse { id, name, input } => Some(OpenAiToolCall {
                                id: id.clone(),
                                r#type: "function".to_string(),
                                function: OpenAiFunctionCall {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            }),
                            _ => None,
                        })
                        .collect();

                    messages.push(OpenAiMessage {
                        role: "assistant".to_string(),
                        content: if text_content.is_empty() {
                            None
                        } else {
                            Some(text_content)
                        },
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                        tool_call_id: None,
                    });
                }
                // Simple text messages
                (role, MessagePart::Text(text)) => {
                    let role_str = match role {
                        ChatRole::User => "user",
                        ChatRole::Assistant => "assistant",
                        ChatRole::System => "system",
                        ChatRole::Tool => "tool",
                    };
                    messages.push(OpenAiMessage {
                        role: role_str.to_string(),
                        content: Some(text.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                // User messages with non-tool-result parts
                (role, MessagePart::Parts(blocks)) => {
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let role_str = match role {
                        ChatRole::User => "user",
                        ChatRole::Assistant => "assistant",
                        ChatRole::System => "system",
                        ChatRole::Tool => "tool",
                    };
                    messages.push(OpenAiMessage {
                        role: role_str.to_string(),
                        content: if text.is_empty() { None } else { Some(text) },
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
            }
        }

        let tools: Vec<OpenAiTool> = request
            .tools
            .iter()
            .map(|t| OpenAiTool {
                r#type: "function".to_string(),
                function: OpenAiFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect();

        OpenAiRequest {
            model,
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools: if tools.is_empty() { None } else { Some(tools) },
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn provider_id(&self) -> &str {
        "openai"
    }

    #[instrument(skip(self, request), fields(model))]
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let body = self.build_request(request);

        tracing::Span::current().record("model", body.model.as_str());
        debug!("openai request: model={}", body.model);

        let response = self
            .client
            .post(self.endpoint())
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("openai request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "openai API error: status={status}, body={body}"
            )));
        }

        let api_response: OpenAiResponse = response
            .json()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse openai response: {e}")))?;

        Ok(from_openai_response(api_response))
    }

    #[instrument(skip(self, request), fields(model))]
    async fn stream_complete(
        &self,
        request: &LlmRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let body = self.build_request(request);

        tracing::Span::current().record("model", body.model.as_str());
        debug!("openai stream request: model={}", body.model);

        // Inject stream=true into the serialized request
        let mut body_value = serde_json::to_value(&body)
            .map_err(|e| Error::Agent(format!("failed to serialize request: {e}")))?;
        body_value["stream"] = serde_json::Value::Bool(true);

        let response = self
            .client
            .post(self.endpoint())
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body_value)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("openai stream request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "openai API error: status={status}, body={body}"
            )));
        }

        let byte_stream: Pin<
            Box<dyn Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send>,
        > = Box::pin(response.bytes_stream());

        let event_stream = futures::stream::unfold(
            (byte_stream, String::new()),
            |(mut stream, mut buffer)| async move {
                loop {
                    // Try to consume a complete SSE event from the buffer
                    if let Some(pos) = buffer.find("\n\n") {
                        let event_str = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();

                        // Extract the data: line
                        let mut data_line = None;
                        for line in event_str.lines() {
                            if let Some(d) = line.strip_prefix("data: ") {
                                data_line = Some(d.to_string());
                            }
                        }

                        if let Some(data) = data_line {
                            // OpenAI sends "data: [DONE]" as the final event
                            if data == "[DONE]" {
                                return Some((Ok(StreamEvent::MessageStop), (stream, buffer)));
                            }

                            if let Some(events) = parse_stream_chunk(&data) {
                                // Yield the first event; remaining events get pushed back
                                // into the buffer as synthetic SSE blocks so the loop
                                // picks them up on the next iteration.
                                let mut iter = events.into_iter();
                                let first = iter.next().unwrap();
                                for extra in iter.rev() {
                                    // Push synthetic SSE event back into buffer
                                    let json =
                                        serde_json::to_string(&SyntheticEvent(extra)).unwrap();
                                    buffer = format!("data: {json}\n\n{buffer}");
                                }
                                return Some((Ok(first), (stream, buffer)));
                            }
                            continue;
                        }
                        continue;
                    }

                    // Need more data from the byte stream
                    match stream.next().await {
                        Some(Ok(bytes)) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(Error::Agent(format!("stream read error: {e}"))),
                                (stream, buffer),
                            ));
                        }
                        None => return None,
                    }
                }
            },
        );

        Ok(Box::pin(event_stream))
    }

    async fn health_check(&self) -> Result<bool> {
        let request = LlmRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Text("ping".to_string()),
            }],
            system: None,
            max_tokens: Some(1),
            temperature: None,
            tools: vec![],
        };

        match self.complete(&request).await {
            Ok(_) => Ok(true),
            Err(e) => {
                info!("openai health check failed: {e}");
                Ok(false)
            }
        }
    }
}

// --- OpenAI Wire Types (private) ---

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiTool>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAiToolCall {
    id: String,
    r#type: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OpenAiTool {
    r#type: String,
    function: OpenAiFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    model: String,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// --- OpenAI Streaming Wire Types (private) ---

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    choices: Vec<OpenAiStreamChoice>,
    #[allow(dead_code)]
    model: Option<String>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    #[allow(dead_code)]
    index: usize,
    delta: OpenAiStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamDelta {
    #[allow(dead_code)]
    role: Option<String>,
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiStreamToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamToolCallDelta {
    index: usize,
    id: Option<String>,
    #[allow(dead_code)]
    r#type: Option<String>,
    function: Option<OpenAiStreamFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

/// Wrapper to allow serializing a StreamEvent back into a synthetic SSE data line
/// when a single chunk produces multiple events.
struct SyntheticEvent(StreamEvent);

impl serde::Serialize for SyntheticEvent {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match &self.0 {
            StreamEvent::TextDelta(text) => {
                map.serialize_entry("_synthetic", "text_delta")?;
                map.serialize_entry("text", text)?;
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                map.serialize_entry("_synthetic", "tool_use_start")?;
                map.serialize_entry("index", index)?;
                map.serialize_entry("id", id)?;
                map.serialize_entry("name", name)?;
            }
            StreamEvent::InputJsonDelta(json) => {
                map.serialize_entry("_synthetic", "input_json_delta")?;
                map.serialize_entry("json", json)?;
            }
            StreamEvent::ContentBlockStop { index } => {
                map.serialize_entry("_synthetic", "content_block_stop")?;
                map.serialize_entry("index", index)?;
            }
            StreamEvent::MessageDelta { stop_reason, .. } => {
                map.serialize_entry("_synthetic", "message_delta")?;
                map.serialize_entry("stop_reason", stop_reason)?;
            }
            StreamEvent::MessageStop => {
                map.serialize_entry("_synthetic", "message_stop")?;
            }
        }
        map.end()
    }
}

fn parse_synthetic_event(value: &serde_json::Value) -> Option<StreamEvent> {
    let kind = value.get("_synthetic")?.as_str()?;
    match kind {
        "text_delta" => Some(StreamEvent::TextDelta(
            value.get("text")?.as_str()?.to_string(),
        )),
        "tool_use_start" => Some(StreamEvent::ToolUseStart {
            index: value.get("index")?.as_u64()? as usize,
            id: value.get("id")?.as_str()?.to_string(),
            name: value.get("name")?.as_str()?.to_string(),
        }),
        "input_json_delta" => Some(StreamEvent::InputJsonDelta(
            value.get("json")?.as_str()?.to_string(),
        )),
        "content_block_stop" => Some(StreamEvent::ContentBlockStop {
            index: value.get("index")?.as_u64()? as usize,
        }),
        "message_delta" => Some(StreamEvent::MessageDelta {
            stop_reason: value
                .get("stop_reason")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            usage: None,
        }),
        "message_stop" => Some(StreamEvent::MessageStop),
        _ => None,
    }
}

/// Parse an OpenAI streaming chunk into one or more StreamEvents.
fn parse_stream_chunk(data: &str) -> Option<Vec<StreamEvent>> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;

    // Handle synthetic events we pushed back into the buffer
    if value.get("_synthetic").is_some() {
        return parse_synthetic_event(&value).map(|e| vec![e]);
    }

    let chunk: OpenAiStreamChunk = serde_json::from_value(value).ok()?;

    let choice = chunk.choices.first()?;
    let mut events = Vec::new();

    // Text content delta
    if let Some(content) = &choice.delta.content
        && !content.is_empty()
    {
        events.push(StreamEvent::TextDelta(content.clone()));
    }

    // Tool call deltas
    if let Some(tool_calls) = &choice.delta.tool_calls {
        for tc in tool_calls {
            // First chunk for a tool call includes id and name
            if let Some(id) = &tc.id {
                let name = tc
                    .function
                    .as_ref()
                    .and_then(|f| f.name.as_ref())
                    .cloned()
                    .unwrap_or_default();
                events.push(StreamEvent::ToolUseStart {
                    index: tc.index,
                    id: id.clone(),
                    name,
                });
            }

            // Argument fragments
            if let Some(func) = &tc.function
                && let Some(args) = &func.arguments
                && !args.is_empty()
            {
                events.push(StreamEvent::InputJsonDelta(args.clone()));
            }
        }
    }

    // finish_reason signals end of generation
    if let Some(reason) = &choice.finish_reason {
        let stop_reason = match reason.as_str() {
            "stop" => "end_turn".to_string(),
            "tool_calls" => "tool_use".to_string(),
            other => other.to_string(),
        };

        let usage = chunk.usage.map(|u| Usage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        });

        events.push(StreamEvent::MessageDelta {
            stop_reason: Some(stop_reason),
            usage,
        });
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

// --- Conversion ---

fn from_openai_response(response: OpenAiResponse) -> LlmResponse {
    let choice = response.choices.into_iter().next();

    let (content, stop_reason) = match choice {
        Some(c) => {
            let mut blocks = Vec::new();

            if let Some(text) = &c.message.content
                && !text.is_empty()
            {
                blocks.push(ContentBlock::Text { text: text.clone() });
            }

            if let Some(tool_calls) = c.message.tool_calls {
                for tc in tool_calls {
                    let input: serde_json::Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                    blocks.push(ContentBlock::ToolUse {
                        id: tc.id,
                        name: tc.function.name,
                        input,
                    });
                }
            }

            // Map OpenAI finish_reason to Anthropic-style stop_reason
            let stop = c.finish_reason.map(|r| match r.as_str() {
                "stop" => "end_turn".to_string(),
                "tool_calls" => "tool_use".to_string(),
                other => other.to_string(),
            });

            (blocks, stop)
        }
        None => (vec![], None),
    };

    LlmResponse {
        content,
        model: response.model,
        usage: response.usage.map(|u| Usage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        }),
        stop_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ToolDefinition;

    #[test]
    fn builds_request_with_default_model() {
        let provider = OpenAiProvider::new("test-key", None, None);
        let request = LlmRequest {
            model: String::new(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Text("hello".to_string()),
            }],
            system: Some("You are helpful".to_string()),
            max_tokens: Some(1024),
            temperature: None,
            tools: vec![],
        };

        let openai_req = provider.build_request(&request);
        assert_eq!(openai_req.model, DEFAULT_MODEL);
        // System message should be first
        assert_eq!(openai_req.messages[0].role, "system");
        assert_eq!(
            openai_req.messages[0].content,
            Some("You are helpful".to_string())
        );
        assert_eq!(openai_req.messages[1].role, "user");
        assert!(openai_req.tools.is_none());
    }

    #[test]
    fn serializes_request_correctly() {
        let req = OpenAiRequest {
            model: "gpt-4o".to_string(),
            messages: vec![OpenAiMessage {
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            max_tokens: Some(1024),
            temperature: None,
            tools: None,
        };

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "gpt-4o");
        assert_eq!(json["max_tokens"], 1024);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "Hello");
        assert!(json.get("temperature").is_none());
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn deserializes_text_response() {
        let json = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello! How can I help?"
                },
                "finish_reason": "stop"
            }],
            "model": "gpt-4o",
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20
            }
        }"#;

        let response: OpenAiResponse = serde_json::from_str(json).unwrap();
        let llm_response = from_openai_response(response);

        assert_eq!(llm_response.content.len(), 1);
        match &llm_response.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello! How can I help?"),
            _ => panic!("expected text block"),
        }
        assert_eq!(llm_response.stop_reason, Some("end_turn".to_string()));
        assert_eq!(llm_response.usage.as_ref().unwrap().input_tokens, 10);
        assert_eq!(llm_response.usage.as_ref().unwrap().output_tokens, 20);
    }

    #[test]
    fn deserializes_tool_call_response() {
        let json = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Let me check that.",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"echo hello\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "gpt-4o",
            "usage": {"prompt_tokens": 50, "completion_tokens": 30}
        }"#;

        let response: OpenAiResponse = serde_json::from_str(json).unwrap();
        let llm_response = from_openai_response(response);

        assert_eq!(llm_response.content.len(), 2);
        assert_eq!(llm_response.stop_reason, Some("tool_use".to_string()));

        match &llm_response.content[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "bash");
                assert_eq!(input["command"], "echo hello");
            }
            _ => panic!("expected tool_use block"),
        }
    }

    #[test]
    fn converts_tool_result_to_tool_messages() {
        let provider = OpenAiProvider::new("test-key", None, None);
        let request = LlmRequest {
            model: String::new(),
            messages: vec![
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: MessagePart::Parts(vec![ContentBlock::ToolUse {
                        id: "call_123".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({"command": "echo hi"}),
                    }]),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: MessagePart::Parts(vec![ContentBlock::ToolResult {
                        tool_use_id: "call_123".to_string(),
                        content: "hi\n".to_string(),
                    }]),
                },
            ],
            system: None,
            max_tokens: None,
            temperature: None,
            tools: vec![],
        };

        let openai_req = provider.build_request(&request);
        // Assistant message with tool_calls
        assert_eq!(openai_req.messages[0].role, "assistant");
        assert!(openai_req.messages[0].tool_calls.is_some());
        // Tool result as role=tool message
        assert_eq!(openai_req.messages[1].role, "tool");
        assert_eq!(
            openai_req.messages[1].tool_call_id,
            Some("call_123".to_string())
        );
        assert_eq!(openai_req.messages[1].content, Some("hi\n".to_string()));
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let provider =
            OpenAiProvider::new("key", None, Some("https://api.example.com/".to_string()));
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn parses_text_stream_chunk() {
        let data = r#"{"id":"chatcmpl-abc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}"#;
        let events = parse_stream_chunk(data).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hello"));
    }

    #[test]
    fn parses_tool_call_stream_chunks() {
        // First chunk: tool_use start with id and name
        let data1 = r#"{"id":"chatcmpl-abc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_123","type":"function","function":{"name":"bash","arguments":""}}]},"finish_reason":null}]}"#;
        let events1 = parse_stream_chunk(data1).unwrap();
        assert!(
            matches!(&events1[0], StreamEvent::ToolUseStart { id, name, .. } if id == "call_123" && name == "bash")
        );

        // Subsequent chunk: argument fragment
        let data2 = r#"{"id":"chatcmpl-abc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"cmd\":"}}]},"finish_reason":null}]}"#;
        let events2 = parse_stream_chunk(data2).unwrap();
        assert!(matches!(&events2[0], StreamEvent::InputJsonDelta(s) if s == r#"{"cmd":"#));
    }

    #[test]
    fn parses_finish_reason_stop() {
        let data = r#"{"id":"chatcmpl-abc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let events = parse_stream_chunk(data).unwrap();
        assert!(
            matches!(&events[0], StreamEvent::MessageDelta { stop_reason: Some(r), .. } if r == "end_turn")
        );
    }

    #[test]
    fn parses_finish_reason_tool_calls() {
        let data = r#"{"id":"chatcmpl-abc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let events = parse_stream_chunk(data).unwrap();
        assert!(
            matches!(&events[0], StreamEvent::MessageDelta { stop_reason: Some(r), .. } if r == "tool_use")
        );
    }

    #[test]
    fn done_sentinel_returns_none() {
        // [DONE] is not valid JSON, so parse_stream_chunk returns None
        assert!(parse_stream_chunk("[DONE]").is_none());
    }

    #[test]
    fn request_includes_tools_when_provided() {
        let provider = OpenAiProvider::new("test-key", None, None);
        let request = LlmRequest {
            model: String::new(),
            messages: vec![],
            system: None,
            max_tokens: None,
            temperature: None,
            tools: vec![ToolDefinition {
                name: "bash".to_string(),
                description: "Run a command".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}}
                }),
            }],
        };

        let openai_req = provider.build_request(&request);
        let tools = openai_req.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "bash");
        assert_eq!(tools[0].r#type, "function");
    }
}
