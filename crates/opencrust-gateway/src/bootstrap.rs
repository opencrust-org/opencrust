use std::sync::Arc;

use opencrust_agents::{
    AgentRuntime, AnthropicProvider, BashTool, CohereEmbeddingProvider, FileReadTool,
    FileWriteTool, OpenAiProvider, WebFetchTool,
};
use opencrust_config::AppConfig;
use opencrust_db::MemoryStore;
use tracing::{info, warn};

/// Build a fully-configured `AgentRuntime` from the application config.
pub fn build_agent_runtime(config: &AppConfig) -> AgentRuntime {
    let mut runtime = AgentRuntime::new();

    // --- LLM Providers ---
    for (name, llm_config) in &config.llm {
        match llm_config.provider.as_str() {
            "anthropic" => {
                let api_key = llm_config
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok());

                if let Some(key) = api_key {
                    let provider = AnthropicProvider::new(
                        key,
                        llm_config.model.clone(),
                        llm_config.base_url.clone(),
                    );
                    runtime.register_provider(Box::new(provider));
                    info!("configured anthropic provider: {name}");
                } else {
                    warn!("skipping anthropic provider {name}: no API key (set api_key in config or ANTHROPIC_API_KEY env var)");
                }
            }
            "openai" => {
                let api_key = llm_config
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("OPENAI_API_KEY").ok());

                if let Some(key) = api_key {
                    let provider = OpenAiProvider::new(
                        key,
                        llm_config.model.clone(),
                        llm_config.base_url.clone(),
                    );
                    runtime.register_provider(Box::new(provider));
                    info!("configured openai provider: {name}");
                } else {
                    warn!("skipping openai provider {name}: no API key (set api_key in config or OPENAI_API_KEY env var)");
                }
            }
            other => {
                warn!("unknown LLM provider type: {other}, skipping {name}");
            }
        }
    }

    // --- Tools ---
    runtime.register_tool(Box::new(BashTool::new(None)));
    runtime.register_tool(Box::new(FileReadTool::new(None)));
    runtime.register_tool(Box::new(FileWriteTool::new(None)));
    runtime.register_tool(Box::new(WebFetchTool::new(None)));

    // --- Memory ---
    if config.memory.enabled {
        let data_dir = config
            .data_dir
            .clone()
            .or_else(|| {
                dirs::home_dir().map(|h| h.join(".opencrust").join("data"))
            })
            .unwrap_or_else(|| std::path::PathBuf::from(".opencrust/data"));

        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            warn!("failed to create data directory: {e}");
        }

        let memory_db_path = data_dir.join("memory.db");
        match MemoryStore::open(&memory_db_path) {
            Ok(store) => {
                let store = Arc::new(store);
                runtime.set_memory_provider(store);
                info!("memory store opened at {}", memory_db_path.display());

                // Attach embedding provider if configured
                if let Some(embed_name) = &config.memory.embedding_provider
                    && let Some(embed_config) = config.embeddings.get(embed_name)
                {
                    match embed_config.provider.as_str() {
                        "cohere" => {
                            let api_key = embed_config
                                .api_key
                                .clone()
                                .or_else(|| std::env::var("COHERE_API_KEY").ok());

                            if let Some(key) = api_key {
                                let provider = CohereEmbeddingProvider::new(
                                    key,
                                    embed_config.model.clone(),
                                    embed_config.base_url.clone(),
                                );
                                runtime.set_embedding_provider(Arc::new(provider));
                                info!("configured cohere embedding provider: {embed_name}");
                            } else {
                                warn!("skipping cohere embedding provider: no API key");
                            }
                        }
                        other => {
                            warn!("unknown embedding provider type: {other}");
                        }
                    }
                }
            }
            Err(e) => {
                warn!("failed to open memory store: {e}");
            }
        }
    }

    // --- Agent Config ---
    if let Some(prompt) = &config.agent.system_prompt {
        runtime.set_system_prompt(prompt.clone());
    }
    if let Some(max_tokens) = config.agent.max_tokens {
        runtime.set_max_tokens(max_tokens);
    }

    runtime
}
