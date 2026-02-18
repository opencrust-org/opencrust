use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use opencrust_agents::{
    AgentRuntime, AnthropicProvider, BashTool, ChatMessage, ChatRole, CohereEmbeddingProvider,
    FileReadTool, FileWriteTool, MessagePart, OllamaProvider, OpenAiProvider, WebFetchTool,
};
use opencrust_channels::{Channel, TelegramChannel};
use opencrust_config::AppConfig;
use opencrust_db::MemoryStore;
use opencrust_security::{Allowlist, PairingManager};
use tracing::{info, warn};

use crate::state::SharedState;

/// Default vault path under the user's home directory.
fn default_vault_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".opencrust").join("credentials").join("vault.json"))
}

fn default_allowlist_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".opencrust").join("allowlist.json"))
        .unwrap_or_else(|| PathBuf::from(".opencrust/allowlist.json"))
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
            .or_else(|| dirs::home_dir().map(|h| h.join(".opencrust").join("data")))
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
    let skills_dir = dirs::home_dir()
        .map(|h| h.join(".opencrust").join("skills"))
        .unwrap_or_else(|| std::path::PathBuf::from(".opencrust/skills"));
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

/// Build and connect configured channels that can be initialized before state is wrapped in Arc.
///
/// Returns the registry and an optional Discord event receiver for message handling.
pub async fn build_channels(
    config: &AppConfig,
) -> (
    opencrust_channels::ChannelRegistry,
    Option<tokio::sync::broadcast::Receiver<opencrust_channels::ChannelEvent>>,
) {
    // Load .env file if present (idempotent, will not overwrite existing env vars)
    if let Err(e) = dotenvy::dotenv() {
        tracing::debug!("no .env file loaded: {e}");
    }

    let mut registry = opencrust_channels::ChannelRegistry::new();
    let mut discord_rx = None;

    for (name, channel_config) in &config.channels {
        let enabled = channel_config.enabled.unwrap_or(true);
        if !enabled {
            info!("channel {name} is disabled, skipping");
            continue;
        }

        match channel_config.channel_type.as_str() {
            "discord" => {
                // Inject secrets from env vars into the settings map
                let mut settings = channel_config.settings.clone();

                if let Ok(token) = std::env::var("DISCORD_BOT_TOKEN") {
                    settings.insert("bot_token".to_string(), serde_json::json!(token));
                }
                if let Ok(app_id) = std::env::var("DISCORD_APP_ID")
                    && let Ok(id) = app_id.parse::<u64>()
                {
                    settings.insert("application_id".to_string(), serde_json::json!(id));
                }

                match opencrust_channels::discord::DiscordChannel::from_settings(&settings) {
                    Ok(mut channel) => {
                        info!("starting Discord channel: {name}");

                        // Subscribe to events before connecting so we do not miss messages.
                        discord_rx = Some(channel.subscribe());

                        if let Err(e) = channel.connect().await {
                            warn!("failed to connect Discord channel {name}: {e}");
                            discord_rx = None;
                        } else {
                            info!("Discord channel {name} connected successfully");
                            registry.register(Box::new(channel));
                        }
                    }
                    Err(e) => {
                        warn!("failed to configure Discord channel {name}: {e}");
                    }
                }
            }
            "telegram" => {
                // Telegram channels need SharedState for callbacks, so they are started later.
                info!("telegram channel {name} will be started after state initialization");
            }
            other => {
                warn!("unknown channel type: {other} for channel {name}, skipping");
            }
        }
    }

    (registry, discord_rx)
}

/// Spawn a background task that listens for incoming Discord messages,
/// routes them through the agent runtime, and sends replies back.
pub fn spawn_discord_listener(
    state: Arc<crate::state::AppState>,
    mut event_rx: tokio::sync::broadcast::Receiver<opencrust_channels::ChannelEvent>,
) {
    tokio::spawn(async move {
        info!("Discord message listener started");

        loop {
            match event_rx.recv().await {
                Ok(opencrust_channels::ChannelEvent::MessageReceived(msg)) => {
                    let user_text = match &msg.content {
                        opencrust_common::MessageContent::Text(text) => text.clone(),
                        opencrust_common::MessageContent::Image { caption, .. } => {
                            caption.clone().unwrap_or_default()
                        }
                        _ => continue,
                    };

                    if user_text.trim().is_empty() {
                        continue;
                    }

                    let session_id = msg.session_id.as_str().to_string();
                    info!(
                        "Discord message from {}: {}",
                        msg.user_id.as_str(),
                        &user_text[..user_text.len().min(100)]
                    );

                    let history: Vec<opencrust_agents::ChatMessage> = state
                        .sessions
                        .get(&session_id)
                        .map(|s| s.history.clone())
                        .unwrap_or_default();

                    match state
                        .agents
                        .process_message(&session_id, &user_text, &history)
                        .await
                    {
                        Ok(response_text) => {
                            if let Some(mut session) = state.sessions.get_mut(&session_id) {
                                session.history.push(opencrust_agents::ChatMessage {
                                    role: opencrust_agents::ChatRole::User,
                                    content: opencrust_agents::MessagePart::Text(user_text.clone()),
                                });
                                session.history.push(opencrust_agents::ChatMessage {
                                    role: opencrust_agents::ChatRole::Assistant,
                                    content: opencrust_agents::MessagePart::Text(
                                        response_text.clone(),
                                    ),
                                });
                            } else {
                                let _id = state.create_session();
                            }

                            let reply = opencrust_common::Message {
                                id: uuid::Uuid::new_v4().to_string(),
                                session_id: msg.session_id.clone(),
                                channel_id: msg.channel_id.clone(),
                                user_id: msg.user_id.clone(),
                                direction: opencrust_common::MessageDirection::Outgoing,
                                content: opencrust_common::MessageContent::Text(response_text),
                                timestamp: chrono::Utc::now(),
                                metadata: msg.metadata.clone(),
                            };

                            if let Some(discord) = state.channels.get("discord")
                                && let Err(e) = discord.send_message(&reply).await
                            {
                                warn!("failed to send Discord reply: {e}");
                            }
                        }
                        Err(e) => {
                            warn!("agent error for Discord message: {e}");

                            let error_reply = opencrust_common::Message {
                                id: uuid::Uuid::new_v4().to_string(),
                                session_id: msg.session_id.clone(),
                                channel_id: msg.channel_id.clone(),
                                user_id: msg.user_id.clone(),
                                direction: opencrust_common::MessageDirection::Outgoing,
                                content: opencrust_common::MessageContent::Text(
                                    "Sorry, I encountered an error processing your message."
                                        .to_string(),
                                ),
                                timestamp: chrono::Utc::now(),
                                metadata: msg.metadata.clone(),
                            };

                            if let Some(discord) = state.channels.get("discord") {
                                let _ = discord.send_message(&error_reply).await;
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Discord event listener lagged, missed {n} events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    info!("Discord event channel closed, stopping listener");
                    break;
                }
            }
        }
    });
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
                  _delta_tx: Option<tokio::sync::mpsc::Sender<String>>| {
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

                    if !state.sessions.contains_key(&session_id) {
                        state.create_session_with_id(session_id.clone());
                    }

                    if let Some(mut session) = state.sessions.get_mut(&session_id) {
                        session.last_active = std::time::Instant::now();
                        session.connected = true;
                    }

                    let history: Vec<ChatMessage> = state
                        .sessions
                        .get(&session_id)
                        .map(|s| s.history.clone())
                        .unwrap_or_default();

                    let response = state
                        .agents
                        .process_message(&session_id, &text, &history)
                        .await
                        .map_err(|e| e.to_string())?;

                    if let Some(mut session) = state.sessions.get_mut(&session_id) {
                        session.history.push(ChatMessage {
                            role: ChatRole::User,
                            content: MessagePart::Text(text),
                        });
                        session.history.push(ChatMessage {
                            role: ChatRole::Assistant,
                            content: MessagePart::Text(response.clone()),
                        });
                    }

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
