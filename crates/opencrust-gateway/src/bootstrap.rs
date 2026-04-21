use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use opencrust_agents::tools::Tool;
use opencrust_agents::{
    AgentRuntime, AnthropicProvider, BashTool, ChatMessage, CohereEmbeddingProvider,
    CreateSkillTool, DocSearchTool, FilePatchTool, FileReadTool, FileWriteTool, GoogleSearchTool,
    ListDocumentsTool, McpManager, MemoryTool, OllamaEmbeddingProvider, OllamaProvider,
    OpenAiProvider, SearchFilesTool, SendMessageHandle, SendMessageTool, WebFetchTool,
    WebSearchTool,
};
use opencrust_channels::{
    ChannelResponse, MediaAttachment, MqttChannel, MqttOnMessageFn, SlackChannel, SlackGroupFilter,
    SlackOnMessageFn, TelegramChannel, WhatsAppChannel, WhatsAppOnMessageFn, WhatsAppWebChannel,
    WhatsAppWebGroupFilter,
};
#[cfg(target_os = "macos")]
use opencrust_channels::{IMessageChannel, IMessageGroupFilter, IMessageOnMessageFn};
use opencrust_config::AppConfig;
use opencrust_db::{MemoryStore, VectorStore};
use opencrust_security::{Allowlist, ChannelPolicy, DmAuthResult, PairingManager, check_dm_auth};
use tracing::{info, warn};

use crate::state::SharedState;

/// Default vault path under the user's home directory.
pub(crate) fn default_vault_path() -> Option<PathBuf> {
    Some(
        opencrust_config::ConfigLoader::default_config_dir()
            .join("credentials")
            .join("vault.json"),
    )
}

/// Resolve an API key using the priority chain: vault -> config -> env var.
pub(crate) fn resolve_api_key(
    config_key: Option<&str>,
    vault_credential_key: &str,
    env_var: &str,
) -> Option<String> {
    // 1. Try credential vault (only works when OPENCRUST_VAULT_PASSPHRASE is set)
    if let Some(vault_path) = default_vault_path()
        && let Some(val) = opencrust_security::try_vault_get(&vault_path, vault_credential_key)
    {
        return Some(val);
    }

    // 2. Config file value
    if let Some(key) = config_key
        && !key.is_empty()
    {
        return Some(key.to_string());
    }

    // 3. Environment variable
    std::env::var(env_var).ok()
}

/// Build a fully-configured `AgentRuntime` from the application config.
pub async fn build_agent_runtime(config: &AppConfig) -> (AgentRuntime, SendMessageHandle) {
    let mut runtime = AgentRuntime::new();

    // --- LLM Providers ---
    for (name, llm_config) in &config.llm {
        match llm_config.provider.as_str() {
            "anthropic" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "ANTHROPIC_API_KEY",
                    "ANTHROPIC_API_KEY",
                );

                if let Some(key) = api_key {
                    let provider = AnthropicProvider::new(
                        key,
                        llm_config.model.clone(),
                        llm_config.base_url.clone(),
                    )
                    .with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured anthropic provider: {name}");
                } else {
                    warn!(
                        "skipping anthropic provider {name}: no API key (set api_key in config or ANTHROPIC_API_KEY env var)"
                    );
                }
            }
            "openai" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "OPENAI_API_KEY",
                    "OPENAI_API_KEY",
                );

                if let Some(key) = api_key {
                    let provider = OpenAiProvider::new(
                        key,
                        llm_config.model.clone(),
                        llm_config.base_url.clone(),
                    )
                    .with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured openai provider: {name}");
                } else {
                    warn!(
                        "skipping openai provider {name}: no API key (set api_key in config or OPENAI_API_KEY env var)"
                    );
                }
            }
            "ollama" => {
                let provider =
                    OllamaProvider::new(llm_config.model.clone(), llm_config.base_url.clone())
                        .with_name(name);
                runtime.register_provider(Arc::new(provider));
                info!("configured ollama provider: {name}");
            }
            "sansa" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "SANSA_API_KEY",
                    "SANSA_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.sansaml.com".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("sansa-auto".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured sansa provider: {name}");
                } else {
                    warn!(
                        "skipping sansa provider {name}: no API key (set api_key in config or SANSA_API_KEY env var)"
                    );
                }
            }
            "deepseek" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "DEEPSEEK_API_KEY",
                    "DEEPSEEK_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.deepseek.com".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("deepseek-chat".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured deepseek provider: {name}");
                } else {
                    warn!(
                        "skipping deepseek provider {name}: no API key (set api_key in config or DEEPSEEK_API_KEY env var)"
                    );
                }
            }
            "mistral" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "MISTRAL_API_KEY",
                    "MISTRAL_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.mistral.ai".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("mistral-large-latest".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured mistral provider: {name}");
                } else {
                    warn!(
                        "skipping mistral provider {name}: no API key (set api_key in config or MISTRAL_API_KEY env var)"
                    );
                }
            }
            "gemini" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "GEMINI_API_KEY",
                    "GEMINI_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config.base_url.clone().or_else(|| {
                        Some("https://generativelanguage.googleapis.com/v1beta/openai/".to_string())
                    });
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("gemini-2.5-flash".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured gemini provider: {name}");
                } else {
                    warn!(
                        "skipping gemini provider {name}: no API key (set api_key in config or GEMINI_API_KEY env var)"
                    );
                }
            }
            "falcon" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "FALCON_API_KEY",
                    "FALCON_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.ai71.ai/v1".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("tiiuae/falcon-180b-chat".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured falcon provider: {name}");
                } else {
                    warn!(
                        "skipping falcon provider {name}: no API key (set api_key in config or FALCON_API_KEY env var)"
                    );
                }
            }
            "jais" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "JAIS_API_KEY",
                    "JAIS_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.core42.ai/v1".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("jais-adapted-70b-chat".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured jais provider: {name}");
                } else {
                    warn!(
                        "skipping jais provider {name}: no API key (set api_key in config or JAIS_API_KEY env var)"
                    );
                }
            }
            "qwen" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "QWEN_API_KEY",
                    "QWEN_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config.base_url.clone().or_else(|| {
                        Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1".to_string())
                    });
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("qwen-plus".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured qwen provider: {name}");
                } else {
                    warn!(
                        "skipping qwen provider {name}: no API key (set api_key in config or QWEN_API_KEY env var)"
                    );
                }
            }
            "yi" => {
                let api_key =
                    resolve_api_key(llm_config.api_key.as_deref(), "YI_API_KEY", "YI_API_KEY");

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.lingyiwanwu.com/v1".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("yi-large".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured yi provider: {name}");
                } else {
                    warn!(
                        "skipping yi provider {name}: no API key (set api_key in config or YI_API_KEY env var)"
                    );
                }
            }
            "cohere" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "COHERE_API_KEY",
                    "COHERE_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.cohere.com/compatibility/v1".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("command-r-plus".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured cohere provider: {name}");
                } else {
                    warn!(
                        "skipping cohere provider {name}: no API key (set api_key in config or COHERE_API_KEY env var)"
                    );
                }
            }
            "minimax" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "MINIMAX_API_KEY",
                    "MINIMAX_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.minimaxi.chat/v1".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("MiniMax-Text-01".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured minimax provider: {name}");
                } else {
                    warn!(
                        "skipping minimax provider {name}: no API key (set api_key in config or MINIMAX_API_KEY env var)"
                    );
                }
            }
            "moonshot" => {
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "MOONSHOT_API_KEY",
                    "MOONSHOT_API_KEY",
                );

                if let Some(key) = api_key {
                    let base_url = llm_config
                        .base_url
                        .clone()
                        .or_else(|| Some("https://api.moonshot.cn/v1".to_string()));
                    let model = llm_config
                        .model
                        .clone()
                        .or_else(|| Some("kimi-k2-0711-preview".to_string()));
                    let provider = OpenAiProvider::new(key, model, base_url).with_name(name);
                    runtime.register_provider(Arc::new(provider));
                    info!("configured moonshot provider: {name}");
                } else {
                    warn!(
                        "skipping moonshot provider {name}: no API key (set api_key in config or MOONSHOT_API_KEY env var)"
                    );
                }
            }
            "vllm" => {
                // vLLM is self-hosted and does not require an API key by default.
                // If the server is started with --api-key, set api_key in config or VLLM_API_KEY.
                let api_key = resolve_api_key(
                    llm_config.api_key.as_deref(),
                    "VLLM_API_KEY",
                    "VLLM_API_KEY",
                )
                .unwrap_or_else(|| "EMPTY".to_string());
                let base_url = llm_config
                    .base_url
                    .clone()
                    .or_else(|| Some("http://localhost:8000".to_string()));
                let model = llm_config.model.clone();
                let provider = OpenAiProvider::new(api_key, model, base_url).with_name(name);
                runtime.register_provider(Arc::new(provider));
                info!("configured vllm provider: {name}");
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
    runtime.register_tool(Box::new(FilePatchTool::new(None)));
    runtime.register_tool(Box::new(SearchFilesTool::new()));
    runtime.register_tool(Box::new(WebFetchTool::new(None)));

    // Self-learning: agent can save reusable skills discovered during conversations.
    // Enabled by default; set `agent.self_learning: false` in config.yml to disable.
    if config.agent.self_learning.unwrap_or(true) {
        let skills_dir = opencrust_config::ConfigLoader::default_config_dir().join("skills");
        runtime.register_tool(Box::new(CreateSkillTool::new(skills_dir)));
        info!("create_skill tool registered (self-learning enabled)");
    } else {
        info!("self-learning disabled via config (agent.self_learning: false)");
    }

    // Web search (Brave or Google)
    let search_config = config.tools.web_search.as_ref();
    let provider = search_config.map(|c| c.provider.as_str());

    match provider {
        Some("google") => {
            let api_key = resolve_api_key(
                search_config.and_then(|c| c.api_key.as_deref()),
                "GOOGLE_SEARCH_KEY",
                "GOOGLE_SEARCH_KEY",
            );
            let cx = resolve_api_key(
                search_config.and_then(|c| c.search_engine_id.as_deref()),
                "GOOGLE_SEARCH_ENGINE_ID",
                "GOOGLE_SEARCH_ENGINE_ID",
            );

            if let (Some(key), Some(cx)) = (api_key, cx) {
                runtime.register_tool(Box::new(GoogleSearchTool::new(key, cx)));
                info!("web search tool registered: google");
            } else {
                warn!("skipping google search: missing api_key or search_engine_id");
            }
        }
        Some("brave") | None => {
            // Legacy brave config check if no tools block exists
            let brave_config_key = if let Some(cfg) = search_config {
                cfg.api_key.clone()
            } else {
                config.llm.get("brave").and_then(|c| c.api_key.clone())
            };

            if let Some(key) = resolve_api_key(
                brave_config_key.as_deref(),
                "BRAVE_API_KEY",
                "BRAVE_API_KEY",
            ) {
                runtime.register_tool(Box::new(WebSearchTool::new(key)));
                info!("web search tool registered: brave");
            } else if provider.is_some() {
                warn!("skipping brave search: no API key found");
            }
        }
        Some(other) => {
            warn!("unknown web search provider: {other}, skipping");
        }
    }

    // --- Memory ---
    if config.memory.enabled {
        let data_dir = config
            .data_dir
            .clone()
            .unwrap_or_else(|| opencrust_config::ConfigLoader::default_config_dir().join("data"));

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
                            let api_key = resolve_api_key(
                                embed_config.api_key.as_deref(),
                                "COHERE_API_KEY",
                                "COHERE_API_KEY",
                            );

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
                        "ollama" => {
                            let provider = OllamaEmbeddingProvider::new(
                                embed_config.model.clone(),
                                embed_config.base_url.clone(),
                            );
                            runtime.set_embedding_provider(Arc::new(provider));
                            info!("configured ollama embedding provider: {embed_name}");
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

    // --- Document Search (RAG) ---
    {
        let data_dir = config
            .data_dir
            .clone()
            .unwrap_or_else(|| opencrust_config::ConfigLoader::default_config_dir().join("data"));
        let memory_db_path = data_dir.join("memory.db");

        // Always register doc_search — the tool opens the DB fresh on every call,
        // so documents ingested after startup (including the very first ingest) are
        // immediately visible without a server restart.
        // Embedding is optional: falls back to keyword search when unavailable.
        {
            let embed_fn: Option<opencrust_agents::tools::doc_search_tool::EmbedFn> =
                runtime.embedding_provider().map(|embed| {
                    let embed_clone = embed.clone();
                    let f: opencrust_agents::tools::doc_search_tool::EmbedFn =
                        Arc::new(move |text: &str| {
                            let e = embed_clone.clone();
                            let t = text.to_string();
                            Box::pin(async move { e.embed_query(&t).await })
                        });
                    f
                });
            let mode = if embed_fn.is_some() {
                "vector"
            } else {
                "keyword"
            };
            runtime.register_tool(Box::new(DocSearchTool::new(
                memory_db_path.clone(),
                embed_fn,
            )));
            runtime.set_doc_db_path(memory_db_path.clone());
            info!("doc_search tool registered ({mode} search)");
            runtime.register_tool(Box::new(ListDocumentsTool::new(memory_db_path.clone())));
            info!("list_documents tool registered");

            // Explicit persistent memory tool — agent-initiated save/recall across sessions.
            runtime.register_tool(Box::new(MemoryTool::new(memory_db_path.clone())));
            info!("memory tool registered");
        }
    }

    // --- send_message tool (wired via handle returned to caller) ---
    let (send_msg_tool, send_msg_handle) = SendMessageTool::new();
    runtime.register_tool(Box::new(send_msg_tool));
    info!("send_message tool registered (wire via SendMessageHandle before use)");

    // --- Agent Config ---
    if let Some(prompt) = &config.agent.system_prompt {
        runtime.set_system_prompt(prompt.clone());
    }
    if let Some(max_tokens) = config.agent.max_tokens {
        runtime.set_max_tokens(max_tokens);
    }
    if let Some(max_context_tokens) = config.agent.max_context_tokens {
        runtime.set_max_context_tokens(max_context_tokens);
    }
    if let Some(limit) = config.memory.recall_limit {
        runtime.set_recall_limit(limit);
    }
    if let Some(enabled) = config.memory.summarization {
        runtime.set_summarization_enabled(enabled);
    }
    if config.debug {
        runtime.set_debug(true);
        info!("debug mode enabled: tool calls will be shown in responses");
    }
    if let Some(limit) = config.agent.skill_recall_limit {
        runtime.set_skill_recall_limit(limit);
    }

    // --- Skills ---
    let skills_dir = opencrust_config::ConfigLoader::default_config_dir().join("skills");
    let scanner = opencrust_skills::SkillScanner::new(&skills_dir);
    match scanner.discover() {
        Ok(skills) => {
            let count = skills.len();
            runtime.index_skills(skills).await;
            if count > 0 {
                info!("indexed {} skill(s) for semantic retrieval", count);
            }
        }
        Err(e) => warn!("failed to scan skills directory: {e}"),
    }

    // --- DNA (personality from dna.md) ---
    let dna_path = opencrust_config::ConfigLoader::default_config_dir().join("dna.md");
    match std::fs::read_to_string(&dna_path) {
        Ok(content) if !content.trim().is_empty() => {
            runtime.set_dna_content(Some(content));
            info!("loaded dna.md from {}", dna_path.display());
        }
        Ok(_) => {}                                              // empty file, ignore
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // no dna.md, that's fine
        Err(e) => warn!("failed to read dna.md: {e}"),
    }

    (runtime, send_msg_handle)
}

/// Resolve MCP server env vars through the vault. Empty values trigger a
/// vault lookup with key `MCP_{SERVER}_{ENV_KEY}`, falling back to the
/// process environment. Non-empty values pass through unchanged.
fn resolve_mcp_env(server_name: &str, env: &HashMap<String, String>) -> HashMap<String, String> {
    let vault_path = default_vault_path();
    let mut resolved = HashMap::new();
    for (key, value) in env {
        if value.is_empty() {
            let vault_key = format!(
                "MCP_{}_{}",
                server_name.to_uppercase().replace('-', "_"),
                key
            );
            if let Some(ref vp) = vault_path
                && let Some(secret) = opencrust_security::try_vault_get(vp, &vault_key)
            {
                resolved.insert(key.clone(), secret);
                continue;
            }
            if let Ok(env_val) = std::env::var(key) {
                resolved.insert(key.clone(), env_val);
                continue;
            }
        }
        resolved.insert(key.clone(), value.clone());
    }
    resolved
}

/// Render a list of skill definitions into the `# Active Skills` prompt block.
pub fn build_skill_block(skills: &[opencrust_skills::SkillDefinition]) -> String {
    let mut block = String::from("# Active Skills\n");
    for skill in skills {
        block.push_str(&format!(
            "\n## {}\n{}\n",
            skill.frontmatter.name, skill.frontmatter.description
        ));
        if !skill.frontmatter.triggers.is_empty() {
            block.push_str(&format!(
                "Triggers: {}\n",
                skill.frontmatter.triggers.join(", ")
            ));
        }
        block.push('\n');
        block.push_str(&skill.body);
        block.push('\n');
    }
    block
}

/// Build MCP tools from merged config (config.yml + mcp.json).
///
/// Returns the Arc-wrapped manager, a flat list of bridged tools (including
/// the resource tool), and an optional instructions string from server handshakes.
pub async fn build_mcp_tools(
    config: &AppConfig,
) -> (Arc<McpManager>, Vec<Box<dyn Tool>>, Option<String>) {
    let loader = match opencrust_config::ConfigLoader::new() {
        Ok(l) => l,
        Err(e) => {
            warn!("failed to create config loader for MCP: {e}");
            return (Arc::new(McpManager::new()), Vec::new(), None);
        }
    };

    let mcp_configs = loader.merged_mcp_config(config);
    if mcp_configs.is_empty() {
        return (Arc::new(McpManager::new()), Vec::new(), None);
    }

    let manager = McpManager::new();
    let mut all_tools: Vec<Box<dyn Tool>> = Vec::new();

    for (name, server_config) in &mcp_configs {
        let enabled = server_config.enabled.unwrap_or(true);
        if !enabled {
            info!("MCP server '{name}' is disabled, skipping");
            continue;
        }

        let timeout_secs = server_config.timeout.unwrap_or(30);

        let resolved_env = resolve_mcp_env(name, &server_config.env);

        let connect_result = match server_config.transport.as_str() {
            "stdio" => {
                manager
                    .connect(
                        name,
                        &server_config.command,
                        &server_config.args,
                        &resolved_env,
                        timeout_secs,
                    )
                    .await
            }
            "http" => {
                let Some(url) = &server_config.url else {
                    warn!(
                        "MCP server '{name}' uses HTTP transport but no 'url' configured, skipping"
                    );
                    continue;
                };
                manager.connect_http(name, url, timeout_secs).await
            }
            other => {
                warn!("MCP server '{name}' uses unsupported transport '{other}', skipping");
                continue;
            }
        };

        match connect_result {
            Ok(()) => {
                let tools = manager
                    .take_tools(name, std::time::Duration::from_secs(timeout_secs))
                    .await;
                info!("MCP server '{name}': registered {} tool(s)", tools.len());
                all_tools.extend(tools);
            }
            Err(e) => {
                warn!("failed to connect MCP server '{name}': {e}");
            }
        }
    }

    // Collect server instructions
    let all_instructions = manager.get_all_instructions().await;
    let instructions_text = if all_instructions.is_empty() {
        None
    } else {
        let mut text = "MCP server instructions:\n".to_string();
        for (server_name, inst) in &all_instructions {
            text.push_str(&format!("\n[{server_name}]: {inst}\n"));
        }
        Some(text)
    };

    // Arc-wrap the manager and add the resource tool
    let manager = Arc::new(manager);
    all_tools.push(Box::new(opencrust_agents::McpResourceTool::new(
        Arc::clone(&manager),
    )));

    (manager, all_tools, instructions_text)
}

/// Build configured channels that can be initialized before state is wrapped in Arc.
pub async fn build_channels(config: &AppConfig) -> opencrust_channels::ChannelRegistry {
    // Load .env file if present (idempotent, will not overwrite existing env vars)
    if let Err(e) = dotenvy::dotenv() {
        tracing::debug!("no .env file loaded: {e}");
    }

    let registry = opencrust_channels::ChannelRegistry::new();

    for (name, channel_config) in &config.channels {
        let enabled = channel_config.enabled.unwrap_or(true);
        if !enabled {
            info!("channel {name} is disabled, skipping");
            continue;
        }

        match channel_config.channel_type.as_str() {
            "discord" => {
                // Discord channels need SharedState for callbacks, so they are started later.
                info!("discord channel {name} will be started after state initialization");
            }
            "telegram" => {
                // Telegram channels need SharedState for callbacks, so they are started later.
                info!("telegram channel {name} will be started after state initialization");
            }
            "slack" => {
                // Slack channels need SharedState for callbacks, so they are started later.
                info!("slack channel {name} will be started after state initialization");
            }
            "whatsapp" => {
                // WhatsApp channels need SharedState for callbacks, so they are started later.
                info!("whatsapp channel {name} will be started after state initialization");
            }
            "imessage" => {
                // iMessage channels need SharedState for callbacks, so they are started later.
                info!("imessage channel {name} will be started after state initialization");
            }
            "mqtt" => {
                info!("mqtt channel {name} will be started after state initialization");
            }
            other => {
                warn!("unknown channel type: {other} for channel {name}, skipping");
            }
        }
    }

    registry
}

/// Build Discord channels from config. Must be called after state is
/// wrapped in `Arc` so the message callback can capture a `SharedState`.
pub fn build_discord_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<Box<dyn opencrust_channels::Channel>> {
    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "discord" || channel_config.enabled == Some(false) {
            continue;
        }

        // Inject secrets from env vars into the settings map.
        let mut settings = channel_config.settings.clone();
        if let Ok(token) = std::env::var("DISCORD_BOT_TOKEN") {
            settings.insert("bot_token".to_string(), serde_json::json!(token));
        }
        if let Ok(app_id) = std::env::var("DISCORD_APP_ID")
            && let Ok(id) = app_id.parse::<u64>()
        {
            settings.insert("application_id".to_string(), serde_json::json!(id));
        }

        let allowlist = Arc::clone(&state.allowlist);
        let pairing = Arc::clone(&state.pairing);

        let policy = Arc::new(ChannelPolicy::from_settings(&settings));

        let group_filter: opencrust_channels::discord::DiscordGroupFilter = {
            let policy = Arc::clone(&policy);
            Arc::new(move |is_mentioned| policy.should_process_group(is_mentioned))
        };

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());
        let auto_reply_voice_discord = config.voice.auto_reply_voice;
        let tts_provider_discord = state.tts_provider.clone();
        let tts_max_chars_discord = config
            .voice
            .tts_max_chars
            .unwrap_or(opencrust_media::TTS_DEFAULT_MAX_CHARS);
        let data_dir_discord = config
            .data_dir
            .clone()
            .unwrap_or_else(|| opencrust_config::ConfigLoader::default_config_dir().join("data"));

        let inject_user_name_discord = channel_config
            .settings
            .get("inject_user_name")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let on_message: opencrust_channels::discord::DiscordOnMessageFn = Arc::new(
            move |channel_id: String,
                  user_id: String,
                  user_name: String,
                  text: String,
                  is_group: bool,
                  file: Option<opencrust_channels::discord::DiscordFile>,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                let tts = tts_provider_discord.clone();
                let tts_max_chars = tts_max_chars_discord;
                let data_dir = data_dir_discord.clone();
                Box::pin(async move {
                    let session_id = format!("discord-{channel_id}");

                    // --- /ingest command ---
                    if let Some(cmd) = text.strip_prefix('!').or_else(|| text.strip_prefix('/')) {
                        let cmd_word = cmd.split_whitespace().next().unwrap_or("");
                        if cmd_word == "ingest" {
                            if let Some(pending) = state.take_pending_file(&session_id) {
                                return crate::ingest::run_ingest(
                                    &state,
                                    &data_dir,
                                    &text,
                                    &pending.filename,
                                    &pending.data,
                                )
                                .await;
                            } else {
                                return Ok(ChannelResponse::Text(
                                    "No pending file. Send a document first, then use !ingest."
                                        .to_string(),
                                ));
                            }
                        }

                        // All other slash commands are handled synchronously.
                        return handle_discord_command(
                            cmd_word,
                            &user_id,
                            &user_name,
                            &channel_id,
                            &allowlist,
                            &pairing,
                            &policy,
                            &state,
                        )
                        .map(ChannelResponse::Text);
                    }

                    // --- File handling ---
                    if let Some(discord_file) = file {
                        let fname = discord_file.filename.clone();
                        let caption = text.trim().to_lowercase();
                        if caption.contains("ingest") {
                            return crate::ingest::run_ingest(
                                &state,
                                &data_dir,
                                &caption,
                                &fname,
                                &discord_file.data,
                            )
                            .await;
                        } else {
                            state.set_pending_file(
                                &session_id,
                                crate::state::PendingFile {
                                    filename: fname.clone(),
                                    data: discord_file.data,
                                    received_at: std::time::Instant::now(),
                                },
                            );
                            return Ok(ChannelResponse::Text(format!(
                                "Received {fname}. Use !ingest to store it for future reference."
                            )));
                        }
                    }

                    // Groups already filtered by channel handler - skip auth for groups
                    if !is_group {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_name, &text, "discord",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(ChannelResponse::Text(welcome)),
                            Err(e) => return Err(e),
                        }
                    }

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );
                    if inject_user_name_discord {
                        state.agents.set_session_user_name(&session_id, &user_name);
                    }

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                        return Err(format!(
                            "message too long (max {max_input_chars} characters)"
                        ));
                    }
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }

                    state
                        .hydrate_session_history(&session_id, Some("discord"), Some(&user_id))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&user_id));
                    let summary = state.session_summary(&session_id);

                    let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    if let Some(s) = new_summary {
                        state.update_session_summary(&session_id, &s);
                    }

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );

                    state
                        .persist_turn(
                            &session_id,
                            Some("discord"),
                            Some(&user_id),
                            &text,
                            &response,
                            Some(serde_json::json!({"discord_channel_id": channel_id})),
                        )
                        .await;

                    if let Some((input, output, provider, model)) =
                        state.agents.take_session_usage(&session_id)
                    {
                        state
                            .persist_usage(&session_id, &provider, &model, input, output)
                            .await;
                    }

                    // TTS: synthesize voice response if configured
                    if auto_reply_voice_discord && let Some(ref provider) = tts {
                        let tts_input = opencrust_media::truncate_for_tts(&response, tts_max_chars);
                        match provider.synthesize(tts_input).await {
                            Ok(audio) => {
                                return Ok(ChannelResponse::Voice {
                                    text: response,
                                    audio,
                                });
                            }
                            Err(e) => {
                                warn!("tts synthesis failed, falling back to text: {e}");
                            }
                        }
                    }
                    Ok(ChannelResponse::Text(response))
                })
            },
        );

        match opencrust_channels::discord::config::DiscordConfig::from_settings(&settings) {
            Ok(discord_config) => {
                let channel = opencrust_channels::discord::DiscordChannel::with_group_filter(
                    discord_config,
                    on_message,
                    group_filter,
                )
                .with_name(name.clone());
                channels.push(Box::new(channel) as Box<dyn opencrust_channels::Channel>);
                info!("configured discord channel: {name}");
            }
            Err(e) => {
                warn!("failed to configure discord channel {name}: {e}");
            }
        }
    }

    channels
}

/// Transcribe voice audio using the Whisper API.
///
/// Priority:
/// 1. Local Whisper server (`stt_base_url` in config) — no API key required
/// 2. `voice.api_key` from config (used for OpenAI)
/// 3. `OPENAI_API_KEY` env var
/// 4. `GROQ_API_KEY` env var
async fn transcribe_voice(
    audio_bytes: &[u8],
    stt_base_url: Option<&str>,
    stt_model: Option<&str>,
    config_api_key: Option<&str>,
) -> std::result::Result<String, String> {
    // 1. Local Whisper server — no API key needed
    if let Some(base_url) = stt_base_url {
        let endpoint = format!("{}/v1/audio/transcriptions", base_url.trim_end_matches('/'));
        let model = stt_model.unwrap_or("Systran/faster-whisper-large-v3");
        return whisper_transcribe(audio_bytes, "", &endpoint, model).await;
    }

    // 2. API key from config
    if let Some(key) = config_api_key {
        return whisper_transcribe(
            audio_bytes,
            key,
            "https://api.openai.com/v1/audio/transcriptions",
            "whisper-1",
        )
        .await;
    }

    // 3. OpenAI env var
    if let Some(key) = resolve_api_key(None, "OPENAI_API_KEY", "OPENAI_API_KEY") {
        return whisper_transcribe(
            audio_bytes,
            &key,
            "https://api.openai.com/v1/audio/transcriptions",
            "whisper-1",
        )
        .await;
    }

    // 4. Groq env var
    if let Some(key) = resolve_api_key(None, "GROQ_API_KEY", "GROQ_API_KEY") {
        return whisper_transcribe(
            audio_bytes,
            &key,
            "https://api.groq.com/openai/v1/audio/transcriptions",
            "whisper-large-v3-turbo",
        )
        .await;
    }

    Err("Voice messages require a transcription source. Options:\n\
         - Set voice.stt_base_url in config.yml for a local Whisper server\n\
         - Set voice.api_key for OpenAI Whisper\n\
         - Set GROQ_API_KEY env var for free Groq Whisper"
        .to_string())
}

async fn whisper_transcribe(
    audio_bytes: &[u8],
    api_key: &str,
    endpoint: &str,
    model: &str,
) -> std::result::Result<String, String> {
    let client = reqwest::Client::new();

    let file_part = reqwest::multipart::Part::bytes(audio_bytes.to_vec())
        .file_name("voice.ogg")
        .mime_str("audio/ogg")
        .map_err(|e| format!("failed to build multipart: {e}"))?;

    let form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", model.to_string());

    let mut req = client.post(endpoint).multipart(form);
    if !api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {api_key}"));
    }
    let response = req
        .send()
        .await
        .map_err(|e| format!("whisper request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("whisper API error: status={status}, body={body}"));
    }

    #[derive(serde::Deserialize)]
    struct WhisperResponse {
        text: String,
    }

    let result: WhisperResponse = response
        .json()
        .await
        .map_err(|e| format!("failed to parse whisper response: {e}"))?;

    Ok(result.text)
}

/// Build Telegram channels from config. Must be called after state is
/// wrapped in `Arc` so the message callback can capture a `SharedState`.
pub fn build_telegram_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<Box<dyn opencrust_channels::Channel>> {
    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "telegram" || channel_config.enabled == Some(false) {
            continue;
        }

        let bot_token = channel_config
            .settings
            .get("bot_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let bot_token =
            bot_token.or_else(|| resolve_api_key(None, "TELEGRAM_BOT_TOKEN", "TELEGRAM_BOT_TOKEN"));

        let Some(bot_token) = bot_token else {
            warn!(
                "telegram channel '{name}' has no bot_token, skipping \
                 (set bot_token in config or TELEGRAM_BOT_TOKEN env var)"
            );
            continue;
        };

        let allowlist = Arc::clone(&state.allowlist);

        let pairing = Arc::clone(&state.pairing);

        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        let group_filter: opencrust_channels::GroupFilter = {
            let policy = Arc::clone(&policy);
            Arc::new(move |is_mentioned| policy.should_process_group(is_mentioned))
        };

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());
        let auto_reply_voice = config.voice.auto_reply_voice;
        let tts_provider = state.tts_provider.clone();
        let tts_max_chars = config
            .voice
            .tts_max_chars
            .unwrap_or(opencrust_media::TTS_DEFAULT_MAX_CHARS);
        let stt_base_url: Option<String> = config.voice.stt_base_url.clone();
        let stt_model: Option<String> = config.voice.stt_model.clone();
        // Resolve STT API key via vault → config → env (same chain as all other keys).
        let stt_api_key: Option<String> = resolve_api_key(
            config.voice.api_key.as_deref(),
            "VOICE_API_KEY",
            "VOICE_API_KEY",
        );
        let data_dir = config
            .data_dir
            .clone()
            .unwrap_or_else(|| opencrust_config::ConfigLoader::default_config_dir().join("data"));

        let inject_user_name_tg = channel_config
            .settings
            .get("inject_user_name")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let on_message: opencrust_channels::OnMessageFn = Arc::new(
            move |chat_id: i64,
                  user_id: String,
                  user_name: String,
                  text: String,
                  is_group: bool,
                  attachment: Option<MediaAttachment>,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                let tts = tts_provider.clone();
                let tts_max_chars = tts_max_chars;
                let stt_base_url = stt_base_url.clone();
                let stt_model = stt_model.clone();
                let stt_api_key = stt_api_key.clone();
                let data_dir = data_dir.clone();
                Box::pin(async move {
                    // --- Command handling (text-only) ---
                    if let Some(cmd) = text.strip_prefix('!').or_else(|| text.strip_prefix('/')) {
                        let cmd = cmd.split_whitespace().next().unwrap_or("");

                        // /ingest - async, needs data_dir and embedding provider
                        if cmd == "ingest" {
                            let session_id = format!("telegram-{chat_id}");
                            if let Some(pending) = state.take_pending_file(&session_id) {
                                return crate::ingest::run_ingest(
                                    &state,
                                    &data_dir,
                                    &text,
                                    &pending.filename,
                                    &pending.data,
                                )
                                .await;
                            } else {
                                return Ok(ChannelResponse::Text(
                                    "No pending file. Send a document first, then use !ingest."
                                        .to_string(),
                                ));
                            }
                        }

                        return handle_command(
                            cmd, &text, &user_id, &user_name, chat_id, &allowlist, &pairing,
                            &policy, &state,
                        )
                        .map(ChannelResponse::Text);
                    }

                    // --- Auth / pairing (skip for groups - already filtered by channel handler) ---
                    if !is_group {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_name, &text, "telegram",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(ChannelResponse::Text(welcome)),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("telegram-{chat_id}");

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );
                    if inject_user_name_tg {
                        state.agents.set_session_user_name(&session_id, &user_name);
                    }

                    // --- Handle media or text ---
                    match attachment {
                        Some(MediaAttachment::Voice { data, duration }) => {
                            let transcript = transcribe_voice(
                                &data,
                                stt_base_url.as_deref(),
                                stt_model.as_deref(),
                                stt_api_key.as_deref(),
                            )
                            .await?;
                            info!(
                                "telegram voice transcribed: {} chars from {}s audio",
                                transcript.len(),
                                duration
                            );

                            let text = opencrust_security::InputValidator::sanitize(&transcript);
                            if opencrust_security::InputValidator::check_prompt_injection(&text) {
                                return Err("input rejected: potential prompt injection detected"
                                    .to_string());
                            }
                            if opencrust_security::InputValidator::exceeds_length(
                                &text,
                                max_input_chars,
                            ) {
                                return Err(format!(
                                    "input rejected: message exceeds {max_input_chars} character limit"
                                ));
                            }

                            state
                                .hydrate_session_history(
                                    &session_id,
                                    Some("telegram"),
                                    Some(&user_id),
                                )
                                .await;
                            let history: Vec<ChatMessage> = state.session_history(&session_id);
                            let continuity_key = state.continuity_key(Some(&user_id));
                            let summary = state.session_summary(&session_id);

                            let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                                state
                                    .agents
                                    .process_message_streaming_with_context_and_summary(
                                        &session_id,
                                        &text,
                                        &history,
                                        delta_sender,
                                        summary.as_deref(),
                                        continuity_key.as_deref(),
                                        Some(&user_id),
                                    )
                                    .await
                            } else {
                                state
                                    .agents
                                    .process_message_with_context_and_summary(
                                        &session_id,
                                        &text,
                                        &history,
                                        summary.as_deref(),
                                        continuity_key.as_deref(),
                                        Some(&user_id),
                                    )
                                    .await
                            }
                            .map_err(|e| e.to_string())?;

                            if let Some(s) = new_summary {
                                state.update_session_summary(&session_id, &s);
                            }

                            let response = opencrust_security::InputValidator::truncate_output(
                                &response,
                                max_output_chars,
                            );
                            state
                                .persist_turn(
                                    &session_id,
                                    Some("telegram"),
                                    Some(&user_id),
                                    &text,
                                    &response,
                                    Some(serde_json::json!({"telegram_chat_id": chat_id})),
                                )
                                .await;
                            if let Some((input, output, provider, model)) =
                                state.agents.take_session_usage(&session_id)
                            {
                                state
                                    .persist_usage(&session_id, &provider, &model, input, output)
                                    .await;
                            }
                            let response = opencrust_security::InputValidator::truncate_output(
                                &response,
                                max_output_chars,
                            );
                            // TTS: synthesize voice response if configured
                            if auto_reply_voice && let Some(ref provider) = tts {
                                let tts_input =
                                    opencrust_media::truncate_for_tts(&response, tts_max_chars);
                                match provider.synthesize(tts_input).await {
                                    Ok(audio) => {
                                        return Ok(ChannelResponse::Voice {
                                            text: response,
                                            audio,
                                        });
                                    }
                                    Err(e) => {
                                        warn!("tts synthesis failed, falling back to text: {e}");
                                    }
                                }
                            }
                            Ok(ChannelResponse::Text(response))
                        }
                        Some(MediaAttachment::Photo { data, caption }) => {
                            use base64::Engine;
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                            let data_url = format!("data:image/jpeg;base64,{b64}");
                            let caption_text = opencrust_security::InputValidator::sanitize(
                                &caption.unwrap_or_else(|| "Describe this image.".to_string()),
                            );
                            if opencrust_security::InputValidator::exceeds_length(
                                &caption_text,
                                max_input_chars,
                            ) {
                                return Err(format!(
                                    "input rejected: message exceeds {max_input_chars} character limit"
                                ));
                            }
                            if opencrust_security::InputValidator::check_prompt_injection(
                                &caption_text,
                            ) {
                                return Err("input rejected: potential prompt injection detected"
                                    .to_string());
                            }

                            let blocks = vec![
                                opencrust_agents::ContentBlock::Image { url: data_url },
                                opencrust_agents::ContentBlock::Text {
                                    text: caption_text.clone(),
                                },
                            ];

                            state
                                .hydrate_session_history(
                                    &session_id,
                                    Some("telegram"),
                                    Some(&user_id),
                                )
                                .await;
                            let history: Vec<ChatMessage> = state.session_history(&session_id);
                            let continuity_key = state.continuity_key(Some(&user_id));
                            let summary = state.session_summary(&session_id);

                            let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                                state
                                    .agents
                                    .process_message_streaming_with_blocks_and_summary(
                                        &session_id,
                                        blocks,
                                        &caption_text,
                                        &history,
                                        delta_sender,
                                        summary.as_deref(),
                                        continuity_key.as_deref(),
                                        Some(&user_id),
                                    )
                                    .await
                            } else {
                                state
                                    .agents
                                    .process_message_with_blocks_and_summary(
                                        &session_id,
                                        blocks,
                                        &caption_text,
                                        &history,
                                        summary.as_deref(),
                                        continuity_key.as_deref(),
                                        Some(&user_id),
                                    )
                                    .await
                            }
                            .map_err(|e| e.to_string())?;

                            if let Some(s) = new_summary {
                                state.update_session_summary(&session_id, &s);
                            }

                            let response = opencrust_security::InputValidator::truncate_output(
                                &response,
                                max_output_chars,
                            );
                            state
                                .persist_turn(
                                    &session_id,
                                    Some("telegram"),
                                    Some(&user_id),
                                    &caption_text,
                                    &response,
                                    Some(serde_json::json!({"telegram_chat_id": chat_id})),
                                )
                                .await;
                            if let Some((input, output, provider, model)) =
                                state.agents.take_session_usage(&session_id)
                            {
                                state
                                    .persist_usage(&session_id, &provider, &model, input, output)
                                    .await;
                            }
                            let response = opencrust_security::InputValidator::truncate_output(
                                &response,
                                max_output_chars,
                            );
                            Ok(ChannelResponse::Text(response))
                        }
                        Some(MediaAttachment::Document {
                            data,
                            filename,
                            mime_type: _,
                            caption,
                        }) => {
                            if data.len() > 10 * 1024 * 1024 {
                                return Err("File too large. Maximum size is 10MB.".to_string());
                            }

                            let fname = filename.unwrap_or_else(|| "file".to_string());
                            let caption_text = caption.unwrap_or_default().trim().to_lowercase();

                            // If caption contains "ingest", ingest immediately
                            if caption_text.contains("ingest") {
                                return crate::ingest::run_ingest(
                                    &state,
                                    &data_dir,
                                    &caption_text,
                                    &fname,
                                    &data,
                                )
                                .await;
                            } else {
                                // Store as pending and prompt
                                state.set_pending_file(
                                    &session_id,
                                    crate::state::PendingFile {
                                        filename: fname.clone(),
                                        data,
                                        received_at: std::time::Instant::now(),
                                    },
                                );
                                Ok(ChannelResponse::Text(format!(
                                    "Received {fname}. Use !ingest to store it for future reference."
                                )))
                            }
                        }
                        None => {
                            // Regular text-only path
                            let text = opencrust_security::InputValidator::sanitize(&text);
                            if opencrust_security::InputValidator::check_prompt_injection(&text) {
                                return Err("input rejected: potential prompt injection detected"
                                    .to_string());
                            }
                            if opencrust_security::InputValidator::exceeds_length(
                                &text,
                                max_input_chars,
                            ) {
                                return Err(format!(
                                    "input rejected: message exceeds {max_input_chars} character limit"
                                ));
                            }

                            state
                                .hydrate_session_history(
                                    &session_id,
                                    Some("telegram"),
                                    Some(&user_id),
                                )
                                .await;
                            let history: Vec<ChatMessage> = state.session_history(&session_id);
                            let continuity_key = state.continuity_key(Some(&user_id));
                            let summary = state.session_summary(&session_id);

                            let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                                state
                                    .agents
                                    .process_message_streaming_with_context_and_summary(
                                        &session_id,
                                        &text,
                                        &history,
                                        delta_sender,
                                        summary.as_deref(),
                                        continuity_key.as_deref(),
                                        Some(&user_id),
                                    )
                                    .await
                            } else {
                                state
                                    .agents
                                    .process_message_with_context_and_summary(
                                        &session_id,
                                        &text,
                                        &history,
                                        summary.as_deref(),
                                        continuity_key.as_deref(),
                                        Some(&user_id),
                                    )
                                    .await
                            }
                            .map_err(|e| e.to_string())?;

                            if let Some(s) = new_summary {
                                state.update_session_summary(&session_id, &s);
                            }

                            let response = opencrust_security::InputValidator::truncate_output(
                                &response,
                                max_output_chars,
                            );
                            state
                                .persist_turn(
                                    &session_id,
                                    Some("telegram"),
                                    Some(&user_id),
                                    &text,
                                    &response,
                                    Some(serde_json::json!({"telegram_chat_id": chat_id})),
                                )
                                .await;
                            if let Some((input, output, provider, model)) =
                                state.agents.take_session_usage(&session_id)
                            {
                                state
                                    .persist_usage(&session_id, &provider, &model, input, output)
                                    .await;
                            }
                            let response = opencrust_security::InputValidator::truncate_output(
                                &response,
                                max_output_chars,
                            );
                            Ok(ChannelResponse::Text(response))
                        }
                    }
                })
            },
        );

        let channel = TelegramChannel::with_group_filter(bot_token, on_message, group_filter)
            .with_name(name.clone());
        channels.push(Box::new(channel) as Box<dyn opencrust_channels::Channel>);
        info!("configured telegram channel: {name}");
    }

    channels
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    cmd: &str,
    _full_text: &str,
    user_id: &str,
    user_name: &str,
    chat_id: i64,
    allowlist: &Arc<Mutex<Allowlist>>,
    pairing: &Arc<Mutex<PairingManager>>,
    policy: &ChannelPolicy,
    state: &SharedState,
) -> std::result::Result<String, String> {
    // If dm_policy is Open, skip all auth checks in commands
    let dm_open = matches!(policy.authorize_dm(user_id), DmAuthResult::Allowed);
    let list = allowlist.lock().unwrap();
    let is_owner = dm_open || list.is_owner(user_id);
    let is_allowed = dm_open || list.is_allowed(user_id);
    drop(list);

    match cmd {
        "start" => {
            if is_allowed {
                Ok(
                    "Welcome to OpenCrust! Send me a message and I will respond.\n\n\
                    Commands:\n\
                    /help - show this help\n\
                    /clear - reset conversation history\n\
                    /pair - generate invite code (owner only)"
                        .to_string(),
                )
            } else {
                let mut list = allowlist.lock().unwrap();
                if list.needs_owner() {
                    list.claim_owner(user_id);
                    info!("telegram: auto-paired owner {} ({})", user_name, user_id);
                    Ok(format!(
                        "Welcome, {}! You are now the owner of this OpenCrust bot.\n\n\
                         Use /pair to generate a code for adding other users.",
                        user_name
                    ))
                } else {
                    Ok("This bot is private. Send the 6-digit pairing code you received to get access.".to_string())
                }
            }
        }
        "help" => {
            if !is_allowed {
                return Err("__blocked__".to_string());
            }
            let mut help = "OpenCrust Commands:\n\
                /help - show this help\n\
                /clear - reset conversation history\n\
                !ingest - store a sent document for future reference"
                .to_string();
            if is_owner {
                help.push_str(
                    "\n/pair - generate a 6-digit invite code\n/users - list allowed users",
                );
            }
            Ok(help)
        }
        "clear" => {
            if !is_allowed {
                return Err("__blocked__".to_string());
            }
            let session_id = format!("telegram-{chat_id}");
            if let Some(mut session) = state.sessions.get_mut(&session_id) {
                session.history.clear();
            }
            state.update_session_summary(&session_id, "");
            if let Some(store) = &state.session_store {
                let _ = store.prune_old_messages(&session_id, 0);
            }
            Ok("Conversation history cleared.".to_string())
        }
        "pair" => {
            if !is_owner {
                if !is_allowed {
                    return Err("__blocked__".to_string());
                }
                return Ok("Only the bot owner can generate pairing codes.".to_string());
            }
            let code = pairing.lock().unwrap().generate("telegram");
            Ok(format!(
                "Pairing code: {code}\n\n\
                 Share this with the person you want to invite. \
                 They should send this code to the bot within 5 minutes."
            ))
        }
        "users" => {
            if !is_owner {
                if !is_allowed {
                    return Err("__blocked__".to_string());
                }
                return Ok("Only the bot owner can list users.".to_string());
            }
            let list = allowlist.lock().unwrap();
            let users = list.list_users();
            let owner = list.owner().unwrap_or("none");
            Ok(format!(
                "Owner: {owner}\nAllowed users ({}):\n{}",
                users.len(),
                users.join("\n")
            ))
        }
        _ => {
            if !is_allowed {
                return Err("__blocked__".to_string());
            }
            Ok(format!(
                "Unknown command: /{cmd}\nUse /help for available commands."
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_discord_command(
    cmd: &str,
    user_id: &str,
    user_name: &str,
    channel_id: &str,
    allowlist: &Arc<Mutex<Allowlist>>,
    pairing: &Arc<Mutex<PairingManager>>,
    policy: &ChannelPolicy,
    state: &SharedState,
) -> std::result::Result<String, String> {
    let dm_open = matches!(policy.authorize_dm(user_id), DmAuthResult::Allowed);
    let list = allowlist.lock().unwrap();
    let is_owner = dm_open || list.is_owner(user_id);
    let is_allowed = dm_open || list.is_allowed(user_id);
    drop(list);

    match cmd {
        "start" => {
            if is_allowed {
                Ok(
                    "Welcome to OpenCrust! Send me a message and I will respond.\n\n\
                    Commands:\n\
                    /help - show this help\n\
                    /clear - reset conversation history\n\
                    /pair - generate invite code (owner only)"
                        .to_string(),
                )
            } else {
                let mut list = allowlist.lock().unwrap();
                if list.needs_owner() {
                    list.claim_owner(user_id);
                    info!("discord: auto-paired owner {} ({})", user_name, user_id);
                    Ok(format!(
                        "Welcome, {}! You are now the owner of this OpenCrust bot.\n\n\
                         Use /pair to generate a code for adding other users.",
                        user_name
                    ))
                } else {
                    Ok("This bot is private. Send the 6-digit pairing code you received to get access.".to_string())
                }
            }
        }
        "help" => {
            if !is_allowed {
                return Err("__blocked__".to_string());
            }
            let mut help = "OpenCrust Commands:\n\
                /help - show this help\n\
                /clear - reset conversation history\n\
                !ingest - store a sent document for future reference"
                .to_string();
            if is_owner {
                help.push_str(
                    "\n/pair - generate a 6-digit invite code\n/users - list allowed users",
                );
            }
            Ok(help)
        }
        "clear" => {
            if !is_allowed {
                return Err("__blocked__".to_string());
            }
            let session_id = format!("discord-{channel_id}");
            if let Some(mut session) = state.sessions.get_mut(&session_id) {
                session.history.clear();
            }
            Ok("Conversation history cleared.".to_string())
        }
        "pair" => {
            if !is_owner {
                if !is_allowed {
                    return Err("__blocked__".to_string());
                }
                return Ok("Only the bot owner can generate pairing codes.".to_string());
            }
            let code = pairing.lock().unwrap().generate("discord");
            Ok(format!(
                "Pairing code: {code}\n\n\
                 Share this with the person you want to invite. \
                 They should send this code to the bot within 5 minutes."
            ))
        }
        "users" => {
            if !is_owner {
                if !is_allowed {
                    return Err("__blocked__".to_string());
                }
                return Ok("Only the bot owner can list users.".to_string());
            }
            let list = allowlist.lock().unwrap();
            let users = list.list_users();
            let owner = list.owner().unwrap_or("none");
            Ok(format!(
                "Owner: {owner}\nAllowed users ({}):\n{}",
                users.len(),
                users.join("\n")
            ))
        }
        _ => {
            if !is_allowed {
                return Err("__blocked__".to_string());
            }
            Ok(format!(
                "Unknown command: /{cmd}\nUse /help for available commands."
            ))
        }
    }
}

/// Build Slack channels from config. Must be called after state is
/// wrapped in `Arc` so the message callback can capture a `SharedState`.
pub fn build_slack_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<Box<dyn opencrust_channels::Channel>> {
    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "slack" || channel_config.enabled == Some(false) {
            continue;
        }

        let bot_token = channel_config
            .settings
            .get("bot_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let bot_token =
            bot_token.or_else(|| resolve_api_key(None, "SLACK_BOT_TOKEN", "SLACK_BOT_TOKEN"));

        let Some(bot_token) = bot_token else {
            warn!(
                "slack channel '{name}' has no bot_token, skipping \
                 (set bot_token in config or SLACK_BOT_TOKEN env var)"
            );
            continue;
        };

        let app_token = channel_config
            .settings
            .get("app_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let app_token =
            app_token.or_else(|| resolve_api_key(None, "SLACK_APP_TOKEN", "SLACK_APP_TOKEN"));

        let Some(app_token) = app_token else {
            warn!(
                "slack channel '{name}' has no app_token, skipping \
                 (set app_token in config or SLACK_APP_TOKEN env var)"
            );
            continue;
        };

        let allowlist = Arc::clone(&state.allowlist);

        let pairing = Arc::clone(&state.pairing);

        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        let bot_user_id = channel_config
            .settings
            .get("bot_user_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let group_filter: SlackGroupFilter = {
            let policy = Arc::clone(&policy);
            Arc::new(move |is_mentioned| policy.should_process_group(is_mentioned))
        };

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());
        let auto_reply_voice_slack = config.voice.auto_reply_voice;
        let tts_provider_slack = state.tts_provider.clone();
        let tts_max_chars_slack = config
            .voice
            .tts_max_chars
            .unwrap_or(opencrust_media::TTS_DEFAULT_MAX_CHARS);
        let data_dir_slack = config
            .data_dir
            .clone()
            .unwrap_or_else(|| opencrust_config::ConfigLoader::default_config_dir().join("data"));

        let inject_user_name_slack = channel_config
            .settings
            .get("inject_user_name")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let on_message: SlackOnMessageFn = Arc::new(
            move |channel_id: String,
                  user_id: String,
                  user_name: String,
                  text: String,
                  is_group: bool,
                  file: Option<opencrust_channels::SlackFile>,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                let tts = tts_provider_slack.clone();
                let tts_max_chars = tts_max_chars_slack;
                let data_dir = data_dir_slack.clone();
                Box::pin(async move {
                    let session_id = format!("slack-{channel_id}");

                    // --- /ingest command ---
                    if let Some(cmd) = text.strip_prefix('!').or_else(|| text.strip_prefix('/')) {
                        let cmd_word = cmd.split_whitespace().next().unwrap_or("");
                        if cmd_word == "ingest" {
                            if let Some(pending) = state.take_pending_file(&session_id) {
                                return crate::ingest::run_ingest(
                                    &state,
                                    &data_dir,
                                    &text,
                                    &pending.filename,
                                    &pending.data,
                                )
                                .await;
                            } else {
                                return Ok(ChannelResponse::Text(
                                    "No pending file. Send a document first, then use !ingest."
                                        .to_string(),
                                ));
                            }
                        }
                    }

                    // --- File handling ---
                    if let Some(slack_file) = file {
                        let fname = slack_file.filename.clone();
                        let caption = text.trim().to_lowercase();
                        if caption.contains("ingest") {
                            return crate::ingest::run_ingest(
                                &state,
                                &data_dir,
                                &caption,
                                &fname,
                                &slack_file.data,
                            )
                            .await;
                        } else {
                            state.set_pending_file(
                                &session_id,
                                crate::state::PendingFile {
                                    filename: fname.clone(),
                                    data: slack_file.data,
                                    received_at: std::time::Instant::now(),
                                },
                            );
                            return Ok(ChannelResponse::Text(format!(
                                "Received {fname}. Use !ingest to store it for future reference."
                            )));
                        }
                    }

                    // Groups already filtered by channel handler - skip auth for groups
                    if !is_group {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_name, &text, "slack",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(ChannelResponse::Text(welcome)),
                            Err(e) => return Err(e),
                        }
                    }

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );
                    if inject_user_name_slack {
                        state.agents.set_session_user_name(&session_id, &user_name);
                    }

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }
                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                        return Err(format!(
                            "input rejected: message exceeds {max_input_chars} character limit"
                        ));
                    }

                    state
                        .hydrate_session_history(&session_id, Some("slack"), Some(&user_id))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&user_id));
                    let summary = state.session_summary(&session_id);

                    let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    if let Some(s) = new_summary {
                        state.update_session_summary(&session_id, &s);
                    }

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    state
                        .persist_turn(
                            &session_id,
                            Some("slack"),
                            Some(&user_id),
                            &text,
                            &response,
                            Some(serde_json::json!({"slack_channel_id": channel_id})),
                        )
                        .await;

                    if let Some((input, output, provider, model)) =
                        state.agents.take_session_usage(&session_id)
                    {
                        state
                            .persist_usage(&session_id, &provider, &model, input, output)
                            .await;
                    }

                    // Slack has no native audio API — always return text.
                    // TTS is not attempted to avoid wasted synthesis.
                    let _ = (auto_reply_voice_slack, tts, tts_max_chars);
                    Ok(ChannelResponse::Text(response))
                })
            },
        );

        let channel = SlackChannel::with_group_filter(
            bot_token,
            app_token,
            on_message,
            group_filter,
            bot_user_id,
        )
        .with_name(name.clone());
        channels.push(Box::new(channel) as Box<dyn opencrust_channels::Channel>);
        info!("configured slack channel: {name}");
    }

    channels
}

/// Build WhatsApp channels from config. Must be called after state is
/// wrapped in `Arc` so the message callback can capture a `SharedState`.
pub fn build_whatsapp_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<Arc<WhatsAppChannel>> {
    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "whatsapp" || channel_config.enabled == Some(false) {
            continue;
        }

        // Mode detection: explicit "web" mode -> skip (handled by build_whatsapp_web_channels)
        let mode = channel_config
            .settings
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if mode == "web" {
            continue;
        }

        let access_token = channel_config
            .settings
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let access_token = access_token
            .or_else(|| resolve_api_key(None, "WHATSAPP_ACCESS_TOKEN", "WHATSAPP_ACCESS_TOKEN"));

        let Some(access_token) = access_token else {
            // No access_token and no explicit mode - might be a web channel
            if mode.is_empty() {
                info!(
                    "whatsapp channel '{name}' has no access_token and no mode set, \
                     will try as whatsapp-web"
                );
            } else {
                warn!(
                    "whatsapp channel '{name}' has no access_token, skipping \
                     (set access_token in config or WHATSAPP_ACCESS_TOKEN env var)"
                );
            }
            continue;
        };

        let phone_number_id = channel_config
            .settings
            .get("phone_number_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if phone_number_id.is_empty() {
            warn!("whatsapp channel '{name}' has no phone_number_id, skipping");
            continue;
        }

        let verify_token = channel_config
            .settings
            .get("verify_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| std::env::var("WHATSAPP_VERIFY_TOKEN").ok())
            .unwrap_or_else(|| "opencrust-verify".to_string());

        let allowlist = Arc::clone(&state.allowlist);

        let pairing = Arc::clone(&state.pairing);

        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());
        let data_dir_wa = config
            .data_dir
            .clone()
            .unwrap_or_else(|| opencrust_config::ConfigLoader::default_config_dir().join("data"));

        let inject_user_name_wa = channel_config
            .settings
            .get("inject_user_name")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let on_message: WhatsAppOnMessageFn = Arc::new(
            move |from_number: String,
                  user_name: String,
                  text: String,
                  _is_group: bool,
                  file: Option<opencrust_channels::whatsapp::WhatsAppFile>,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                let data_dir = data_dir_wa.clone();
                Box::pin(async move {
                    let session_id = format!("whatsapp-{from_number}");

                    // WhatsApp Business is DM-only, always check auth
                    {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy,
                            &mut list,
                            &pairing,
                            &from_number,
                            &user_name,
                            &text,
                            "whatsapp",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(ChannelResponse::Text(welcome)),
                            Err(e) => return Err(e),
                        }
                    }

                    // --- /ingest command ---
                    if let Some(cmd) = text.strip_prefix('!').or_else(|| text.strip_prefix('/')) {
                        let cmd_word = cmd.split_whitespace().next().unwrap_or("");
                        if cmd_word == "ingest" {
                            if let Some(pending) = state.take_pending_file(&session_id) {
                                return crate::ingest::run_ingest(
                                    &state,
                                    &data_dir,
                                    &text,
                                    &pending.filename,
                                    &pending.data,
                                )
                                .await;
                            } else {
                                return Ok(ChannelResponse::Text(
                                    "No pending file. Send a document first, then use !ingest."
                                        .to_string(),
                                ));
                            }
                        }
                    }

                    // --- File handling ---
                    if let Some(wa_file) = file {
                        let fname = wa_file.filename.clone();
                        let caption = text.trim().to_lowercase();
                        if caption.contains("ingest") {
                            return crate::ingest::run_ingest(
                                &state,
                                &data_dir,
                                &caption,
                                &fname,
                                &wa_file.data,
                            )
                            .await;
                        } else {
                            state.set_pending_file(
                                &session_id,
                                crate::state::PendingFile {
                                    filename: fname.clone(),
                                    data: wa_file.data,
                                    received_at: std::time::Instant::now(),
                                },
                            );
                            return Ok(ChannelResponse::Text(format!(
                                "Received {fname}. Use !ingest to store it for future reference."
                            )));
                        }
                    }

                    state.check_user_rate_limit(&from_number, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &from_number, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );
                    if inject_user_name_wa {
                        state.agents.set_session_user_name(&session_id, &user_name);
                    }

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }
                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                        return Err(format!(
                            "input rejected: message exceeds {max_input_chars} character limit"
                        ));
                    }

                    state
                        .hydrate_session_history(&session_id, Some("whatsapp"), Some(&from_number))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&from_number));
                    let summary = state.session_summary(&session_id);

                    let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&from_number),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&from_number),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    if let Some(s) = new_summary {
                        state.update_session_summary(&session_id, &s);
                    }

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    state
                        .persist_turn(
                            &session_id,
                            Some("whatsapp"),
                            Some(&from_number),
                            &text,
                            &response,
                            Some(serde_json::json!({"whatsapp_from": from_number})),
                        )
                        .await;

                    if let Some((input, output, provider, model)) =
                        state.agents.take_session_usage(&session_id)
                    {
                        state
                            .persist_usage(&session_id, &provider, &model, input, output)
                            .await;
                    }

                    Ok(ChannelResponse::Text(response))
                })
            },
        );

        let channel = WhatsAppChannel::new(access_token, phone_number_id, verify_token, on_message)
            .with_name(name.clone());
        channels.push(Arc::new(channel));
        info!("configured whatsapp channel: {name}");
    }

    channels
}

/// Build WhatsApp Web channels from config (sidecar-driven, QR code pairing).
///
/// Picks up channels where `mode == "web"` or where no `access_token` is set
/// (auto-detect as web mode).
pub fn build_whatsapp_web_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<WhatsAppWebChannel> {
    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "whatsapp" || channel_config.enabled == Some(false) {
            continue;
        }

        let mode = channel_config
            .settings
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let has_access_token = channel_config
            .settings
            .get("access_token")
            .and_then(|v| v.as_str())
            .is_some()
            || resolve_api_key(None, "WHATSAPP_ACCESS_TOKEN", "WHATSAPP_ACCESS_TOKEN").is_some();

        // Only build as web if explicitly "web" or no access_token (and not explicitly "business")
        if mode == "business" {
            continue;
        }
        if mode != "web" && has_access_token {
            continue;
        }

        let allowlist = Arc::clone(&state.allowlist);

        let pairing = Arc::clone(&state.pairing);

        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        // Warn if group_policy: mention is set - WhatsApp has no mention detection
        if channel_config
            .settings
            .get("group_policy")
            .and_then(|v| v.as_str())
            == Some("mention")
        {
            warn!(
                "whatsapp-web channel '{name}': group_policy 'mention' is not supported \
                 (WhatsApp has no standard mention format) - acting as 'disabled'"
            );
        }

        let group_filter: WhatsAppWebGroupFilter = {
            let policy = Arc::clone(&policy);
            Arc::new(move |is_mentioned| policy.should_process_group(is_mentioned))
        };

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());

        let inject_user_name_waweb = channel_config
            .settings
            .get("inject_user_name")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let on_message: WhatsAppOnMessageFn = Arc::new(
            move |from_jid: String,
                  user_name: String,
                  text: String,
                  is_group: bool,
                  _file: Option<opencrust_channels::whatsapp::WhatsAppFile>,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                Box::pin(async move {
                    // Groups already filtered by channel handler - skip auth for groups
                    if !is_group {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy,
                            &mut list,
                            &pairing,
                            &from_jid,
                            &user_name,
                            &text,
                            "whatsapp-web",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(ChannelResponse::Text(welcome)),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("whatsapp-web-{from_jid}");

                    state.check_user_rate_limit(&from_jid, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &from_jid, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );
                    if inject_user_name_waweb {
                        state.agents.set_session_user_name(&session_id, &user_name);
                    }

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }
                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                        return Err(format!(
                            "input rejected: message exceeds {max_input_chars} character limit"
                        ));
                    }

                    state
                        .hydrate_session_history(&session_id, Some("whatsapp-web"), Some(&from_jid))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&from_jid));
                    let summary = state.session_summary(&session_id);

                    let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&from_jid),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&from_jid),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    if let Some(s) = new_summary {
                        state.update_session_summary(&session_id, &s);
                    }

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    state
                        .persist_turn(
                            &session_id,
                            Some("whatsapp-web"),
                            Some(&from_jid),
                            &text,
                            &response,
                            Some(serde_json::json!({"whatsapp_from": from_jid})),
                        )
                        .await;

                    if let Some((input, output, provider, model)) =
                        state.agents.take_session_usage(&session_id)
                    {
                        state
                            .persist_usage(&session_id, &provider, &model, input, output)
                            .await;
                    }

                    Ok(ChannelResponse::Text(response))
                })
            },
        );

        let channel =
            WhatsAppWebChannel::with_group_filter(on_message, group_filter).with_name(name.clone());
        channels.push(channel);
        info!("configured whatsapp-web channel: {name}");
    }

    channels
}

/// Build iMessage channels from config. macOS-only.
///
/// Must be called after state is wrapped in `Arc` so the message callback can capture a `SharedState`.
#[cfg(target_os = "macos")]
pub fn build_imessage_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<Box<dyn opencrust_channels::Channel>> {
    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "imessage" || channel_config.enabled == Some(false) {
            continue;
        }

        let poll_interval_secs: u64 = channel_config
            .settings
            .get("poll_interval_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(2);

        let allowlist = Arc::clone(&state.allowlist);

        let pairing = Arc::clone(&state.pairing);

        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        // Warn if group_policy: mention is set - iMessage has no mention concept
        if channel_config
            .settings
            .get("group_policy")
            .and_then(|v| v.as_str())
            == Some("mention")
        {
            warn!(
                "imessage channel '{name}': group_policy 'mention' is not supported \
                 (iMessage has no mention concept) - acting as 'disabled'"
            );
        }

        let group_filter: IMessageGroupFilter = {
            let policy = Arc::clone(&policy);
            Arc::new(move |is_mentioned| policy.should_process_group(is_mentioned))
        };

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());

        let on_message: IMessageOnMessageFn = Arc::new(
            move |session_key: String,
                  sender_id: String,
                  text: String,
                  is_group: bool,
                  _delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                Box::pin(async move {
                    // Groups already filtered by channel handler - skip auth for groups
                    if !is_group {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &sender_id, "", &text, "imessage",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(ChannelResponse::Text(welcome)),
                            Err(e) => return Err(e),
                        }
                    }

                    // session_key is group_name for groups, sender handle for DMs
                    let session_id = format!("imessage-{session_key}");

                    state.check_user_rate_limit(&sender_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &sender_id, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }
                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                        return Err(format!(
                            "input rejected: message exceeds {max_input_chars} character limit"
                        ));
                    }

                    state
                        .hydrate_session_history(&session_id, Some("imessage"), Some(&sender_id))
                        .await;
                    let history: Vec<opencrust_agents::ChatMessage> =
                        state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&sender_id));
                    let summary = state.session_summary(&session_id);

                    let (response, new_summary) = state
                        .agents
                        .process_message_with_context_and_summary(
                            &session_id,
                            &text,
                            &history,
                            summary.as_deref(),
                            continuity_key.as_deref(),
                            Some(&sender_id),
                        )
                        .await
                        .map_err(|e| e.to_string())?;

                    if let Some(s) = new_summary {
                        state.update_session_summary(&session_id, &s);
                    }

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    state
                        .persist_turn(
                            &session_id,
                            Some("imessage"),
                            Some(&sender_id),
                            &text,
                            &response,
                            Some(serde_json::json!({"imessage_sender": sender_id})),
                        )
                        .await;

                    if let Some((input, output, provider, model)) =
                        state.agents.take_session_usage(&session_id)
                    {
                        state
                            .persist_usage(&session_id, &provider, &model, input, output)
                            .await;
                    }

                    Ok(ChannelResponse::Text(response))
                })
            },
        );

        let channel =
            IMessageChannel::with_group_filter(poll_interval_secs, on_message, group_filter)
                .with_name(name.clone());
        channels.push(Box::new(channel) as Box<dyn opencrust_channels::Channel>);
        info!("configured imessage channel: {name}");
    }

    channels
}

/// Build LINE channels from config. Must be called after state is
/// wrapped in `Arc` so the message callback can capture a `SharedState`.
pub fn build_line_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<opencrust_channels::line::LineChannel> {
    use opencrust_channels::line::{LineChannel, LineFile, LineGroupFilter, LineOnMessageFn};

    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "line" || channel_config.enabled == Some(false) {
            continue;
        }

        let channel_access_token = channel_config
            .settings
            .get("channel_access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                resolve_api_key(
                    None,
                    "LINE_CHANNEL_ACCESS_TOKEN",
                    "LINE_CHANNEL_ACCESS_TOKEN",
                )
            });

        let Some(channel_access_token) = channel_access_token else {
            warn!("line channel '{name}' has no channel_access_token, skipping");
            continue;
        };

        let channel_secret = channel_config
            .settings
            .get("channel_secret")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| resolve_api_key(None, "LINE_CHANNEL_SECRET", "LINE_CHANNEL_SECRET"));

        let Some(channel_secret) = channel_secret else {
            warn!("line channel '{name}' has no channel_secret, skipping");
            continue;
        };

        let group_policy = channel_config
            .settings
            .get("group_policy")
            .and_then(|v| v.as_str())
            .unwrap_or("open");

        let group_filter: LineGroupFilter = match group_policy {
            "disabled" => Arc::new(|_| false),
            "mention" => Arc::new(|is_mentioned| is_mentioned),
            _ => Arc::new(|_| true), // "open" — process all group messages
        };

        let allowlist = Arc::clone(&state.allowlist);
        let pairing = Arc::clone(&state.pairing);
        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());
        let auto_reply_voice_line = config.voice.auto_reply_voice;
        let tts_provider_line = state.tts_provider.clone();
        let tts_max_chars_line = config
            .voice
            .tts_max_chars
            .unwrap_or(opencrust_media::TTS_DEFAULT_MAX_CHARS);
        let data_dir_line = config
            .data_dir
            .clone()
            .unwrap_or_else(|| opencrust_config::ConfigLoader::default_config_dir().join("data"));

        // Clone before on_message moves these into its capture.
        let rag_rate_limit_pre = Arc::clone(&rate_limit_config);
        let rag_guardrails_pre = Arc::clone(&guardrails_config);

        let on_message: LineOnMessageFn = Arc::new(
            move |user_id: String,
                  context_id: String,
                  text: String,
                  is_group: bool,
                  file: Option<LineFile>,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                let tts = tts_provider_line.clone();
                let tts_max_chars = tts_max_chars_line;
                let data_dir = data_dir_line.clone();
                Box::pin(async move {
                    if !is_group {
                        // Owner-only commands handled before auth so the owner can
                        // use /pair before their user ID is in the allowlist.
                        let cmd = text.trim();
                        if cmd == "/pair" || cmd == "/users" {
                            let list = allowlist.lock().unwrap();
                            let is_owner = list.is_owner(&user_id);
                            drop(list);
                            if !is_owner {
                                return Ok(ChannelResponse::Text(
                                    "Only the bot owner can use this command.".to_string(),
                                ));
                            }
                            if cmd == "/pair" {
                                let code = pairing.lock().unwrap().generate("line");
                                return Ok(ChannelResponse::Text(format!(
                                    "Pairing code: {code}\n\nShare this with the person you want to invite. Valid for 5 minutes."
                                )));
                            }
                            // /users
                            let list = allowlist.lock().unwrap();
                            let owner = list.owner().unwrap_or("none").to_string();
                            let users = list.list_users();
                            return Ok(ChannelResponse::Text(format!(
                                "Owner: {owner}\nAllowed users ({}):\n{}",
                                users.len(),
                                users.join("\n")
                            )));
                        }

                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_id, &text, "line",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(ChannelResponse::Text(welcome)),
                            Err(e) => return Err(e),
                        }
                    }

                    // Groups share a session per group/room; DMs are per user.
                    let session_id = if is_group {
                        format!("line-{context_id}")
                    } else {
                        format!("line-{user_id}")
                    };

                    // Post-auth commands available to all allowed users (DM only).
                    if !is_group {
                        let cmd = text.trim();
                        if cmd == "/help" {
                            let list = allowlist.lock().unwrap();
                            let is_owner = list.is_owner(&user_id);
                            drop(list);
                            let mut help = "OpenCrust Commands:\n\
                                /help - show this help\n\
                                /clear - reset conversation history\n\
                                !ingest - store a sent document for future reference"
                                .to_string();
                            if is_owner {
                                help.push_str(
                                    "\n/pair - generate a 6-digit invite code\n/users - list allowed users",
                                );
                            }
                            return Ok(ChannelResponse::Text(help));
                        }
                        if cmd == "/clear" {
                            if let Some(mut session) = state.sessions.get_mut(&session_id) {
                                session.history.clear();
                            }
                            state.update_session_summary(&session_id, "");
                            if let Some(store) = &state.session_store {
                                let _ = store.prune_old_messages(&session_id, 0);
                            }
                            return Ok(ChannelResponse::Text(
                                "Conversation history cleared.".to_string(),
                            ));
                        }
                    }

                    // Group /clear and !clear — strip @mention prefix then check command.
                    if is_group {
                        let raw = text.trim();
                        let cmd = raw
                            .strip_prefix(|c: char| c == '@')
                            .and_then(|s| s.split_once(char::is_whitespace))
                            .map(|(_, rest)| rest.trim())
                            .unwrap_or(raw);
                        if cmd == "/clear" || cmd == "!clear" {
                            if !allowlist.lock().unwrap().is_allowed(&user_id) {
                                return Err("__blocked__".to_string());
                            }
                            if let Some(mut session) = state.sessions.get_mut(&session_id) {
                                session.history.clear();
                            }
                            state.update_session_summary(&session_id, "");
                            if let Some(store) = &state.session_store {
                                let _ = store.prune_old_messages(&session_id, 0);
                            }
                            return Ok(ChannelResponse::Text(
                                "Conversation history cleared.".to_string(),
                            ));
                        }
                    }

                    // /ingest — run pending file through the ingestion pipeline.
                    if matches!(text.trim(), "/ingest" | "!ingest")
                        || text.trim().starts_with("/ingest ")
                        || text.trim().starts_with("!ingest ")
                    {
                        if let Some(pending) = state.take_pending_file(&session_id) {
                            return crate::ingest::run_ingest(
                                &state,
                                &data_dir,
                                &text,
                                &pending.filename,
                                &pending.data,
                            )
                            .await;
                        } else {
                            return Ok(ChannelResponse::Text(
                                "No file pending. Send a document first, then !ingest.".to_string(),
                            ));
                        }
                    }

                    // File received — store as pending and prompt the user.
                    if let Some(line_file) = file {
                        let fname = line_file.filename.clone();
                        state.set_pending_file(
                            &session_id,
                            crate::state::PendingFile {
                                filename: line_file.filename,
                                data: line_file.data,
                                received_at: std::time::Instant::now(),
                            },
                        );
                        return Ok(ChannelResponse::Text(format!(
                            "File received: {fname}. Send !ingest to add it to memory, or !ingest replace to overwrite an existing version."
                        )));
                    }

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }
                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                        return Err(format!(
                            "input rejected: message exceeds {max_input_chars} character limit"
                        ));
                    }

                    state
                        .hydrate_session_history(&session_id, Some("line"), Some(&user_id))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&user_id));
                    let summary = state.session_summary(&session_id);

                    let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    if let Some(s) = new_summary {
                        state.update_session_summary(&session_id, &s);
                    }

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    state
                        .persist_turn(
                            &session_id,
                            Some("line"),
                            Some(&user_id),
                            &text,
                            &response,
                            Some(serde_json::json!({"line_user_id": user_id})),
                        )
                        .await;

                    if let Some((input, output, provider, model)) =
                        state.agents.take_session_usage(&session_id)
                    {
                        state
                            .persist_usage(&session_id, &provider, &model, input, output)
                            .await;
                    }

                    // TTS: synthesize voice response if configured.
                    // LINE requires a CDN URL for audio; the channel handler falls back to text.
                    if auto_reply_voice_line && let Some(ref provider) = tts {
                        let tts_input = opencrust_media::truncate_for_tts(&response, tts_max_chars);
                        match provider.synthesize(tts_input).await {
                            Ok(audio) => {
                                return Ok(ChannelResponse::Voice {
                                    text: response,
                                    audio,
                                });
                            }
                            Err(e) => {
                                warn!("tts synthesis failed, falling back to text: {e}");
                            }
                        }
                    }
                    Ok(ChannelResponse::Text(response))
                })
            },
        );

        // --- Group RAG setup ---
        // If group_rag_enabled=true and setup succeeds, build a RAG-augmented channel and continue.
        // On any failure, fall through to build a plain channel without RAG.
        let group_rag_enabled = channel_config
            .settings
            .get("group_rag_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if group_rag_enabled {
            let embed_provider_name = channel_config
                .settings
                .get("embedding_provider")
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let embed_config = config.embeddings.get(embed_provider_name);

            let embed_provider: Option<Arc<dyn opencrust_agents::EmbeddingProvider>> =
                match embed_config.map(|c| c.provider.as_str()) {
                    Some("cohere") => {
                        let api_key = resolve_api_key(
                            embed_config.and_then(|c| c.api_key.as_deref()),
                            "COHERE_API_KEY",
                            "COHERE_API_KEY",
                        );
                        api_key.map(|key| {
                            Arc::new(CohereEmbeddingProvider::new(
                                key,
                                embed_config.and_then(|c| c.model.clone()),
                                embed_config.and_then(|c| c.base_url.clone()),
                            ))
                                as Arc<dyn opencrust_agents::EmbeddingProvider>
                        })
                    }
                    _ => {
                        warn!(
                            "line channel '{name}': group_rag_enabled=true but no valid embedding_provider configured, skipping RAG"
                        );
                        None
                    }
                };

            if let Some(provider) = embed_provider {
                let rag_top_k = channel_config
                    .settings
                    .get("rag_top_k")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5) as usize;

                let data_dir = config.data_dir.clone().unwrap_or_else(|| {
                    opencrust_config::ConfigLoader::default_config_dir().join("data")
                });
                let rag_db_path = data_dir.join("group_rag.db");

                match VectorStore::open(&rag_db_path) {
                    Ok(store) => {
                        let store = Arc::new(store);
                        info!("line channel '{name}': group RAG enabled (top_k={rag_top_k})");

                        let observe_store = Arc::clone(&store);
                        let observe_provider = Arc::clone(&provider);
                        let observe_fn: opencrust_channels::line::GroupObserveFn =
                            Arc::new(move |group_id: String, user_id: String, text: String| {
                                let store = Arc::clone(&observe_store);
                                let provider = Arc::clone(&observe_provider);
                                Box::pin(async move {
                                    match provider
                                        .embed_documents(std::slice::from_ref(&text))
                                        .await
                                    {
                                        Ok(mut embeddings) => {
                                            if let Some(embedding) = embeddings.pop() {
                                                let dims = embedding.len();
                                                if let Err(e) = store.insert_group_message(
                                                    "line", &group_id, &user_id, &text, &embedding,
                                                    dims,
                                                ) {
                                                    warn!("group RAG: insert failed: {e}");
                                                }
                                            }
                                        }
                                        Err(e) => warn!("group RAG: embed failed: {e}"),
                                    }
                                })
                            });

                        // Wrap on_message to handle RAG commands and prepend retrieved context.
                        let rag_store = Arc::clone(&store);
                        let rag_provider = Arc::clone(&provider);
                        let rag_allowlist = Arc::clone(&allowlist);
                        let inner_on_message = Arc::clone(&on_message);
                        let rag_state = Arc::clone(state);
                        let rag_agents = Arc::clone(&state.agents);
                        let rag_rate_limit = rag_rate_limit_pre;
                        let rag_guardrails = rag_guardrails_pre;
                        let rag_max_input_chars = max_input_chars;
                        let rag_max_output_chars = max_output_chars;
                        // Lazy cache: user_id → display_name, populated on first query per user.
                        let name_cache: Arc<Mutex<HashMap<String, String>>> =
                            Arc::new(Mutex::new(HashMap::new()));
                        let rag_client = reqwest::Client::new();
                        let rag_token = channel_access_token.clone();
                        let rag_on_message: LineOnMessageFn = Arc::new(
                            move |user_id: String,
                                  context_id: String,
                                  text: String,
                                  is_group: bool,
                                  file: Option<LineFile>,
                                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                                let store = Arc::clone(&rag_store);
                                let provider = Arc::clone(&rag_provider);
                                let allowlist = Arc::clone(&rag_allowlist);
                                let inner = Arc::clone(&inner_on_message);
                                let top_k = rag_top_k;
                                let name_cache = Arc::clone(&name_cache);
                                let rag_client = rag_client.clone();
                                let rag_token = rag_token.clone();
                                let state = Arc::clone(&rag_state);
                                let agents = Arc::clone(&rag_agents);
                                let rate_limit_config = Arc::clone(&rag_rate_limit);
                                let guardrails_config = Arc::clone(&rag_guardrails);
                                let max_input_chars = rag_max_input_chars;
                                let max_output_chars = rag_max_output_chars;
                                Box::pin(async move {
                                    // RAG group commands (mention required, handled before agent).
                                    // Strip leading @mention token so "@bot !cmd" matches "!cmd".
                                    if is_group {
                                        let stripped = text.trim();
                                        let cmd = stripped
                                            .strip_prefix(|c: char| c == '@')
                                            .and_then(|s| s.split_once(char::is_whitespace))
                                            .map(|(_, rest)| rest.trim())
                                            .unwrap_or(stripped);
                                        if cmd == "!context-stats" {
                                            let count = store
                                                .count_group_messages("line", &context_id)
                                                .unwrap_or(0);
                                            return Ok(ChannelResponse::Text(format!(
                                                "Group context: {count} messages stored"
                                            )));
                                        }
                                        if cmd == "!clear-context" {
                                            let is_allowed =
                                                allowlist.lock().unwrap().is_allowed(&user_id);
                                            if !is_allowed {
                                                return Ok(ChannelResponse::Text(
                                                    "Only authorized users can clear group context."
                                                        .to_string(),
                                                ));
                                            }
                                            let deleted = store
                                                .clear_group_messages("line", &context_id)
                                                .unwrap_or(0);
                                            return Ok(ChannelResponse::Text(format!(
                                                "Group context cleared. ({deleted} messages removed)"
                                            )));
                                        }
                                        // Route /clear and !clear through inner with stripped text.
                                        if cmd == "/clear" || cmd == "!clear" {
                                            return inner(
                                                user_id,
                                                context_id,
                                                cmd.to_string(),
                                                is_group,
                                                file,
                                                delta_tx,
                                            )
                                            .await;
                                        }
                                        // Route !ingest through inner with stripped text so the
                                        // pending file lookup uses the correct session key.
                                        if cmd == "!ingest"
                                            || cmd.starts_with("!ingest ")
                                            || cmd == "/ingest"
                                            || cmd.starts_with("/ingest ")
                                        {
                                            return inner(
                                                user_id,
                                                context_id,
                                                cmd.to_string(),
                                                is_group,
                                                file,
                                                delta_tx,
                                            )
                                            .await;
                                        }
                                    }

                                    // When the group RAG search finds relevant messages, route
                                    // through a dedicated tool-free synthesis call so the LLM
                                    // cannot invoke FileRead/Bash to "verify" paths that already
                                    // exist verbatim in the retrieved context.
                                    // Only falls through to the full agent pipeline when no hits.
                                    if is_group
                                        && file.is_none()
                                        && let Ok(query_embedding) =
                                            provider.embed_query(&text).await
                                    {
                                            let dims = query_embedding.len();
                                            if let Ok(hits) = store.search_group_messages(
                                                "line",
                                                &context_id,
                                                &query_embedding,
                                                dims,
                                                top_k,
                                            ) && !hits.is_empty() {
                                                    let mut lines =
                                                        Vec::with_capacity(hits.len());
                                                    for (uid, msg) in &hits {
                                                        let display = {
                                                            let cached = name_cache
                                                                .lock()
                                                                .unwrap()
                                                                .get(uid)
                                                                .cloned();
                                                            if let Some(n) = cached {
                                                                n
                                                            } else {
                                                                match opencrust_channels::line::api::get_group_member_display_name(
                                                                    &rag_client,
                                                                    &rag_token,
                                                                    &context_id,
                                                                    uid,
                                                                    opencrust_channels::line::api::LINE_API_BASE,
                                                                )
                                                                .await
                                                                {
                                                                    Ok(n) => {
                                                                        name_cache
                                                                            .lock()
                                                                            .unwrap()
                                                                            .insert(
                                                                                uid.clone(),
                                                                                n.clone(),
                                                                            );
                                                                        n
                                                                    }
                                                                    Err(_) => uid.clone(),
                                                                }
                                                            }
                                                        };
                                                        lines.push(format!("{display}: {msg}"));
                                                    }
                                                    let context_block = lines.join("\n");

                                                    // Security checks: run once on the synthesis
                                                    // path. The fallback inner() path runs its own
                                                    // checks independently, so there is no
                                                    // double-counting.
                                                    state
                                                        .check_user_rate_limit(
                                                            &user_id,
                                                            &rate_limit_config,
                                                        )
                                                        ?;
                                                    let session_id = format!("line-{context_id}");
                                                    state
                                                        .check_token_budget(
                                                            &session_id,
                                                            &user_id,
                                                            &guardrails_config,
                                                        )
                                                        .await?;
                                                    let text =
                                                        opencrust_security::InputValidator::sanitize(&text);
                                                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                                                        return Err("input rejected: potential prompt injection detected".to_string());
                                                    }
                                                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                                                        return Err(format!(
                                                            "input rejected: message exceeds {max_input_chars} character limit"
                                                        ));
                                                    }

                                                    // Tool-free synthesis: the LLM answers from
                                                    // context only, with no tools registered.
                                                    state
                                                        .hydrate_session_history(
                                                            &session_id,
                                                            Some("line"),
                                                            Some(&user_id),
                                                        )
                                                        .await;
                                                    let history = state.session_history(&session_id);
                                                    match agents
                                                        .synthesize_from_context(
                                                            &session_id,
                                                            &context_block,
                                                            &text,
                                                            &history,
                                                        )
                                                        .await
                                                    {
                                                        Ok(response) => {
                                                            let response =
                                                                opencrust_security::InputValidator::truncate_output(
                                                                    &response,
                                                                    max_output_chars,
                                                                );
                                                            state
                                                                .persist_turn(
                                                                    &session_id,
                                                                    Some("line"),
                                                                    Some(&user_id),
                                                                    &text,
                                                                    &response,
                                                                    Some(serde_json::json!({"line_user_id": user_id})),
                                                                )
                                                                .await;
                                                            if let Some((input, output, provider_id, model)) =
                                                                agents.take_session_usage(&session_id)
                                                            {
                                                                state
                                                                    .persist_usage(
                                                                        &session_id,
                                                                        &provider_id,
                                                                        &model,
                                                                        input,
                                                                        output,
                                                                    )
                                                                    .await;
                                                            }
                                                            return Ok(ChannelResponse::Text(
                                                                response,
                                                            ));
                                                        }
                                                        Err(e) => {
                                                            warn!(
                                                                "group RAG synthesis failed, falling back to agent: {e}"
                                                            );
                                                        }
                                                    }
                                                }
                                    }

                                    // No RAG hits (or synthesis failed): full agent pipeline.
                                    inner(user_id, context_id, text, is_group, file, delta_tx).await
                                })
                            },
                        );

                        let channel = LineChannel::with_group_filter(
                            channel_access_token,
                            channel_secret,
                            rag_on_message,
                            group_filter,
                        )
                        .with_group_observe(observe_fn)
                        .with_name(name.clone());
                        channels.push(channel);
                        info!("configured line channel: {name}");
                        continue;
                    }
                    Err(e) => {
                        warn!(
                            "line channel '{name}': failed to open group RAG store: {e}, disabling RAG"
                        );
                    }
                }
            }
        }

        let channel = LineChannel::with_group_filter(
            channel_access_token,
            channel_secret,
            on_message,
            group_filter,
        )
        .with_name(name.clone());
        channels.push(channel);
        info!("configured line channel: {name}");
    }

    channels
}

pub fn build_wechat_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<Arc<opencrust_channels::wechat::WeChatChannel>> {
    use opencrust_channels::wechat::{
        WeChatChannel, WeChatFile, WeChatGroupFilter, WeChatOnMessageFn,
    };

    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "wechat" || channel_config.enabled == Some(false) {
            continue;
        }

        let appid = channel_config
            .settings
            .get("appid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| resolve_api_key(None, "WECHAT_APPID", "WECHAT_APPID"));

        let Some(appid) = appid else {
            warn!("wechat channel '{name}' has no appid, skipping");
            continue;
        };

        let secret = channel_config
            .settings
            .get("secret")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| resolve_api_key(None, "WECHAT_SECRET", "WECHAT_SECRET"));

        let Some(secret) = secret else {
            warn!("wechat channel '{name}' has no secret, skipping");
            continue;
        };

        let token = channel_config
            .settings
            .get("token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| resolve_api_key(None, "WECHAT_TOKEN", "WECHAT_TOKEN"));

        let Some(token) = token else {
            warn!("wechat channel '{name}' has no webhook token, skipping");
            continue;
        };

        let group_policy = channel_config
            .settings
            .get("group_policy")
            .and_then(|v| v.as_str())
            .unwrap_or("open");

        let group_filter: WeChatGroupFilter = match group_policy {
            "disabled" => Arc::new(|_| false),
            _ => Arc::new(|_| true),
        };

        let allowlist = Arc::clone(&state.allowlist);
        let pairing = Arc::clone(&state.pairing);
        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());
        let auto_reply_voice_wechat = config.voice.auto_reply_voice;
        let tts_provider_wechat = state.tts_provider.clone();
        let tts_max_chars_wechat = config
            .voice
            .tts_max_chars
            .unwrap_or(opencrust_media::TTS_DEFAULT_MAX_CHARS);
        let data_dir_wechat = config
            .data_dir
            .clone()
            .unwrap_or_else(|| opencrust_config::ConfigLoader::default_config_dir().join("data"));

        let on_message: WeChatOnMessageFn = Arc::new(
            move |user_id: String,
                  context_id: String,
                  text: String,
                  _is_group: bool,
                  file: Option<WeChatFile>,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                let tts = tts_provider_wechat.clone();
                let tts_max_chars = tts_max_chars_wechat;
                let data_dir = data_dir_wechat.clone();
                Box::pin(async move {
                    {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_id, &text, "wechat",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(ChannelResponse::Text(welcome)),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("wechat-{context_id}");

                    // /ingest — run pending file through the ingestion pipeline.
                    if matches!(text.trim(), "/ingest" | "!ingest")
                        || text.trim().starts_with("/ingest ")
                        || text.trim().starts_with("!ingest ")
                    {
                        if let Some(pending) = state.take_pending_file(&session_id) {
                            return crate::ingest::run_ingest(
                                &state,
                                &data_dir,
                                &text,
                                &pending.filename,
                                &pending.data,
                            )
                            .await;
                        } else {
                            return Ok(ChannelResponse::Text(
                                "No file pending. Send an image first, then !ingest.".to_string(),
                            ));
                        }
                    }

                    // Image received — store as pending and prompt the user.
                    if let Some(wechat_file) = file {
                        let fname = wechat_file.filename.clone();
                        state.set_pending_file(
                            &session_id,
                            crate::state::PendingFile {
                                filename: wechat_file.filename,
                                data: wechat_file.data,
                                received_at: std::time::Instant::now(),
                            },
                        );
                        return Ok(ChannelResponse::Text(format!(
                            "Image received: {fname}. Send !ingest to add it to memory, or !ingest replace to overwrite an existing version."
                        )));
                    }

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }
                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                        return Err(format!(
                            "input rejected: message exceeds {max_input_chars} character limit"
                        ));
                    }

                    state
                        .hydrate_session_history(&session_id, Some("wechat"), Some(&user_id))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&user_id));
                    let summary = state.session_summary(&session_id);

                    let (response, new_summary) = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context_and_summary(
                                &session_id,
                                &text,
                                &history,
                                summary.as_deref(),
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    if let Some(s) = new_summary {
                        state.update_session_summary(&session_id, &s);
                    }

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    state
                        .persist_turn(
                            &session_id,
                            Some("wechat"),
                            Some(&user_id),
                            &text,
                            &response,
                            Some(serde_json::json!({"wechat_openid": user_id})),
                        )
                        .await;

                    if let Some((input, output, provider, model)) =
                        state.agents.take_session_usage(&session_id)
                    {
                        state
                            .persist_usage(&session_id, &provider, &model, input, output)
                            .await;
                    }

                    // TTS: synthesize voice response if configured.
                    // The WeChat channel handler uploads audio and sends a voice message.
                    if auto_reply_voice_wechat && let Some(ref provider) = tts {
                        let tts_input = opencrust_media::truncate_for_tts(&response, tts_max_chars);
                        match provider.synthesize(tts_input).await {
                            Ok(audio) => {
                                return Ok(ChannelResponse::Voice {
                                    text: response,
                                    audio,
                                });
                            }
                            Err(e) => {
                                warn!("tts synthesis failed, falling back to text: {e}");
                            }
                        }
                    }
                    Ok(ChannelResponse::Text(response))
                })
            },
        );

        let channel =
            WeChatChannel::with_group_filter(appid, secret, token, on_message, group_filter)
                .with_name(name.clone());
        channels.push(Arc::new(channel));
        info!("configured wechat channel: {name}");
    }

    channels
}

/// Build MQTT channels from config.  Must be called after `SharedState` is
/// available so the message callback can capture it.
pub fn build_mqtt_channels(config: &AppConfig, state: &SharedState) -> Vec<MqttChannel> {
    use opencrust_channels::mqtt::config::MqttConfig;

    let mut channels = Vec::new();

    for (name, channel_config) in &config.channels {
        if channel_config.channel_type != "mqtt" || channel_config.enabled == Some(false) {
            continue;
        }

        let mqtt_config = match MqttConfig::from_settings(name, &channel_config.settings) {
            Ok(c) => c,
            Err(e) => {
                warn!("mqtt channel '{name}' config error: {e}, skipping");
                continue;
            }
        };

        let state_for_cb = Arc::clone(state);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());
        let channel_name = name.clone();
        let _publish_topic = mqtt_config.publish_topic.clone();

        let on_message: MqttOnMessageFn = Arc::new(
            move |user_id: String,
                  session_id: String,
                  text: String,
                  _delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                let channel = channel_name.clone();
                Box::pin(async move {
                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;
                    state.agents.set_session_tool_config(
                        &session_id,
                        guardrails_config.allowed_tools.clone(),
                        guardrails_config.session_tool_call_budget,
                    );

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }
                    if opencrust_security::InputValidator::exceeds_length(&text, max_input_chars) {
                        return Err(format!(
                            "input rejected: message exceeds {max_input_chars} character limit"
                        ));
                    }

                    state
                        .hydrate_session_history(&session_id, Some("mqtt"), Some(&user_id))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&user_id));
                    let summary = state.session_summary(&session_id);

                    let (response, new_summary) = state
                        .agents
                        .process_message_with_context_and_summary(
                            &session_id,
                            &text,
                            &history,
                            summary.as_deref(),
                            continuity_key.as_deref(),
                            Some(&user_id),
                        )
                        .await
                        .map_err(|e| e.to_string())?;

                    if let Some(s) = new_summary {
                        state.update_session_summary(&session_id, &s);
                    }

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );

                    state
                        .persist_turn(
                            &session_id,
                            Some("mqtt"),
                            Some(&user_id),
                            &text,
                            &response,
                            Some(serde_json::json!({
                                "mqtt_channel": channel,
                                "mqtt_user_id": user_id,
                            })),
                        )
                        .await;

                    if let Some((input_tok, output_tok, provider, model)) =
                        state.agents.take_session_usage(&session_id)
                    {
                        state
                            .persist_usage(&session_id, &provider, &model, input_tok, output_tok)
                            .await;
                    }

                    Ok(ChannelResponse::Text(response))
                })
            },
        );

        channels.push(MqttChannel::new(mqtt_config, on_message));
        info!("configured mqtt channel: {name}");
    }

    channels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn build_agent_runtime_empty_config_no_crash() {
        let config = AppConfig::default();
        let (runtime, _handle) = build_agent_runtime(&config).await;
        // Should succeed with no providers or tools crashing
        assert!(runtime.system_prompt().is_none());
    }

    #[tokio::test]
    async fn build_agent_runtime_unknown_provider_skips_gracefully() {
        let mut config = AppConfig::default();
        config.llm.insert(
            "bad".to_string(),
            opencrust_config::LlmProviderConfig {
                provider: "nonexistent-provider".to_string(),
                model: None,
                api_key: None,
                base_url: None,
                extra: std::collections::HashMap::new(),
            },
        );
        // Should not panic — unknown providers are logged and skipped
        let _r = build_agent_runtime(&config).await;
    }

    #[tokio::test]
    async fn build_agent_runtime_vllm_provider_no_api_key() {
        let mut config = AppConfig::default();
        config.llm.insert(
            "my-vllm".to_string(),
            opencrust_config::LlmProviderConfig {
                provider: "vllm".to_string(),
                model: Some("Qwen/Qwen2.5-7B-Instruct".to_string()),
                api_key: None,
                base_url: Some("http://localhost:8000".to_string()),
                extra: std::collections::HashMap::new(),
            },
        );
        // Should register the vllm provider without panicking
        let _r = build_agent_runtime(&config).await;
    }

    #[test]
    fn resolve_api_key_prefers_config_over_env() {
        // Config value should win when present
        let result = resolve_api_key(
            Some("from-config"),
            "NONEXISTENT_VAULT_KEY",
            "NONEXISTENT_ENV_VAR_12345",
        );
        assert_eq!(result, Some("from-config".to_string()));
    }

    #[test]
    fn resolve_api_key_falls_back_to_env() {
        // Set a unique env var for this test
        let var_name = "OPENCRUST_TEST_API_KEY_BOOTSTRAP_72";
        // SAFETY: this test is single-threaded and uses a unique env var name.
        unsafe { std::env::set_var(var_name, "from-env") };
        let result = resolve_api_key(None, "NONEXISTENT_VAULT_KEY", var_name);
        assert_eq!(result, Some("from-env".to_string()));
        unsafe { std::env::remove_var(var_name) };
    }

    #[test]
    fn resolve_api_key_returns_none_when_all_missing() {
        let result = resolve_api_key(None, "NONEXISTENT_VAULT_KEY", "NONEXISTENT_ENV_VAR_99999");
        assert_eq!(result, None);
    }
}
