use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub gateway: GatewayConfig,

    #[serde(default)]
    pub channels: HashMap<String, ChannelConfig>,

    #[serde(default)]
    pub llm: HashMap<String, LlmProviderConfig>,

    #[serde(default)]
    pub embeddings: HashMap<String, EmbeddingProviderConfig>,

    #[serde(default)]
    pub memory: MemoryConfig,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub data_dir: Option<PathBuf>,

    #[serde(default)]
    pub log_level: Option<String>,

    /// Show debug info (tool calls, RAG scores) in responses. Set via --debug flag.
    #[serde(default)]
    pub debug: bool,

    #[serde(default)]
    pub mcp: HashMap<String, McpServerConfig>,

    /// Named agent configurations for multi-agent routing.
    /// If empty, the single `agent:` block is used as "default".
    #[serde(default)]
    pub agents: HashMap<String, NamedAgentConfig>,

    #[serde(default)]
    pub tools: ToolsConfig,

    #[serde(default)]
    pub guardrails: GuardrailsConfig,

    #[serde(default)]
    pub voice: VoiceConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            gateway: GatewayConfig::default(),
            channels: HashMap::new(),
            llm: HashMap::new(),
            embeddings: HashMap::new(),
            memory: MemoryConfig::default(),
            agent: AgentConfig::default(),
            data_dir: None,
            log_level: Some("info".to_string()),
            debug: false,
            mcp: HashMap::new(),
            agents: HashMap::new(),
            tools: ToolsConfig::default(),
            guardrails: GuardrailsConfig::default(),
            voice: VoiceConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            api_key: None,
            rate_limit: RateLimitConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    #[serde(default = "default_rate_per_second")]
    pub per_second: u64,

    #[serde(default = "default_rate_burst_size")]
    pub burst_size: u32,

    /// Max messages per user per minute across all channels (default: 20).
    #[serde(default = "default_per_user_per_minute")]
    pub per_user_per_minute: u32,

    /// Burst allowance before per-user throttling kicks in (default: 5).
    #[serde(default = "default_per_user_burst")]
    pub per_user_burst: u32,

    /// Seconds a user is silenced after exceeding burst (default: 0 = disabled).
    #[serde(default)]
    pub cooldown_secs: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            per_second: default_rate_per_second(),
            burst_size: default_rate_burst_size(),
            per_user_per_minute: default_per_user_per_minute(),
            per_user_burst: default_per_user_burst(),
            cooldown_secs: 0,
        }
    }
}

fn default_rate_per_second() -> u64 {
    1
}

fn default_rate_burst_size() -> u32 {
    60
}

fn default_per_user_per_minute() -> u32 {
    20
}

fn default_per_user_burst() -> u32 {
    5
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3888
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    #[serde(rename = "type")]
    pub channel_type: String,

    pub enabled: Option<bool>,

    #[serde(flatten)]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    pub provider: String,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,

    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingProviderConfig {
    pub provider: String,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub dimensions: Option<usize>,

    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_memory_enabled")]
    pub enabled: bool,

    pub embedding_provider: Option<String>,

    #[serde(default)]
    pub shared_continuity: bool,

    /// Max memory entries recalled per turn (default: 10).
    #[serde(default)]
    pub recall_limit: Option<usize>,

    /// Enable rolling conversation summaries when context window fills.
    /// Default: true when memory is enabled.
    #[serde(default)]
    pub summarization: Option<bool>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_memory_enabled(),
            embedding_provider: None,
            shared_continuity: false,
            recall_limit: None,
            summarization: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    pub system_prompt: Option<String>,
    pub default_provider: Option<String>,
    pub max_tokens: Option<u32>,
    pub max_context_tokens: Option<usize>,
    /// Allow the agent to persist skills it discovers during conversations.
    /// Registers the `create_skill` tool at startup. Default: true.
    pub self_learning: Option<bool>,
    /// Max skills injected per turn via semantic search (default: 5).
    /// When skill count ≤ this limit all skills are always injected (no embedding call needed).
    pub skill_recall_limit: Option<usize>,
}

/// A named agent configuration for multi-agent routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedAgentConfig {
    /// Which LLM provider key (from `llm:` section) to use.
    pub provider: Option<String>,
    /// Override model name (otherwise uses the provider's default).
    pub model: Option<String>,
    /// Custom system prompt for this agent.
    pub system_prompt: Option<String>,
    /// Max output tokens.
    pub max_tokens: Option<u32>,
    /// Max context window tokens.
    pub max_context_tokens: Option<usize>,
    /// Restrict which tools this agent can use (empty = all tools).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Path to an agent-specific DNA file (overrides global dna.md).
    pub dna_file: Option<String>,
    /// Path to an agent-specific skills directory (overrides global skills/).
    pub skills_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default)]
    pub web_search: Option<WebSearchConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchConfig {
    pub provider: String,
    pub api_key: Option<String>,
    pub search_engine_id: Option<String>,
}

/// Safety, rate limiting, and cost controls applied across all channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailsConfig {
    /// Max input message length in characters. Messages exceeding this are rejected.
    /// Default: 16000.
    #[serde(default = "default_max_input_chars")]
    pub max_input_chars: usize,

    /// Max response length in characters. Responses are truncated if exceeded.
    /// Default: 32000.
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: usize,

    /// Enable content filtering on bot responses (harmful content, PII patterns).
    /// Default: false.
    #[serde(default)]
    pub content_filter_enabled: bool,

    /// Max input + output tokens per session before the agent stops. None = unlimited.
    #[serde(default)]
    pub token_budget_session: Option<u32>,

    /// Max tokens per user per day across all sessions. None = unlimited.
    #[serde(default)]
    pub token_budget_user_daily: Option<u32>,

    /// Max tokens per user per month across all sessions. None = unlimited.
    #[serde(default)]
    pub token_budget_user_monthly: Option<u32>,

    /// Hard spending cap in USD. None = unlimited.
    #[serde(default)]
    pub spending_cap_usd: Option<f64>,

    /// Alert threshold as a percentage of spending_cap_usd (default: 80).
    #[serde(default = "default_spending_alert_pct")]
    pub spending_alert_pct: u8,

    /// Max tool calls per session (separate from the 10-iteration loop cap). None = loop-cap only.
    #[serde(default)]
    pub session_tool_call_budget: Option<u32>,

    /// Allowlist of tool names the agent may call. `None` means all registered tools are permitted.
    /// Empty list means no tools may be called.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
}

impl Default for GuardrailsConfig {
    fn default() -> Self {
        Self {
            max_input_chars: default_max_input_chars(),
            max_output_chars: default_max_output_chars(),
            content_filter_enabled: false,
            token_budget_session: None,
            token_budget_user_daily: None,
            token_budget_user_monthly: None,
            spending_cap_usd: None,
            spending_alert_pct: default_spending_alert_pct(),
            session_tool_call_budget: None,
            allowed_tools: None,
        }
    }
}

fn default_max_input_chars() -> usize {
    16_000
}

fn default_max_output_chars() -> usize {
    32_000
}

fn default_spending_alert_pct() -> u8 {
    80
}

fn default_memory_enabled() -> bool {
    true
}

fn default_transport() -> String {
    "stdio".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,

    #[serde(default)]
    pub args: Vec<String>,

    #[serde(default)]
    pub env: HashMap<String, String>,

    #[serde(default = "default_transport")]
    pub transport: String,

    /// Future: HTTP transport URL
    pub url: Option<String>,

    /// Whether this server is enabled (default: true)
    pub enabled: Option<bool>,

    /// Connection timeout in seconds (default: 30)
    pub timeout: Option<u64>,
}

/// Voice input/output configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// TTS provider: `"openai"` (cloud) or `"kokoro"` (self-hosted).
    #[serde(default)]
    pub tts_provider: Option<String>,

    /// Voice ID (provider-specific).
    /// OpenAI: `"alloy"` | `"echo"` | `"fable"` | `"onyx"` | `"nova"` | `"shimmer"`
    /// Kokoro: `"af_heart"` | `"af_bella"` | … (see Kokoro docs)
    #[serde(default)]
    pub voice: Option<String>,

    /// Model override (OpenAI: `"tts-1"` / `"tts-1-hd"`; ignored by Kokoro).
    #[serde(default)]
    pub model: Option<String>,

    /// Base URL for a self-hosted TTS server (e.g. `http://localhost:8881` for Kokoro FastAPI).
    /// Also works with any OpenAI-compatible TTS endpoint when `tts_provider = "openai"`.
    #[serde(default, alias = "base_url")]
    pub tts_base_url: Option<String>,

    /// API key for the TTS/STT provider. Checked after vault, before env var.
    /// Falls back to the OpenAI provider key when not set.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Base URL for a self-hosted Whisper STT server (e.g. `http://localhost:8000`).
    /// When set, transcription is routed here instead of OpenAI/Groq and no API key is required.
    #[serde(default)]
    pub stt_base_url: Option<String>,

    /// Whisper model ID for self-hosted STT (default: `Systran/faster-whisper-large-v3`).
    #[serde(default)]
    pub stt_model: Option<String>,

    /// Maximum characters sent to TTS synthesis (default: 4000).
    /// Protects against OpenAI's 4096-char limit and oversized Kokoro responses.
    #[serde(default)]
    pub tts_max_chars: Option<usize>,

    /// When `true`, voice-message inputs receive a voice response.
    #[serde(default)]
    pub auto_reply_voice: bool,
}

#[cfg(test)]
mod tests {
    use super::AppConfig;

    #[test]
    fn app_config_defaults_include_memory_block() {
        let config = AppConfig::default();
        assert!(config.memory.enabled);
        assert!(!config.memory.shared_continuity);
        assert!(config.embeddings.is_empty());
    }

    #[test]
    fn parses_memory_and_embedding_config() {
        let raw = r#"
gateway:
  host: "127.0.0.1"
  port: 3888
memory:
  enabled: true
  embedding_provider: "cohere-main"
  shared_continuity: true
embeddings:
  cohere-main:
    provider: cohere
    model: embed-english-v3.0
    api_key: test-key
    base_url: https://api.cohere.com
    dimensions: 1024
"#;

        let config: AppConfig = serde_yaml::from_str(raw).expect("yaml should parse");
        assert!(config.memory.enabled);
        assert_eq!(
            config.memory.embedding_provider.as_deref(),
            Some("cohere-main")
        );
        assert!(config.memory.shared_continuity);

        let cohere = config
            .embeddings
            .get("cohere-main")
            .expect("cohere embedding provider should exist");
        assert_eq!(cohere.provider, "cohere");
        assert_eq!(cohere.model.as_deref(), Some("embed-english-v3.0"));
        assert_eq!(cohere.dimensions, Some(1024));
    }
}
