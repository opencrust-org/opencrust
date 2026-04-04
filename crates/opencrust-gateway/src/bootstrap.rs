use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use opencrust_agents::tools::Tool;
use opencrust_agents::{
    AgentRuntime, AnthropicProvider, BashTool, ChatMessage, CohereEmbeddingProvider, DocSearchTool,
    FileReadTool, FileWriteTool, GoogleSearchTool, McpManager, OllamaEmbeddingProvider,
    OllamaProvider, OpenAiProvider, WebFetchTool, WebSearchTool,
};
#[cfg(target_os = "macos")]
use opencrust_channels::{IMessageChannel, IMessageGroupFilter, IMessageOnMessageFn};
use opencrust_channels::{
    MediaAttachment, SlackChannel, SlackGroupFilter, SlackOnMessageFn, TelegramChannel,
    WhatsAppChannel, WhatsAppOnMessageFn, WhatsAppWebChannel, WhatsAppWebGroupFilter,
};
use opencrust_config::AppConfig;
use opencrust_db::MemoryStore;
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

fn default_allowlist_path() -> PathBuf {
    opencrust_config::ConfigLoader::default_config_dir().join("allowlist.json")
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
pub fn build_agent_runtime(config: &AppConfig) -> AgentRuntime {
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
                    );
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
                    );
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
                    OllamaProvider::new(llm_config.model.clone(), llm_config.base_url.clone());
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("sansa");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("deepseek");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("mistral");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("gemini");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("falcon");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("jais");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("qwen");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("yi");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("cohere");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("minimax");
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
                    let provider = OpenAiProvider::new(key, model, base_url).with_name("moonshot");
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
                let provider = OpenAiProvider::new(api_key, model, base_url).with_name("vllm");
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
    runtime.register_tool(Box::new(WebFetchTool::new(None)));

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

        if let Ok(doc_store) = opencrust_db::DocumentStore::open(&memory_db_path) {
            let doc_store = Arc::new(doc_store);
            // Check if there are any documents to search
            let has_docs = doc_store
                .list_documents()
                .map(|d| !d.is_empty())
                .unwrap_or(false);
            if has_docs && let Some(embed) = runtime.embedding_provider() {
                let embed_clone = embed.clone();
                let embed_fn: opencrust_agents::tools::doc_search_tool::EmbedFn =
                    Arc::new(move |text: &str| {
                        let e = embed_clone.clone();
                        let t = text.to_string();
                        Box::pin(async move { e.embed_query(&t).await })
                    });
                runtime.register_tool(Box::new(DocSearchTool::new(doc_store, embed_fn)));
                info!("doc_search tool registered (documents available for RAG)");
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
    if let Some(max_context_tokens) = config.agent.max_context_tokens {
        runtime.set_max_context_tokens(max_context_tokens);
    }
    if let Some(limit) = config.memory.recall_limit {
        runtime.set_recall_limit(limit);
    }
    if let Some(enabled) = config.memory.summarization {
        runtime.set_summarization_enabled(enabled);
    }

    // --- Skills ---
    let skills_dir = opencrust_config::ConfigLoader::default_config_dir().join("skills");
    let scanner = opencrust_skills::SkillScanner::new(&skills_dir);
    match scanner.discover() {
        Ok(skills) if !skills.is_empty() => {
            let mut skill_block = String::from("\n\n# Active Skills\n");
            for skill in &skills {
                skill_block.push_str(&format!(
                    "\n## {}\n{}\n",
                    skill.frontmatter.name, skill.frontmatter.description
                ));
                if !skill.frontmatter.triggers.is_empty() {
                    skill_block.push_str(&format!(
                        "Triggers: {}\n",
                        skill.frontmatter.triggers.join(", ")
                    ));
                }
                skill_block.push('\n');
                skill_block.push_str(&skill.body);
                skill_block.push('\n');
            }

            let new_prompt = match runtime.system_prompt() {
                Some(existing) => format!("{existing}{skill_block}"),
                None => skill_block,
            };
            runtime.set_system_prompt(new_prompt);
            info!("injected {} skill(s) into system prompt", skills.len());
        }
        Ok(_) => {} // no skills found
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

    runtime
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

        let allowlist = Arc::new(Mutex::new(Allowlist::load_or_create(
            &default_allowlist_path(),
        )));
        let pairing = Arc::new(Mutex::new(PairingManager::new(
            std::time::Duration::from_secs(300),
        )));

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

        let on_message: opencrust_channels::discord::DiscordOnMessageFn = Arc::new(
            move |channel_id: String,
                  user_id: String,
                  user_name: String,
                  text: String,
                  is_group: bool,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                Box::pin(async move {
                    if let Some(cmd) = text.strip_prefix('/') {
                        let cmd = cmd.split_whitespace().next().unwrap_or("");
                        return handle_discord_command(
                            cmd,
                            &user_id,
                            &user_name,
                            &channel_id,
                            &allowlist,
                            &pairing,
                            &policy,
                            &state,
                        );
                    }

                    // Groups already filtered by channel handler - skip auth for groups
                    if !is_group {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_name, &text, "discord",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(welcome),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("discord-{channel_id}");

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;

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

                    Ok(response)
                })
            },
        );

        match opencrust_channels::discord::config::DiscordConfig::from_settings(&settings) {
            Ok(discord_config) => {
                let channel = opencrust_channels::discord::DiscordChannel::with_group_filter(
                    discord_config,
                    on_message,
                    group_filter,
                );
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
/// Tries OpenAI first, then Groq. Returns an error with a helpful message
/// if neither API key is configured.
async fn transcribe_voice(audio_bytes: &[u8]) -> std::result::Result<String, String> {
    let openai_key = resolve_api_key(None, "OPENAI_API_KEY", "OPENAI_API_KEY");
    let groq_key = resolve_api_key(None, "GROQ_API_KEY", "GROQ_API_KEY");

    if let Some(key) = openai_key {
        return whisper_transcribe(
            audio_bytes,
            &key,
            "https://api.openai.com/v1/audio/transcriptions",
            "whisper-1",
        )
        .await;
    }

    if let Some(key) = groq_key {
        return whisper_transcribe(
            audio_bytes,
            &key,
            "https://api.groq.com/openai/v1/audio/transcriptions",
            "whisper-large-v3-turbo",
        )
        .await;
    }

    Err("Voice messages require an OpenAI or Groq API key. \
         Groq offers free Whisper transcription at groq.com"
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

    let response = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {api_key}"))
        .multipart(form)
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

        let allowlist = Arc::new(Mutex::new(Allowlist::load_or_create(
            &default_allowlist_path(),
        )));

        let pairing = Arc::new(Mutex::new(PairingManager::new(
            std::time::Duration::from_secs(300),
        )));

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
                Box::pin(async move {
                    // --- Command handling (text-only) ---
                    if let Some(cmd) = text.strip_prefix('/') {
                        let cmd = cmd.split_whitespace().next().unwrap_or("");
                        return handle_command(
                            cmd, &text, &user_id, &user_name, chat_id, &allowlist, &pairing,
                            &policy, &state,
                        );
                    }

                    // --- Auth / pairing (skip for groups - already filtered by channel handler) ---
                    if !is_group {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_name, &text, "telegram",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(welcome),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("telegram-{chat_id}");

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;

                    // --- Handle media or text ---
                    match attachment {
                        Some(MediaAttachment::Voice { data, duration }) => {
                            let transcript = transcribe_voice(&data).await?;
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
                            Ok(response)
                        }
                        Some(MediaAttachment::Photo { data, caption }) => {
                            use base64::Engine;
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                            let data_url = format!("data:image/jpeg;base64,{b64}");
                            let caption_text =
                                caption.unwrap_or_else(|| "Describe this image.".to_string());

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
                            Ok(response)
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
                            let ext = fname.rsplit('.').next().unwrap_or("").to_lowercase();
                            let text_exts = [
                                "txt", "md", "json", "csv", "log", "py", "rs", "js", "ts", "toml",
                                "yaml", "yml", "xml", "html",
                            ];

                            if !text_exts.contains(&ext.as_str()) {
                                return Err(format!(
                                    "Unsupported file type (.{ext}). Supported: \
                                     txt, md, json, csv, py, rs, js, ts, toml, yaml, yml, xml, html"
                                ));
                            }

                            let file_content = String::from_utf8(data).map_err(|_| {
                                "File does not appear to be valid UTF-8 text.".to_string()
                            })?;
                            let user_text = format!(
                                "```{fname}\n{file_content}\n```\n\n{}",
                                caption.unwrap_or_default()
                            );

                            let text = opencrust_security::InputValidator::sanitize(&user_text);
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
                            Ok(response)
                        }
                        None => {
                            // Existing text-only path
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
                            Ok(response)
                        }
                    }
                })
            },
        );

        let channel = TelegramChannel::with_group_filter(bot_token, on_message, group_filter);
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
                /clear - reset conversation history"
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
                /clear - reset conversation history"
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

        let allowlist = Arc::new(Mutex::new(Allowlist::load_or_create(
            &default_allowlist_path(),
        )));

        let pairing = Arc::new(Mutex::new(PairingManager::new(
            std::time::Duration::from_secs(300),
        )));

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

        let on_message: SlackOnMessageFn = Arc::new(
            move |channel_id: String,
                  user_id: String,
                  user_name: String,
                  text: String,
                  is_group: bool,
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
                            &policy, &mut list, &pairing, &user_id, &user_name, &text, "slack",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(welcome),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("slack-{channel_id}");

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;

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

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    Ok(response)
                })
            },
        );

        let channel = SlackChannel::with_group_filter(
            bot_token,
            app_token,
            on_message,
            group_filter,
            bot_user_id,
        );
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

        let allowlist = Arc::new(Mutex::new(Allowlist::load_or_create(
            &default_allowlist_path(),
        )));

        let pairing = Arc::new(Mutex::new(PairingManager::new(
            std::time::Duration::from_secs(300),
        )));

        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());

        let on_message: WhatsAppOnMessageFn = Arc::new(
            move |from_number: String,
                  user_name: String,
                  text: String,
                  _is_group: bool,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                Box::pin(async move {
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
                            Ok(Some(welcome)) => return Ok(welcome),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("whatsapp-{from_number}");

                    state.check_user_rate_limit(&from_number, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &from_number, &guardrails_config)
                        .await?;

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

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    Ok(response)
                })
            },
        );

        let channel = Arc::new(WhatsAppChannel::new(
            access_token,
            phone_number_id,
            verify_token,
            on_message,
        ));
        channels.push(channel);
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

        let allowlist = Arc::new(Mutex::new(Allowlist::load_or_create(
            &default_allowlist_path(),
        )));

        let pairing = Arc::new(Mutex::new(PairingManager::new(
            std::time::Duration::from_secs(300),
        )));

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

        let on_message: WhatsAppOnMessageFn = Arc::new(
            move |from_jid: String,
                  user_name: String,
                  text: String,
                  is_group: bool,
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
                            Ok(Some(welcome)) => return Ok(welcome),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("whatsapp-web-{from_jid}");

                    state.check_user_rate_limit(&from_jid, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &from_jid, &guardrails_config)
                        .await?;

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

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    Ok(response)
                })
            },
        );

        let channel = WhatsAppWebChannel::with_group_filter(on_message, group_filter);
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

        let allowlist = Arc::new(Mutex::new(Allowlist::load_or_create(
            &default_allowlist_path(),
        )));

        let pairing = Arc::new(Mutex::new(PairingManager::new(
            std::time::Duration::from_secs(300),
        )));

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
                            Ok(Some(welcome)) => return Ok(welcome),
                            Err(e) => return Err(e),
                        }
                    }

                    // session_key is group_name for groups, sender handle for DMs
                    let session_id = format!("imessage-{session_key}");

                    state.check_user_rate_limit(&sender_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &sender_id, &guardrails_config)
                        .await?;

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

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    Ok(response)
                })
            },
        );

        let channel =
            IMessageChannel::with_group_filter(poll_interval_secs, on_message, group_filter);
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
) -> Vec<Arc<opencrust_channels::line::LineChannel>> {
    use opencrust_channels::line::{LineChannel, LineGroupFilter, LineOnMessageFn};

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

        if group_policy == "mention" {
            warn!(
                "line channel '{name}': group_policy 'mention' is not supported \
                 (LINE has no mention detection API) — treating as 'disabled'"
            );
        }

        let group_filter: LineGroupFilter = match group_policy {
            "disabled" | "mention" => Arc::new(|_| false),
            _ => Arc::new(|_| true), // "open" — process all group messages
        };

        let allowlist = Arc::new(Mutex::new(Allowlist::load_or_create(
            &default_allowlist_path(),
        )));
        let pairing = Arc::new(Mutex::new(PairingManager::new(
            std::time::Duration::from_secs(300),
        )));
        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());

        let on_message: LineOnMessageFn = Arc::new(
            move |user_id: String,
                  context_id: String,
                  text: String,
                  is_group: bool,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                Box::pin(async move {
                    if !is_group {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_id, &text, "line",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(welcome),
                            Err(e) => return Err(e),
                        }
                    }

                    // Groups share a session per group/room; DMs are per user.
                    let session_id = if is_group {
                        format!("line-{context_id}")
                    } else {
                        format!("line-{user_id}")
                    };

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;

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

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    Ok(response)
                })
            },
        );

        let channel = Arc::new(LineChannel::with_group_filter(
            channel_access_token,
            channel_secret,
            on_message,
            group_filter,
        ));
        channels.push(channel);
        info!("configured line channel: {name}");
    }

    channels
}

pub fn build_wechat_channels(
    config: &AppConfig,
    state: &SharedState,
) -> Vec<Arc<opencrust_channels::wechat::WeChatChannel>> {
    use opencrust_channels::wechat::{WeChatChannel, WeChatGroupFilter, WeChatOnMessageFn};

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

        let allowlist = Arc::new(Mutex::new(Allowlist::load_or_create(
            &default_allowlist_path(),
        )));
        let pairing = Arc::new(Mutex::new(PairingManager::new(
            std::time::Duration::from_secs(300),
        )));
        let policy = Arc::new(ChannelPolicy::from_settings(&channel_config.settings));

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);
        let policy_for_cb = Arc::clone(&policy);
        let max_input_chars = config.guardrails.max_input_chars;
        let max_output_chars = config.guardrails.max_output_chars;
        let rate_limit_config = Arc::new(config.gateway.rate_limit.clone());
        let guardrails_config = Arc::new(config.guardrails.clone());

        let on_message: WeChatOnMessageFn = Arc::new(
            move |user_id: String,
                  context_id: String,
                  text: String,
                  _is_group: bool,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                let policy = Arc::clone(&policy_for_cb);
                let rate_limit_config = Arc::clone(&rate_limit_config);
                let guardrails_config = Arc::clone(&guardrails_config);
                Box::pin(async move {
                    {
                        let mut list = allowlist.lock().unwrap();
                        match check_dm_auth(
                            &policy, &mut list, &pairing, &user_id, &user_id, &text, "wechat",
                        ) {
                            Ok(None) => {}
                            Ok(Some(welcome)) => return Ok(welcome),
                            Err(e) => return Err(e),
                        }
                    }

                    let session_id = format!("wechat-{context_id}");

                    state.check_user_rate_limit(&user_id, &rate_limit_config)?;
                    state
                        .check_token_budget(&session_id, &user_id, &guardrails_config)
                        .await?;

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

                    let response = opencrust_security::InputValidator::truncate_output(
                        &response,
                        max_output_chars,
                    );
                    Ok(response)
                })
            },
        );

        let channel = Arc::new(WeChatChannel::with_group_filter(
            appid,
            secret,
            token,
            on_message,
            group_filter,
        ));
        channels.push(channel);
        info!("configured wechat channel: {name}");
    }

    channels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_agent_runtime_empty_config_no_crash() {
        let config = AppConfig::default();
        let runtime = build_agent_runtime(&config);
        // Should succeed with no providers or tools crashing
        assert!(runtime.system_prompt().is_none());
    }

    #[test]
    fn build_agent_runtime_unknown_provider_skips_gracefully() {
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
        let _runtime = build_agent_runtime(&config);
    }

    #[test]
    fn build_agent_runtime_vllm_provider_no_api_key() {
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
        let _runtime = build_agent_runtime(&config);
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
