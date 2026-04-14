#[cfg(feature = "mcp")]
pub mod mcp;

pub mod a2a;
pub mod anthropic;
pub mod embeddings;
pub mod ollama;
pub mod openai;
pub mod providers;
pub mod runtime;
pub mod tools;

pub use anthropic::AnthropicProvider;
pub use embeddings::{CohereEmbeddingProvider, EmbeddingProvider, OllamaEmbeddingProvider};
pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;
pub use providers::{
    ChatMessage, ChatRole, ContentBlock, LlmProvider, LlmRequest, LlmResponse, MessagePart,
    StreamEvent, ToolDefinition,
};
pub use runtime::AgentRuntime;
pub use tools::{
    BashTool, CancelHeartbeat, CreateSkillTool, DocSearchTool, FileReadTool, FileWriteTool,
    GoogleSearchTool, HandoffHandle, HandoffTool, ListHeartbeats, ScheduleHeartbeat, Tool,
    ToolContext, ToolOutput, WebFetchTool, WebSearchTool,
};

#[cfg(feature = "mcp")]
pub use mcp::{McpManager, McpPromptInfo, McpResourceInfo, McpResourceTool, McpToolInfo};
