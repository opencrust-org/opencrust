use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};
use dialoguer::{Confirm, Input, MultiSelect, Password, Select};
use opencrust_config::{AppConfig, ChannelConfig, LlmProviderConfig};
use tracing::info;

// ---------------------------------------------------------------------------
// Environment detection
// ---------------------------------------------------------------------------

/// Keys discovered in the environment.
#[derive(Default, Debug)]
struct DetectedKeys {
    anthropic_api_key: Option<String>,
    openai_api_key: Option<String>,
    sansa_api_key: Option<String>,
    telegram_bot_token: Option<String>,
    discord_bot_token: Option<String>,
    discord_app_id: Option<String>,
    slack_bot_token: Option<String>,
    slack_app_token: Option<String>,
    whatsapp_access_token: Option<String>,
}

impl DetectedKeys {
    fn has_any_llm(&self) -> bool {
        self.anthropic_api_key.is_some()
            || self.openai_api_key.is_some()
            || self.sansa_api_key.is_some()
    }

    fn has_any_channel(&self) -> bool {
        self.telegram_bot_token.is_some()
            || self.discord_bot_token.is_some()
            || self.slack_bot_token.is_some()
            || self.whatsapp_access_token.is_some()
    }

    /// Pick the best provider based on detected keys (Anthropic > OpenAI > Sansa).
    fn best_provider(&self) -> Option<&str> {
        if self.anthropic_api_key.is_some() {
            Some("anthropic")
        } else if self.openai_api_key.is_some() {
            Some("openai")
        } else if self.sansa_api_key.is_some() {
            Some("sansa")
        } else {
            None
        }
    }

    /// Return the API key for the given provider.
    fn key_for_provider(&self, provider: &str) -> Option<&str> {
        match provider {
            "anthropic" => self.anthropic_api_key.as_deref(),
            "openai" => self.openai_api_key.as_deref(),
            "sansa" => self.sansa_api_key.as_deref(),
            _ => None,
        }
    }
}

fn detect_env_keys() -> DetectedKeys {
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    DetectedKeys {
        anthropic_api_key: get("ANTHROPIC_API_KEY"),
        openai_api_key: get("OPENAI_API_KEY"),
        sansa_api_key: get("SANSA_API_KEY"),
        telegram_bot_token: get("TELEGRAM_BOT_TOKEN"),
        discord_bot_token: get("DISCORD_BOT_TOKEN"),
        discord_app_id: get("DISCORD_APP_ID"),
        slack_bot_token: get("SLACK_BOT_TOKEN"),
        slack_app_token: get("SLACK_APP_TOKEN"),
        whatsapp_access_token: get("WHATSAPP_ACCESS_TOKEN"),
    }
}

// ---------------------------------------------------------------------------
// Validation helpers (async)
// ---------------------------------------------------------------------------

/// Validate an LLM API key by making a minimal request.
async fn validate_llm_key(provider: &str, api_key: &str, base_url: Option<&str>) -> Result<bool> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let ok = match provider {
        "anthropic" => {
            let resp = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .body(r#"{"model":"claude-sonnet-4-5-20250929","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#)
                .send()
                .await?;
            // 200 = valid key, 400 is also fine (means auth passed but request was bad)
            let status = resp.status().as_u16();
            status == 200 || status == 400
        }
        "openai" | "sansa" => {
            let url = base_url.unwrap_or(match provider {
                "sansa" => "https://api.sansa.ai/v1",
                _ => "https://api.openai.com/v1",
            });
            let resp = client
                .post(format!("{url}/chat/completions"))
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(r#"{"model":"gpt-4o-mini","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#)
                .send()
                .await?;
            let status = resp.status().as_u16();
            status == 200 || status == 400
        }
        _ => {
            // Unknown provider - skip validation
            return Ok(true);
        }
    };

    Ok(ok)
}

/// Validate a Telegram bot token. Returns the bot username on success.
async fn validate_telegram_token(token: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(format!("https://api.telegram.org/bot{token}/getMe"))
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("invalid bot token (HTTP {})", resp.status());
    }

    let body: serde_json::Value = resp.json().await?;
    let username = body["result"]["username"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    Ok(username)
}

/// Validate a Discord bot token. Returns the bot username on success.
async fn validate_discord_token(token: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get("https://discord.com/api/v10/users/@me")
        .header("authorization", format!("Bot {token}"))
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("invalid bot token (HTTP {})", resp.status());
    }

    let body: serde_json::Value = resp.json().await?;
    let name = body["username"].as_str().unwrap_or("unknown").to_string();
    Ok(name)
}

/// Validate a Slack bot token. Returns the team name on success.
async fn validate_slack_token(token: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .post("https://slack.com/api/auth.test")
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await?;

    let body: serde_json::Value = resp.json().await?;
    if body["ok"].as_bool() != Some(true) {
        let err = body["error"].as_str().unwrap_or("unknown error");
        anyhow::bail!("{err}");
    }

    let team = body["team"].as_str().unwrap_or("unknown").to_string();
    Ok(team)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_existing_config(config_dir: &Path) -> Option<AppConfig> {
    let loader = opencrust_config::ConfigLoader::with_dir(config_dir);
    if loader.config_file_exists() {
        loader.load().ok()
    } else {
        None
    }
}

fn env_var_for_provider(provider: &str) -> &str {
    match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "sansa" => "SANSA_API_KEY",
        _ => "API_KEY",
    }
}

fn mask_token(s: &str) -> String {
    if s.len() <= 8 {
        "****".to_string()
    } else {
        format!("{}...{}", &s[..4], &s[s.len() - 4..])
    }
}

// ---------------------------------------------------------------------------
// Provider result from the provider section
// ---------------------------------------------------------------------------

struct ProviderResult {
    provider: String,
    api_key: String,
    from_env: bool,
    verified: bool,
}

// ---------------------------------------------------------------------------
// Section: LLM Provider
// ---------------------------------------------------------------------------

async fn section_provider(
    existing: &Option<AppConfig>,
    detected: &DetectedKeys,
) -> Result<Option<ProviderResult>> {
    println!();
    println!("  --- LLM Provider ---");

    // Show existing config
    let existing_provider = existing
        .as_ref()
        .and_then(|c| c.llm.get("main"))
        .map(|p| p.provider.clone());
    let existing_has_key = existing
        .as_ref()
        .and_then(|c| c.llm.get("main"))
        .and_then(|p| p.api_key.as_ref())
        .is_some();

    if let Some(ref prov) = existing_provider {
        let key_status = if existing_has_key {
            "API key configured"
        } else {
            "using env var"
        };
        println!("  Current: {prov} ({key_status})");

        let choices = &["Keep current", "Change"];
        let sel = Select::new()
            .with_prompt("LLM provider")
            .items(choices)
            .default(0)
            .interact()
            .context("selection cancelled")?;
        if sel == 0 {
            return Ok(None); // keep existing
        }
        println!();
    }

    // If env vars detected, offer to use them
    if detected.has_any_llm() {
        println!("  Scanning environment...");
        if detected.anthropic_api_key.is_some() {
            println!("    Found ANTHROPIC_API_KEY");
        }
        if detected.openai_api_key.is_some() {
            println!("    Found OPENAI_API_KEY");
        }
        if detected.sansa_api_key.is_some() {
            println!("    Found SANSA_API_KEY");
        }
        println!();

        let choices = &["Use detected keys (recommended)", "Enter keys manually"];
        let sel = Select::new()
            .with_prompt("API keys found in environment")
            .items(choices)
            .default(0)
            .interact()
            .context("selection cancelled")?;

        if sel == 0 {
            let provider = detected.best_provider().unwrap(); // safe - has_any_llm was true
            let key = detected.key_for_provider(provider).unwrap();

            // Validate
            print!("  Testing {provider} connection... ");
            match validate_llm_key(provider, key, None).await {
                Ok(true) => {
                    println!("connected");
                    return Ok(Some(ProviderResult {
                        provider: provider.to_string(),
                        api_key: key.to_string(),
                        from_env: true,
                        verified: true,
                    }));
                }
                Ok(false) => {
                    println!("failed (invalid API key)");
                    println!("  Falling back to manual entry.");
                    println!();
                }
                Err(e) => {
                    println!("failed ({e})");
                    println!("  Falling back to manual entry.");
                    println!();
                }
            }
        }
    }

    // Manual provider selection
    let providers = &["anthropic", "openai", "sansa"];
    let default_idx = existing_provider
        .as_deref()
        .and_then(|p| providers.iter().position(|&x| x == p))
        .unwrap_or(0);
    let selection = Select::new()
        .with_prompt("Select your LLM provider")
        .items(providers)
        .default(default_idx)
        .interact()
        .context("provider selection cancelled")?;
    let provider = providers[selection];
    let env_hint = env_var_for_provider(provider);

    // API key entry with retry loop
    loop {
        let key_prompt = if existing_has_key {
            format!("{provider} API key (Enter to keep existing, or set {env_hint} env var later)")
        } else {
            format!("{provider} API key (or set {env_hint} env var later)")
        };

        let api_key: String = Password::new()
            .with_prompt(&key_prompt)
            .allow_empty_password(true)
            .interact()
            .context("API key input cancelled")?;
        let api_key = api_key.trim().to_string();

        // If user pressed Enter with existing key, keep it
        if api_key.is_empty() && existing_has_key {
            let old_key = existing
                .as_ref()
                .and_then(|c| c.llm.get("main"))
                .and_then(|p| p.api_key.clone())
                .unwrap_or_default();
            println!("  Keeping existing API key.");
            return Ok(Some(ProviderResult {
                provider: provider.to_string(),
                api_key: old_key,
                from_env: false,
                verified: false,
            }));
        }

        // Empty key with no existing - skip (will use env var)
        if api_key.is_empty() {
            println!("  Set {env_hint} environment variable before starting the server.");
            return Ok(Some(ProviderResult {
                provider: provider.to_string(),
                api_key: String::new(),
                from_env: true,
                verified: false,
            }));
        }

        // Validate the key
        print!("  Testing {provider} connection... ");
        match validate_llm_key(provider, &api_key, None).await {
            Ok(true) => {
                println!("connected");
                return Ok(Some(ProviderResult {
                    provider: provider.to_string(),
                    api_key,
                    from_env: false,
                    verified: true,
                }));
            }
            Ok(false) => {
                println!("failed (invalid API key)");
                let retry = Confirm::new()
                    .with_prompt("Try again?")
                    .default(true)
                    .interact()
                    .unwrap_or(false);
                if !retry {
                    return Ok(Some(ProviderResult {
                        provider: provider.to_string(),
                        api_key,
                        from_env: false,
                        verified: false,
                    }));
                }
            }
            Err(e) => {
                println!("failed ({e})");
                let retry = Confirm::new()
                    .with_prompt("Try again?")
                    .default(true)
                    .interact()
                    .unwrap_or(false);
                if !retry {
                    return Ok(Some(ProviderResult {
                        provider: provider.to_string(),
                        api_key,
                        from_env: false,
                        verified: false,
                    }));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Section: System Prompt
// ---------------------------------------------------------------------------

fn section_system_prompt(existing: &Option<AppConfig>) -> Result<Option<String>> {
    println!();
    println!("  --- System Prompt ---");

    let existing_prompt = existing
        .as_ref()
        .and_then(|c| c.agent.system_prompt.clone());

    if let Some(ref prompt) = existing_prompt {
        let display = if prompt.len() > 60 {
            format!("{}...", &prompt[..57])
        } else {
            prompt.clone()
        };
        println!("  Current: {display}");

        let choices = &["Keep current", "Change"];
        let sel = Select::new()
            .with_prompt("System prompt")
            .items(choices)
            .default(0)
            .interact()
            .context("selection cancelled")?;
        if sel == 0 {
            return Ok(None);
        }
    }

    let default =
        existing_prompt.unwrap_or_else(|| "You are a helpful personal AI assistant.".to_string());
    let prompt: String = Input::new()
        .with_prompt("System prompt (optional)")
        .default(default)
        .allow_empty(true)
        .interact_text()
        .context("system prompt input cancelled")?;

    Ok(Some(prompt))
}

// ---------------------------------------------------------------------------
// Section: Channels
// ---------------------------------------------------------------------------

async fn section_channels(
    existing: &Option<AppConfig>,
    detected: &DetectedKeys,
) -> Result<Option<HashMap<String, ChannelConfig>>> {
    println!();
    println!("  --- Channels (optional) ---");

    let existing_channels = existing
        .as_ref()
        .map(|c| &c.channels)
        .cloned()
        .unwrap_or_default();

    // Show existing channel status
    if !existing_channels.is_empty() {
        for (name, ch) in &existing_channels {
            let status = if ch.enabled.unwrap_or(true) {
                "configured"
            } else {
                "disabled"
            };
            println!("  {}: {status}", name);
        }

        let choices = &["Keep current channels", "Add or change channels"];
        let sel = Select::new()
            .with_prompt("Channels")
            .items(choices)
            .default(0)
            .interact()
            .context("selection cancelled")?;
        if sel == 0 {
            return Ok(None);
        }
        println!();
    }

    // Build channel list with pre-selection for detected env vars or existing config
    let channel_names = ["Telegram", "Discord", "Slack", "WhatsApp"];
    let mut defaults = vec![false; 4];

    // Pre-check channels with detected tokens or existing config
    if detected.telegram_bot_token.is_some() || existing_channels.contains_key("telegram") {
        defaults[0] = true;
    }
    if detected.discord_bot_token.is_some() || existing_channels.contains_key("discord") {
        defaults[1] = true;
    }
    if detected.slack_bot_token.is_some() || existing_channels.contains_key("slack") {
        defaults[2] = true;
    }
    if detected.whatsapp_access_token.is_some() || existing_channels.contains_key("whatsapp") {
        defaults[3] = true;
    }

    let selections = MultiSelect::new()
        .with_prompt(
            "Which channels would you like to connect? (Space to toggle, Enter to confirm)",
        )
        .items(&channel_names)
        .defaults(&defaults)
        .interact()
        .context("channel selection cancelled")?;

    if selections.is_empty() {
        println!("  No channels selected.");
        // Return empty map to clear channels if user had some before
        return Ok(Some(HashMap::new()));
    }

    let mut channels = existing_channels;

    for &idx in &selections {
        match idx {
            0 => {
                // Telegram
                let existing_token = channels
                    .get("telegram")
                    .and_then(|c| c.settings.get("bot_token"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(cfg) = setup_telegram(
                    existing_token.as_deref(),
                    detected.telegram_bot_token.as_deref(),
                )
                .await?
                {
                    channels.insert("telegram".to_string(), cfg);
                }
            }
            1 => {
                // Discord
                let existing_token = channels
                    .get("discord")
                    .and_then(|c| c.settings.get("bot_token"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let existing_app_id = channels
                    .get("discord")
                    .and_then(|c| c.settings.get("application_id"))
                    .and_then(|v| v.as_str().or_else(|| v.as_u64().map(|_| "")))
                    .map(|_| {
                        channels
                            .get("discord")
                            .and_then(|c| c.settings.get("application_id"))
                            .map(|v| v.to_string().trim_matches('"').to_string())
                            .unwrap_or_default()
                    });
                if let Some(cfg) = setup_discord(
                    existing_token.as_deref(),
                    existing_app_id.as_deref(),
                    detected.discord_bot_token.as_deref(),
                    detected.discord_app_id.as_deref(),
                )
                .await?
                {
                    channels.insert("discord".to_string(), cfg);
                }
            }
            2 => {
                // Slack
                let existing_bot = channels
                    .get("slack")
                    .and_then(|c| c.settings.get("bot_token"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let existing_app = channels
                    .get("slack")
                    .and_then(|c| c.settings.get("app_token"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(cfg) = setup_slack(
                    existing_bot.as_deref(),
                    existing_app.as_deref(),
                    detected.slack_bot_token.as_deref(),
                    detected.slack_app_token.as_deref(),
                )
                .await?
                {
                    channels.insert("slack".to_string(), cfg);
                }
            }
            3 => {
                // WhatsApp
                let existing_token = channels
                    .get("whatsapp")
                    .and_then(|c| c.settings.get("access_token"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let existing_phone = channels
                    .get("whatsapp")
                    .and_then(|c| c.settings.get("phone_number_id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let existing_verify = channels
                    .get("whatsapp")
                    .and_then(|c| c.settings.get("verify_token"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(cfg) = setup_whatsapp(
                    existing_token.as_deref(),
                    existing_phone.as_deref(),
                    existing_verify.as_deref(),
                    detected.whatsapp_access_token.as_deref(),
                )
                .await?
                {
                    channels.insert("whatsapp".to_string(), cfg);
                }
            }
            _ => {}
        }
    }

    // Remove channels that were deselected
    let selected_names: Vec<&str> = selections
        .iter()
        .map(|&i| match i {
            0 => "telegram",
            1 => "discord",
            2 => "slack",
            3 => "whatsapp",
            _ => "",
        })
        .collect();
    let known = ["telegram", "discord", "slack", "whatsapp"];
    for name in &known {
        if !selected_names.contains(name) {
            channels.remove(*name);
        }
    }

    Ok(Some(channels))
}

// ---------------------------------------------------------------------------
// Channel setup helpers
// ---------------------------------------------------------------------------

async fn setup_telegram(
    existing_token: Option<&str>,
    env_token: Option<&str>,
) -> Result<Option<ChannelConfig>> {
    println!();
    println!("  Telegram Setup");
    println!("  1. Open Telegram and message @BotFather");
    println!("  2. Send /newbot and follow the prompts");
    println!("  3. Copy the bot token (looks like 123456:ABC-DEF...)");
    println!();

    let token =
        prompt_token_with_source("Bot token", existing_token, env_token, "TELEGRAM_BOT_TOKEN")?;

    if token.is_empty() {
        println!("  Skipping Telegram (no token provided).");
        return Ok(None);
    }

    // Validate
    print!("  Testing connection... ");
    match validate_telegram_token(&token).await {
        Ok(username) => println!("@{username} (ok)"),
        Err(e) => {
            println!("failed ({e})");
            let skip = !Confirm::new()
                .with_prompt("  Save anyway?")
                .default(true)
                .interact()
                .unwrap_or(true);
            if skip {
                return Ok(None);
            }
        }
    }

    let mut settings = HashMap::new();
    settings.insert("bot_token".to_string(), serde_json::json!(token));

    Ok(Some(ChannelConfig {
        channel_type: "telegram".to_string(),
        enabled: Some(true),
        settings,
    }))
}

async fn setup_discord(
    existing_token: Option<&str>,
    existing_app_id: Option<&str>,
    env_token: Option<&str>,
    env_app_id: Option<&str>,
) -> Result<Option<ChannelConfig>> {
    println!();
    println!("  Discord Setup");
    println!("  1. Go to https://discord.com/developers/applications");
    println!("  2. Create an application, then add a Bot");
    println!("  3. Copy the bot token and application ID");
    println!("  4. Enable MESSAGE CONTENT intent under Bot settings");
    println!();

    let token =
        prompt_token_with_source("Bot token", existing_token, env_token, "DISCORD_BOT_TOKEN")?;

    if token.is_empty() {
        println!("  Skipping Discord (no token provided).");
        return Ok(None);
    }

    // Application ID
    let app_id_source = env_app_id.or(existing_app_id);
    let app_id_prompt = if let Some(src) = app_id_source {
        format!("Application ID [{}]", mask_token(src))
    } else {
        "Application ID".to_string()
    };
    let app_id: String = Input::new()
        .with_prompt(&app_id_prompt)
        .default(app_id_source.unwrap_or("").to_string())
        .allow_empty(true)
        .interact_text()
        .context("input cancelled")?;
    let app_id = app_id.trim().to_string();

    // Validate
    print!("  Testing connection... ");
    match validate_discord_token(&token).await {
        Ok(name) => println!("{name} (ok)"),
        Err(e) => {
            println!("failed ({e})");
            let skip = !Confirm::new()
                .with_prompt("  Save anyway?")
                .default(true)
                .interact()
                .unwrap_or(true);
            if skip {
                return Ok(None);
            }
        }
    }

    let mut settings = HashMap::new();
    settings.insert("bot_token".to_string(), serde_json::json!(token));
    if !app_id.is_empty() {
        // Store as number if it parses, otherwise as string
        if let Ok(id) = app_id.parse::<u64>() {
            settings.insert("application_id".to_string(), serde_json::json!(id));
        } else {
            settings.insert("application_id".to_string(), serde_json::json!(app_id));
        }
    }

    Ok(Some(ChannelConfig {
        channel_type: "discord".to_string(),
        enabled: Some(true),
        settings,
    }))
}

async fn setup_slack(
    existing_bot: Option<&str>,
    existing_app: Option<&str>,
    env_bot: Option<&str>,
    env_app: Option<&str>,
) -> Result<Option<ChannelConfig>> {
    println!();
    println!("  Slack Setup");
    println!("  1. Go to https://api.slack.com/apps and create an app");
    println!("  2. Enable Socket Mode and get an app-level token (xapp-...)");
    println!("  3. Install to workspace and get bot token (xoxb-...)");
    println!();

    let bot_token = prompt_token_with_source(
        "Bot token (xoxb-...)",
        existing_bot,
        env_bot,
        "SLACK_BOT_TOKEN",
    )?;

    if bot_token.is_empty() {
        println!("  Skipping Slack (no token provided).");
        return Ok(None);
    }

    let app_token = prompt_token_with_source(
        "App token (xapp-...)",
        existing_app,
        env_app,
        "SLACK_APP_TOKEN",
    )?;

    // Validate bot token
    print!("  Testing connection... ");
    match validate_slack_token(&bot_token).await {
        Ok(team) => println!("{team} (ok)"),
        Err(e) => {
            println!("failed ({e})");
            let skip = !Confirm::new()
                .with_prompt("  Save anyway?")
                .default(true)
                .interact()
                .unwrap_or(true);
            if skip {
                return Ok(None);
            }
        }
    }

    let mut settings = HashMap::new();
    settings.insert("bot_token".to_string(), serde_json::json!(bot_token));
    if !app_token.is_empty() {
        settings.insert("app_token".to_string(), serde_json::json!(app_token));
    }

    Ok(Some(ChannelConfig {
        channel_type: "slack".to_string(),
        enabled: Some(true),
        settings,
    }))
}

async fn setup_whatsapp(
    existing_token: Option<&str>,
    existing_phone: Option<&str>,
    existing_verify: Option<&str>,
    env_token: Option<&str>,
) -> Result<Option<ChannelConfig>> {
    println!();
    println!("  WhatsApp Setup");
    println!("  1. Go to https://developers.facebook.com");
    println!("  2. Create a WhatsApp Business app");
    println!("  3. Get your access token and phone number ID");
    println!();

    let access_token = prompt_token_with_source(
        "Access token",
        existing_token,
        env_token,
        "WHATSAPP_ACCESS_TOKEN",
    )?;

    if access_token.is_empty() {
        println!("  Skipping WhatsApp (no token provided).");
        return Ok(None);
    }

    // Phone number ID
    let phone_default = existing_phone.unwrap_or("");
    let phone_id: String = Input::new()
        .with_prompt("Phone number ID")
        .default(phone_default.to_string())
        .allow_empty(true)
        .interact_text()
        .context("input cancelled")?;
    let phone_id = phone_id.trim().to_string();

    // Verify token
    let verify_default = existing_verify.unwrap_or("opencrust-verify");
    let verify_token: String = Input::new()
        .with_prompt("Verify token")
        .default(verify_default.to_string())
        .allow_empty(true)
        .interact_text()
        .context("input cancelled")?;
    let verify_token = verify_token.trim().to_string();

    // No simple validation endpoint for WhatsApp - just save
    println!("  WhatsApp configured (no connection test available).");

    let mut settings = HashMap::new();
    settings.insert("access_token".to_string(), serde_json::json!(access_token));
    if !phone_id.is_empty() {
        settings.insert("phone_number_id".to_string(), serde_json::json!(phone_id));
    }
    if !verify_token.is_empty() {
        settings.insert("verify_token".to_string(), serde_json::json!(verify_token));
    }

    Ok(Some(ChannelConfig {
        channel_type: "whatsapp".to_string(),
        enabled: Some(true),
        settings,
    }))
}

/// Prompt for a token, showing the source if detected from env or existing config.
/// Returns the token string (may be empty if user skips).
fn prompt_token_with_source(
    label: &str,
    existing: Option<&str>,
    env_val: Option<&str>,
    env_var_name: &str,
) -> Result<String> {
    // Prefer env, then existing
    let source = env_val.or(existing);

    if let Some(val) = source {
        let source_label = if env_val.is_some() {
            format!("detected from {env_var_name}")
        } else {
            "from config".to_string()
        };
        let prompt = format!("{label} [{source_label}: {}]", mask_token(val));
        let input: String = Password::new()
            .with_prompt(&prompt)
            .allow_empty_password(true)
            .interact()
            .context("input cancelled")?;
        let input = input.trim().to_string();
        if input.is_empty() {
            Ok(val.to_string())
        } else {
            Ok(input)
        }
    } else {
        let input: String = Password::new()
            .with_prompt(label)
            .allow_empty_password(true)
            .interact()
            .context("input cancelled")?;
        Ok(input.trim().to_string())
    }
}

// ---------------------------------------------------------------------------
// Main wizard entry point
// ---------------------------------------------------------------------------

/// Run the interactive onboarding wizard. Writes config.yml and optionally
/// stores the API key in the credential vault.
///
/// The wizard is section-based: each section can be independently kept or
/// changed when an existing config is found. Environment variables are
/// auto-detected and API keys are validated inline.
pub async fn run_wizard(config_dir: &Path) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        println!("Non-interactive environment detected.");
        println!(
            "To configure OpenCrust, edit: {}/config.yml",
            config_dir.display()
        );
        println!();
        println!("Minimal config.yml example:");
        println!("---");
        println!("llm:");
        println!("  main:");
        println!("    provider: anthropic");
        println!("    api_key: sk-ant-...");
        println!("agent:");
        println!("  system_prompt: \"You are a helpful assistant.\"");
        return Ok(());
    }

    let existing = load_existing_config(config_dir);
    let detected = detect_env_keys();

    println!();
    println!("  OpenCrust Setup Wizard");
    println!("  ----------------------");
    println!();

    // Show env detection summary if anything was found
    if detected.has_any_llm() || detected.has_any_channel() {
        println!("  Scanning environment...");
        if detected.anthropic_api_key.is_some() {
            println!("    Found ANTHROPIC_API_KEY");
        }
        if detected.openai_api_key.is_some() {
            println!("    Found OPENAI_API_KEY");
        }
        if detected.sansa_api_key.is_some() {
            println!("    Found SANSA_API_KEY");
        }
        if detected.telegram_bot_token.is_some() {
            println!("    Found TELEGRAM_BOT_TOKEN");
        }
        if detected.discord_bot_token.is_some() {
            println!("    Found DISCORD_BOT_TOKEN");
        }
        if detected.slack_bot_token.is_some() {
            println!("    Found SLACK_BOT_TOKEN");
        }
        if detected.whatsapp_access_token.is_some() {
            println!("    Found WHATSAPP_ACCESS_TOKEN");
        }
        println!();
    }

    // --- Section 1: Provider ---
    let provider_result = section_provider(&existing, &detected).await?;

    // --- Section 2: System Prompt ---
    let new_prompt = section_system_prompt(&existing)?;

    // --- Section 3: Channels ---
    let new_channels = section_channels(&existing, &detected).await?;

    // --- Build config ---
    let mut config = existing.unwrap_or_default();

    // Apply provider changes
    if let Some(pr) = &provider_result {
        let mut llm_config = LlmProviderConfig {
            provider: pr.provider.clone(),
            model: None,
            api_key: None,
            base_url: None,
            extra: Default::default(),
        };

        if !pr.api_key.is_empty() && !pr.from_env {
            // Ask about vault storage
            let store_in_vault = {
                let choices = &[
                    "Store in encrypted vault (recommended)",
                    "Store as plaintext in config.yml",
                    "Skip storing (use env var)",
                ];
                Select::new()
                    .with_prompt("How should the API key be stored?")
                    .items(choices)
                    .default(0)
                    .interact()
                    .context("storage choice cancelled")?
            };

            let env_hint = env_var_for_provider(&pr.provider);

            match store_in_vault {
                0 => {
                    // Encrypted vault
                    let vault_path = config_dir.join("credentials").join("vault.json");
                    let passphrase: String = Password::new()
                        .with_prompt("Set a vault passphrase")
                        .with_confirmation("Confirm passphrase", "Passphrases don't match")
                        .interact()
                        .context("passphrase input cancelled")?;

                    match opencrust_security::CredentialVault::create(&vault_path, &passphrase) {
                        Ok(mut vault) => {
                            vault.set(env_hint, &pr.api_key);
                            vault.save().context("failed to save vault")?;
                            println!("  API key encrypted in vault.");
                            println!("  Set OPENCRUST_VAULT_PASSPHRASE env var for server mode.");
                        }
                        Err(e) => {
                            println!(
                                "  Warning: vault creation failed ({e}), storing in config instead."
                            );
                            llm_config.api_key = Some(pr.api_key.clone());
                        }
                    }
                }
                1 => {
                    llm_config.api_key = Some(pr.api_key.clone());
                }
                _ => {
                    println!(
                        "  Set {} environment variable before starting the server.",
                        env_hint
                    );
                }
            }
        } else if !pr.api_key.is_empty() && pr.from_env {
            // Key from env - don't store it, user already has it set
            println!(
                "  Using {} from environment (not stored in config).",
                env_var_for_provider(&pr.provider)
            );
        }

        config.llm.insert("main".to_string(), llm_config);
    }

    // Apply system prompt changes
    if let Some(prompt) = new_prompt {
        config.agent.system_prompt = if prompt.is_empty() {
            None
        } else {
            Some(prompt)
        };
    }

    // Apply channel changes
    if let Some(channels) = new_channels {
        config.channels = channels;
    }

    // Write config
    let config_path = config_dir.join("config.yml");
    let yaml = serde_yaml::to_string(&config).context("failed to serialize config")?;
    std::fs::write(&config_path, &yaml)
        .context(format!("failed to write {}", config_path.display()))?;

    info!("config written to {}", config_path.display());

    // --- Summary ---
    println!();
    println!("  Configuration Summary");
    println!("  ---------------------");

    if let Some(main) = config.llm.get("main") {
        let verified = provider_result
            .as_ref()
            .map(|p| p.verified)
            .unwrap_or(false);
        let status = if verified { " (verified)" } else { "" };
        println!("  Provider:  {}{status}", main.provider);
    }

    if let Some(prompt) = &config.agent.system_prompt {
        let display = if prompt.len() > 50 {
            format!("{}...", &prompt[..47])
        } else {
            prompt.clone()
        };
        println!("  Prompt:    {display}");
    }

    if !config.channels.is_empty() {
        let names: Vec<&str> = config.channels.keys().map(|k| k.as_str()).collect();
        println!("  Channels:  {}", names.join(", "));
    }

    println!();
    println!("  Config written to {}", config_path.display());
    println!("  Run `opencrust start` to launch.");
    println!();

    Ok(())
}
