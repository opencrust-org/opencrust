use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use opencrust_common::Result;
use serde::{Deserialize, Serialize};

/// Trait for LLM provider integrations (Anthropic, OpenAI, Ollama, etc.).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider identifier (e.g. "anthropic", "openai", "ollama").
    fn provider_id(&self) -> &str;

    /// Send a completion request and return the response.
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse>;

    /// Stream a completion request, returning events as they arrive.
    /// Default implementation returns an error indicating streaming is not supported.
    async fn stream_complete(
        &self,
        _request: &LlmRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        Err(opencrust_common::Error::Agent(format!(
            "{} provider does not support streaming",
            self.provider_id()
        )))
    }

    /// Return the provider's configured default model, if known.
    fn configured_model(&self) -> Option<&str> {
        None
    }

    /// Return a list of models that can be selected for this provider.
    async fn available_models(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Check if the provider is available and configured.
    async fn health_check(&self) -> Result<bool>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub system: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: MessagePart,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessagePart {
    Text(String),
    Parts(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { url: String },
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub usage: Option<Usage>,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Events emitted during a streaming LLM completion.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of text output.
    TextDelta(String),
    /// A tool use block started.
    ToolUseStart {
        index: usize,
        id: String,
        name: String,
    },
    /// Partial JSON input for a tool use block.
    InputJsonDelta(String),
    /// A content block finished.
    ContentBlockStop { index: usize },
    /// The message is finishing with metadata.
    MessageDelta {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
    /// Stream complete.
    MessageStop,
}
