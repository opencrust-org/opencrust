use async_trait::async_trait;
use opencrust_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};

use crate::providers::{
    ChatMessage, ChatRole, ContentBlock, LlmProvider, LlmRequest, LlmResponse, MessagePart,
    Usage,
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
                            ContentBlock::ToolUse { id, name, input } => {
                                Some(OpenAiToolCall {
                                    id: id.clone(),
                                    r#type: "function".to_string(),
                                    function: OpenAiFunctionCall {
                                        name: name.clone(),
                                        arguments: serde_json::to_string(input)
                                            .unwrap_or_default(),
                                    },
                                })
                            }
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
        assert_eq!(
            openai_req.messages[1].content,
            Some("hi\n".to_string())
        );
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
