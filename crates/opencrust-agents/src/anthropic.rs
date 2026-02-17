use async_trait::async_trait;
use opencrust_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};

use crate::providers::{
    ChatMessage, ChatRole, ContentBlock, LlmProvider, LlmRequest, LlmResponse, MessagePart,
    Usage,
};

const DEFAULT_MODEL: &str = "claude-sonnet-4-5-20250929";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Anthropic Claude LLM provider.
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl AnthropicProvider {
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
        format!("{}/v1/messages", self.base_url.trim_end_matches('/'))
    }

    fn build_request(&self, request: &LlmRequest) -> AnthropicRequest {
        let model = if request.model.is_empty() {
            self.model.clone()
        } else {
            request.model.clone()
        };

        let messages: Vec<AnthropicMessage> = request
            .messages
            .iter()
            .filter(|m| !matches!(m.role, ChatRole::System))
            .map(to_anthropic_message)
            .collect();

        let tools: Vec<AnthropicTool> = request
            .tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect();

        AnthropicRequest {
            model,
            max_tokens: request.max_tokens.unwrap_or(4096),
            system: request.system.clone(),
            messages,
            temperature: request.temperature,
            tools: if tools.is_empty() { None } else { Some(tools) },
        }
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn provider_id(&self) -> &str {
        "anthropic"
    }

    #[instrument(skip(self, request), fields(model))]
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let body = self.build_request(request);

        tracing::Span::current().record("model", body.model.as_str());
        debug!("anthropic request: model={}", body.model);

        let response = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("anthropic request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "anthropic API error: status={status}, body={body}"
            )));
        }

        let api_response: AnthropicResponse = response
            .json()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse anthropic response: {e}")))?;

        Ok(from_anthropic_response(api_response))
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
                info!("anthropic health check failed: {e}");
                Ok(false)
            }
        }
    }
}

// --- Anthropic Wire Types (private) ---

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicBlock>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AnthropicBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicBlock>,
    model: String,
    usage: Option<AnthropicUsage>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

// --- Conversion Functions ---

fn to_anthropic_message(msg: &ChatMessage) -> AnthropicMessage {
    let role = match msg.role {
        ChatRole::User | ChatRole::Tool => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::System => "user",
    };

    let content = match &msg.content {
        MessagePart::Text(text) => AnthropicContent::Text(text.clone()),
        MessagePart::Parts(blocks) => {
            let anthropic_blocks: Vec<AnthropicBlock> = blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => AnthropicBlock::Text { text: text.clone() },
                    ContentBlock::ToolUse { id, name, input } => AnthropicBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => AnthropicBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: content.clone(),
                    },
                    ContentBlock::Image { url } => AnthropicBlock::Text {
                        text: format!("[image: {url}]"),
                    },
                })
                .collect();
            AnthropicContent::Blocks(anthropic_blocks)
        }
    };

    AnthropicMessage {
        role: role.to_string(),
        content,
    }
}

fn from_anthropic_response(response: AnthropicResponse) -> LlmResponse {
    let content: Vec<ContentBlock> = response
        .content
        .into_iter()
        .map(|block| match block {
            AnthropicBlock::Text { text } => ContentBlock::Text { text },
            AnthropicBlock::ToolUse { id, name, input } => {
                ContentBlock::ToolUse { id, name, input }
            }
            AnthropicBlock::ToolResult {
                tool_use_id,
                content,
            } => ContentBlock::ToolResult {
                tool_use_id,
                content,
            },
        })
        .collect();

    LlmResponse {
        content,
        model: response.model,
        usage: response.usage.map(|u| Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        }),
        stop_reason: response.stop_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ToolDefinition;

    #[test]
    fn builds_request_with_default_model() {
        let provider = AnthropicProvider::new("test-key", None, None);
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

        let anthropic_req = provider.build_request(&request);
        assert_eq!(anthropic_req.model, DEFAULT_MODEL);
        assert_eq!(anthropic_req.max_tokens, 1024);
        assert_eq!(anthropic_req.system, Some("You are helpful".to_string()));
        assert!(anthropic_req.tools.is_none());
    }

    #[test]
    fn builds_request_with_explicit_model() {
        let provider = AnthropicProvider::new("test-key", None, None);
        let request = LlmRequest {
            model: "claude-haiku-4-5-20251001".to_string(),
            messages: vec![],
            system: None,
            max_tokens: None,
            temperature: Some(0.7),
            tools: vec![],
        };

        let anthropic_req = provider.build_request(&request);
        assert_eq!(anthropic_req.model, "claude-haiku-4-5-20251001");
        assert_eq!(anthropic_req.temperature, Some(0.7));
    }

    #[test]
    fn serializes_request_correctly() {
        let req = AnthropicRequest {
            model: "claude-sonnet-4-5-20250929".to_string(),
            max_tokens: 1024,
            system: Some("Be helpful".to_string()),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("Hello".to_string()),
            }],
            temperature: None,
            tools: None,
        };

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "claude-sonnet-4-5-20250929");
        assert_eq!(json["max_tokens"], 1024);
        assert_eq!(json["system"], "Be helpful");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "Hello");
        assert!(json.get("temperature").is_none());
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn deserializes_text_response() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Hello! How can I help?"}
            ],
            "model": "claude-sonnet-4-5-20250929",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20
            },
            "stop_reason": "end_turn"
        }"#;

        let response: AnthropicResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.model, "claude-sonnet-4-5-20250929");
        assert_eq!(response.stop_reason, Some("end_turn".to_string()));

        let llm_response = from_anthropic_response(response);
        assert_eq!(llm_response.content.len(), 1);
        match &llm_response.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello! How can I help?"),
            _ => panic!("expected text block"),
        }
        assert_eq!(llm_response.usage.as_ref().unwrap().input_tokens, 10);
        assert_eq!(llm_response.usage.as_ref().unwrap().output_tokens, 20);
    }

    #[test]
    fn deserializes_tool_use_response() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Let me run that command."},
                {"type": "tool_use", "id": "toolu_123", "name": "bash", "input": {"command": "echo hello"}}
            ],
            "model": "claude-sonnet-4-5-20250929",
            "usage": {"input_tokens": 50, "output_tokens": 30},
            "stop_reason": "tool_use"
        }"#;

        let response: AnthropicResponse = serde_json::from_str(json).unwrap();
        let llm_response = from_anthropic_response(response);

        assert_eq!(llm_response.content.len(), 2);
        assert_eq!(llm_response.stop_reason, Some("tool_use".to_string()));

        match &llm_response.content[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_123");
                assert_eq!(name, "bash");
                assert_eq!(input["command"], "echo hello");
            }
            _ => panic!("expected tool_use block"),
        }
    }

    #[test]
    fn converts_tool_result_message() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Parts(vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_123".to_string(),
                content: "hello\n".to_string(),
            }]),
        };

        let anthropic_msg = to_anthropic_message(&msg);
        assert_eq!(anthropic_msg.role, "user");
        match &anthropic_msg.content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    AnthropicBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => {
                        assert_eq!(tool_use_id, "toolu_123");
                        assert_eq!(content, "hello\n");
                    }
                    _ => panic!("expected tool_result block"),
                }
            }
            _ => panic!("expected blocks content"),
        }
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let provider = AnthropicProvider::new(
            "key",
            None,
            Some("https://api.example.com/".to_string()),
        );
        assert_eq!(provider.endpoint(), "https://api.example.com/v1/messages");
    }

    #[test]
    fn request_includes_tools_when_provided() {
        let provider = AnthropicProvider::new("test-key", None, None);
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

        let anthropic_req = provider.build_request(&request);
        let tools = anthropic_req.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "bash");
    }
}
