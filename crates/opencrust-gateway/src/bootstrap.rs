use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use opencrust_agents::tools::Tool;
use opencrust_agents::{
    AgentRuntime, AnthropicProvider, BashTool, ChatMessage, CohereEmbeddingProvider, FileReadTool,
    FileWriteTool, McpManager, OllamaProvider, OpenAiProvider, WebFetchTool, WebSearchTool,
};
#[cfg(target_os = "macos")]
use opencrust_channels::{IMessageChannel, IMessageOnMessageFn};
use opencrust_channels::{
    SlackChannel, SlackOnMessageFn, TelegramChannel, WhatsAppChannel, WhatsAppOnMessageFn,
};
use opencrust_config::AppConfig;
use opencrust_db::MemoryStore;
use opencrust_security::{Allowlist, PairingManager};
use tracing::{info, warn};

use crate::state::SharedState;

/// Default vault path under the user's home directory.
fn default_vault_path() -> Option<PathBuf> {
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
fn resolve_api_key(
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
                    runtime.register_provider(Box::new(provider));
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
                    runtime.register_provider(Box::new(provider));
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
                runtime.register_provider(Box::new(provider));
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
                    let provider = OpenAiProvider::new(key, model, base_url);
                    runtime.register_provider(Box::new(provider));
                    info!("configured sansa provider: {name}");
                } else {
                    warn!(
                        "skipping sansa provider {name}: no API key (set api_key in config or SANSA_API_KEY env var)"
                    );
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

    // Web search (Brave Search API) — only registered when an API key is available
    let brave_config_key = config.llm.get("brave").and_then(|c| c.api_key.clone());
    if let Some(key) = resolve_api_key(
        brave_config_key.as_deref(),
        "BRAVE_API_KEY",
        "BRAVE_API_KEY",
    ) {
        runtime.register_tool(Box::new(WebSearchTool::new(key)));
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

    runtime
}

/// Build MCP tools from merged config (config.yml + mcp.json).
///
/// Returns the manager and a flat list of bridged tools ready for registration.
pub async fn build_mcp_tools(config: &AppConfig) -> (McpManager, Vec<Box<dyn Tool>>) {
    let loader = match opencrust_config::ConfigLoader::new() {
        Ok(l) => l,
        Err(e) => {
            warn!("failed to create config loader for MCP: {e}");
            return (McpManager::new(), Vec::new());
        }
    };

    let mcp_configs = loader.merged_mcp_config(config);
    if mcp_configs.is_empty() {
        return (McpManager::new(), Vec::new());
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

        let connect_result = match server_config.transport.as_str() {
            "stdio" => {
                manager
                    .connect(
                        name,
                        &server_config.command,
                        &server_config.args,
                        &server_config.env,
                        timeout_secs,
                    )
                    .await
            }
            #[cfg(feature = "mcp-http")]
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

    (manager, all_tools)
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

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);

        let on_message: opencrust_channels::discord::DiscordOnMessageFn = Arc::new(
            move |channel_id: String,
                  user_id: String,
                  user_name: String,
                  text: String,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
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
                            &state,
                        );
                    }

                    {
                        let mut list = allowlist.lock().unwrap();
                        if list.needs_owner() {
                            list.claim_owner(&user_id);
                            info!("discord: auto-paired owner {} ({})", user_name, user_id);
                            return Ok(format!(
                                "Welcome, {}! You are now the owner of this OpenCrust bot.\n\n\
                                 Use /pair to generate a code for adding other users.\n\
                                 Use /help for available commands.",
                                user_name
                            ));
                        }

                        if !list.is_allowed(&user_id) {
                            let trimmed = text.trim();
                            if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_digit()) {
                                let claimed = pairing.lock().unwrap().claim(trimmed, &user_id);
                                if claimed.is_some() {
                                    list.add(&user_id);
                                    info!(
                                        "discord: paired user {} ({}) via code",
                                        user_name, user_id
                                    );
                                    return Ok(format!(
                                        "Welcome, {}! You now have access to this bot.",
                                        user_name
                                    ));
                                }
                            }

                            warn!(
                                "discord: unauthorized user {} ({}) in channel {}",
                                user_name, user_id, channel_id
                            );
                            return Err("__blocked__".to_string());
                        }
                    }

                    let session_id = format!("discord-{channel_id}");

                    let text = opencrust_security::InputValidator::sanitize(&text);
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

                    let response = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context(
                                &session_id,
                                &text,
                                &history,
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    state
                        .persist_turn(
                            &session_id,
                            Some("discord"),
                            Some(&user_id),
                            &text,
                            &response,
                        )
                        .await;

                    Ok(response)
                })
            },
        );

        match opencrust_channels::discord::DiscordChannel::from_settings_with_callback(
            &settings, on_message,
        ) {
            Ok(channel) => {
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

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);

        let on_message: opencrust_channels::OnMessageFn = Arc::new(
            move |chat_id: i64,
                  user_id: String,
                  user_name: String,
                  text: String,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                Box::pin(async move {
                    if let Some(cmd) = text.strip_prefix('/') {
                        let cmd = cmd.split_whitespace().next().unwrap_or("");
                        return handle_command(
                            cmd, &text, &user_id, &user_name, chat_id, &allowlist, &pairing, &state,
                        );
                    }

                    {
                        let mut list = allowlist.lock().unwrap();
                        if list.needs_owner() {
                            list.claim_owner(&user_id);
                            info!("telegram: auto-paired owner {} ({})", user_name, user_id);
                            return Ok(format!(
                                "Welcome, {}! You are now the owner of this OpenCrust bot.\n\n\
                                 Use /pair to generate a code for adding other users.\n\
                                 Use /help for available commands.",
                                user_name
                            ));
                        }

                        if !list.is_allowed(&user_id) {
                            let trimmed = text.trim();
                            if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_digit()) {
                                let claimed = pairing.lock().unwrap().claim(trimmed, &user_id);
                                if claimed.is_some() {
                                    list.add(&user_id);
                                    info!(
                                        "telegram: paired user {} ({}) via code",
                                        user_name, user_id
                                    );
                                    return Ok(format!(
                                        "Welcome, {}! You now have access to this bot.",
                                        user_name
                                    ));
                                }
                            }

                            warn!(
                                "telegram: unauthorized user {} ({}) in chat {}",
                                user_name, user_id, chat_id
                            );
                            return Err("__blocked__".to_string());
                        }
                    }

                    let session_id = format!("telegram-{chat_id}");

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }

                    state
                        .hydrate_session_history(&session_id, Some("telegram"), Some(&user_id))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&user_id));

                    let response = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context(
                                &session_id,
                                &text,
                                &history,
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    state
                        .persist_turn(
                            &session_id,
                            Some("telegram"),
                            Some(&user_id),
                            &text,
                            &response,
                        )
                        .await;

                    Ok(response)
                })
            },
        );

        let channel = TelegramChannel::new(bot_token, on_message);
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
    state: &SharedState,
) -> std::result::Result<String, String> {
    let list = allowlist.lock().unwrap();
    let is_owner = list.is_owner(user_id);
    let is_allowed = list.is_allowed(user_id);
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
    state: &SharedState,
) -> std::result::Result<String, String> {
    let list = allowlist.lock().unwrap();
    let is_owner = list.is_owner(user_id);
    let is_allowed = list.is_allowed(user_id);
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

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);

        let on_message: SlackOnMessageFn = Arc::new(
            move |channel_id: String,
                  user_id: String,
                  user_name: String,
                  text: String,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                Box::pin(async move {
                    // Allowlist / pairing check
                    {
                        let mut list = allowlist.lock().unwrap();
                        if list.needs_owner() {
                            list.claim_owner(&user_id);
                            info!("slack: auto-paired owner {} ({})", user_name, user_id);
                            return Ok(format!(
                                "Welcome, {}! You are now the owner of this OpenCrust bot.\n\n\
                                 Use /pair to generate a code for adding other users.\n\
                                 Use /help for available commands.",
                                user_name
                            ));
                        }

                        if !list.is_allowed(&user_id) {
                            let trimmed = text.trim();
                            if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_digit()) {
                                let claimed = pairing.lock().unwrap().claim(trimmed, &user_id);
                                if claimed.is_some() {
                                    list.add(&user_id);
                                    info!(
                                        "slack: paired user {} ({}) via code",
                                        user_name, user_id
                                    );
                                    return Ok(format!(
                                        "Welcome, {}! You now have access to this bot.",
                                        user_name
                                    ));
                                }
                            }

                            warn!(
                                "slack: unauthorized user {} ({}) in channel {}",
                                user_name, user_id, channel_id
                            );
                            return Err("__blocked__".to_string());
                        }
                    }

                    let session_id = format!("slack-{channel_id}");

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }

                    state
                        .hydrate_session_history(&session_id, Some("slack"), Some(&user_id))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&user_id));

                    let response = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context(
                                &session_id,
                                &text,
                                &history,
                                continuity_key.as_deref(),
                                Some(&user_id),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    state
                        .persist_turn(&session_id, Some("slack"), Some(&user_id), &text, &response)
                        .await;

                    Ok(response)
                })
            },
        );

        let channel = SlackChannel::new(bot_token, app_token, on_message);
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

        let access_token = channel_config
            .settings
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let access_token = access_token
            .or_else(|| resolve_api_key(None, "WHATSAPP_ACCESS_TOKEN", "WHATSAPP_ACCESS_TOKEN"));

        let Some(access_token) = access_token else {
            warn!(
                "whatsapp channel '{name}' has no access_token, skipping \
                 (set access_token in config or WHATSAPP_ACCESS_TOKEN env var)"
            );
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

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);

        let on_message: WhatsAppOnMessageFn = Arc::new(
            move |from_number: String,
                  user_name: String,
                  text: String,
                  delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                Box::pin(async move {
                    // Allowlist / pairing check
                    {
                        let mut list = allowlist.lock().unwrap();
                        if list.needs_owner() {
                            list.claim_owner(&from_number);
                            info!(
                                "whatsapp: auto-paired owner {} ({})",
                                user_name, from_number
                            );
                            return Ok(format!(
                                "Welcome, {}! You are now the owner of this OpenCrust bot.\n\n\
                                 Send /pair to generate a code for adding other users.\n\
                                 Send /help for available commands.",
                                user_name
                            ));
                        }

                        if !list.is_allowed(&from_number) {
                            let trimmed = text.trim();
                            if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_digit()) {
                                let claimed = pairing.lock().unwrap().claim(trimmed, &from_number);
                                if claimed.is_some() {
                                    list.add(&from_number);
                                    info!(
                                        "whatsapp: paired user {} ({}) via code",
                                        user_name, from_number
                                    );
                                    return Ok(format!(
                                        "Welcome, {}! You now have access to this bot.",
                                        user_name
                                    ));
                                }
                            }

                            warn!(
                                "whatsapp: unauthorized user {} ({})",
                                user_name, from_number
                            );
                            return Err("__blocked__".to_string());
                        }
                    }

                    let session_id = format!("whatsapp-{from_number}");

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }

                    state
                        .hydrate_session_history(&session_id, Some("whatsapp"), Some(&from_number))
                        .await;
                    let history: Vec<ChatMessage> = state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&from_number));

                    let response = if let Some(delta_sender) = delta_tx {
                        state
                            .agents
                            .process_message_streaming_with_context(
                                &session_id,
                                &text,
                                &history,
                                delta_sender,
                                continuity_key.as_deref(),
                                Some(&from_number),
                            )
                            .await
                    } else {
                        state
                            .agents
                            .process_message_with_context(
                                &session_id,
                                &text,
                                &history,
                                continuity_key.as_deref(),
                                Some(&from_number),
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;

                    state
                        .persist_turn(
                            &session_id,
                            Some("whatsapp"),
                            Some(&from_number),
                            &text,
                            &response,
                        )
                        .await;

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

        let state_for_cb = Arc::clone(state);
        let allowlist_for_cb = Arc::clone(&allowlist);
        let pairing_for_cb = Arc::clone(&pairing);

        let on_message: IMessageOnMessageFn = Arc::new(
            move |session_key: String,
                  sender_id: String,
                  text: String,
                  _delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
                let state = Arc::clone(&state_for_cb);
                let allowlist = Arc::clone(&allowlist_for_cb);
                let pairing = Arc::clone(&pairing_for_cb);
                Box::pin(async move {
                    // Allowlist / pairing check (always against the actual sender)
                    {
                        let mut list = allowlist.lock().unwrap();
                        if list.needs_owner() {
                            list.claim_owner(&sender_id);
                            info!("imessage: auto-paired owner {sender_id}");
                            return Ok("Welcome! You are now the owner of this OpenCrust bot.\n\n\
                                 Send /pair to generate a code for adding other users.\n\
                                 Send /help for available commands."
                                .to_string());
                        }

                        if !list.is_allowed(&sender_id) {
                            let trimmed = text.trim();
                            if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_digit()) {
                                let claimed = pairing.lock().unwrap().claim(trimmed, &sender_id);
                                if claimed.is_some() {
                                    list.add(&sender_id);
                                    info!("imessage: paired user {sender_id} via code");
                                    return Ok(
                                        "Welcome! You now have access to this bot.".to_string()
                                    );
                                }
                            }

                            warn!("imessage: unauthorized user {sender_id}");
                            return Err("__blocked__".to_string());
                        }
                    }

                    // session_key is group_name for groups, sender handle for DMs
                    let session_id = format!("imessage-{session_key}");

                    let text = opencrust_security::InputValidator::sanitize(&text);
                    if opencrust_security::InputValidator::check_prompt_injection(&text) {
                        return Err(
                            "input rejected: potential prompt injection detected".to_string()
                        );
                    }

                    state
                        .hydrate_session_history(&session_id, Some("imessage"), Some(&sender_id))
                        .await;
                    let history: Vec<opencrust_agents::ChatMessage> =
                        state.session_history(&session_id);
                    let continuity_key = state.continuity_key(Some(&sender_id));

                    let response = state
                        .agents
                        .process_message_with_context(
                            &session_id,
                            &text,
                            &history,
                            continuity_key.as_deref(),
                            Some(&sender_id),
                        )
                        .await
                        .map_err(|e| e.to_string())?;

                    state
                        .persist_turn(
                            &session_id,
                            Some("imessage"),
                            Some(&sender_id),
                            &text,
                            &response,
                        )
                        .await;

                    Ok(response)
                })
            },
        );

        let channel = IMessageChannel::new(poll_interval_secs, on_message);
        channels.push(Box::new(channel) as Box<dyn opencrust_channels::Channel>);
        info!("configured imessage channel: {name}");
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
