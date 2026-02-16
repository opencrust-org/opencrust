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
    pub data_dir: Option<PathBuf>,

    #[serde(default)]
    pub log_level: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            gateway: GatewayConfig::default(),
            channels: HashMap::new(),
            llm: HashMap::new(),
            embeddings: HashMap::new(),
            memory: MemoryConfig::default(),
            data_dir: None,
            log_level: Some("info".to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3000
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
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_memory_enabled(),
            embedding_provider: None,
            shared_continuity: false,
        }
    }
}

fn default_memory_enabled() -> bool {
    true
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
  port: 3000
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
