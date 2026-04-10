use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};
use dialoguer::{Confirm, Input, MultiSelect, Password, Select};
use opencrust_config::{
    AppConfig, ChannelConfig, EmbeddingProviderConfig, LlmProviderConfig, McpServerConfig,
};
use tracing::info;

// ---------------------------------------------------------------------------
// Environment detection
// ---------------------------------------------------------------------------

/// Keys discovered in the environment.
#[derive(Default, Debug)]
struct DetectedKeys {
    anthropic_api_key: Option<String>,
    anthropic_base_url: Option<String>,
    openai_api_key: Option<String>,
    openai_base_url: Option<String>,
    sansa_api_key: Option<String>,
    sansa_base_url: Option<String>,
    telegram_bot_token: Option<String>,
    discord_bot_token: Option<String>,
    discord_app_id: Option<String>,
    slack_bot_token: Option<String>,
    slack_app_token: Option<String>,
    whatsapp_access_token: Option<String>,
    line_channel_secret: Option<String>,
    line_channel_access_token: Option<String>,
    wechat_appid: Option<String>,
    wechat_secret: Option<String>,
    wechat_token: Option<String>,
    brave_api_key: Option<String>,
    google_search_key: Option<String>,
    google_search_engine_id: Option<String>,
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
            || self.line_channel_access_token.is_some()
            || self.wechat_appid.is_some()
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

    /// Return the base URL for the given provider, if set in environment.
    fn base_url_for_provider(&self, provider: &str) -> Option<&str> {
        match provider {
            "anthropic" => self.anthropic_base_url.as_deref(),
            "openai" => self.openai_base_url.as_deref(),
            "sansa" => self.sansa_base_url.as_deref(),
            _ => None,
        }
    }
}

fn detect_env_keys() -> DetectedKeys {
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    DetectedKeys {
        anthropic_api_key: get("ANTHROPIC_API_KEY"),
        anthropic_base_url: get("ANTHROPIC_BASE_URL"),
        openai_api_key: get("OPENAI_API_KEY"),
        openai_base_url: get("OPENAI_BASE_URL"),
        sansa_api_key: get("SANSA_API_KEY"),
        sansa_base_url: get("SANSA_BASE_URL"),
        telegram_bot_token: get("TELEGRAM_BOT_TOKEN"),
        discord_bot_token: get("DISCORD_BOT_TOKEN"),
        discord_app_id: get("DISCORD_APP_ID"),
        slack_bot_token: get("SLACK_BOT_TOKEN"),
        slack_app_token: get("SLACK_APP_TOKEN"),
        whatsapp_access_token: get("WHATSAPP_ACCESS_TOKEN"),
        line_channel_secret: get("LINE_CHANNEL_SECRET"),
        line_channel_access_token: get("LINE_CHANNEL_ACCESS_TOKEN"),
        wechat_appid: get("WECHAT_APPID"),
        wechat_secret: get("WECHAT_SECRET"),
        wechat_token: get("WECHAT_TOKEN"),
        brave_api_key: get("BRAVE_API_KEY"),
        google_search_key: get("GOOGLE_SEARCH_KEY"),
        google_search_engine_id: get("GOOGLE_SEARCH_ENGINE_ID"),
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
        "vllm" => {
            // vLLM: hit /v1/models to check the server is reachable
            let url = base_url.unwrap_or("http://localhost:8000");
            let url = url.trim_end_matches('/');
            let resp = client
                .get(format!("{url}/v1/models"))
                .header("authorization", format!("Bearer {api_key}"))
                .send()
                .await?;
            let status = resp.status().as_u16();
            // 200 = ok, 401 = server reachable but key required
            status == 200 || status == 401
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

/// Validate a LINE channel access token. Returns the bot display name on success.
async fn validate_line_token(access_token: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get("https://api.line.me/v2/bot/info")
        .header("authorization", format!("Bearer {access_token}"))
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("invalid channel access token (HTTP {})", resp.status());
    }

    let body: serde_json::Value = resp.json().await?;
    let name = body["displayName"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    Ok(name)
}

async fn validate_google_search(api_key: &str, cx: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get("https://www.googleapis.com/customsearch/v1")
        .query(&[("key", api_key), ("cx", cx), ("q", "test")])
        .send()
        .await?;

    if !resp.status().is_success() {
        let err: serde_json::Value = resp.json().await?;
        let msg = err["error"]["message"].as_str().unwrap_or("unknown error");
        anyhow::bail!("{msg}");
    }
    Ok(())
}

async fn validate_brave_search(api_key: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", "test")])
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("invalid API key (HTTP {})", resp.status());
    }
    Ok(())
}

/// Validate a base URL format.
fn validate_base_url(url: &str) -> Result<()> {
    if url.is_empty() {
        return Ok(());
    }

    // Check protocol
    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!("URL must start with http:// or https://");
    }

    // Try to parse as URL
    reqwest::Url::parse(url).context("invalid URL format")?;

    Ok(())
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
    opencrust_config::providers::env_var_for_provider(provider)
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
    model: Option<String>,
    base_url: Option<String>,
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
        if detected.anthropic_base_url.is_some() {
            println!("    Found ANTHROPIC_BASE_URL");
        }
        if detected.openai_api_key.is_some() {
            println!("    Found OPENAI_API_KEY");
        }
        if detected.openai_base_url.is_some() {
            println!("    Found OPENAI_BASE_URL");
        }
        if detected.sansa_api_key.is_some() {
            println!("    Found SANSA_API_KEY");
        }
        if detected.sansa_base_url.is_some() {
            println!("    Found SANSA_BASE_URL");
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
            let base_url = detected.base_url_for_provider(provider);

            // Show custom base URL if detected
            if let Some(url) = base_url {
                println!("  Using custom base URL: {}", url);
            }

            // Validate
            print!("  Testing {provider} connection... ");
            match validate_llm_key(provider, key, base_url).await {
                Ok(true) => {
                    println!("connected");
                    return Ok(Some(ProviderResult {
                        provider: provider.to_string(),
                        api_key: key.to_string(),
                        model: None,
                        base_url: base_url.map(|s| s.to_string()),
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

    // Manual provider selection - driven by the provider registry
    use opencrust_config::providers::KNOWN_PROVIDERS;

    let display_names: Vec<&str> = KNOWN_PROVIDERS.iter().map(|p| p.display_name).collect();
    let default_idx = existing_provider
        .as_deref()
        .and_then(|ep| KNOWN_PROVIDERS.iter().position(|p| p.id == ep))
        .unwrap_or(0);
    let selection = Select::new()
        .with_prompt("Select your LLM provider")
        .items(&display_names)
        .default(default_idx)
        .interact()
        .context("provider selection cancelled")?;
    let known = &KNOWN_PROVIDERS[selection];
    let env_hint = known.env_var;

    // Existing config values
    let existing_base_url = existing
        .as_ref()
        .and_then(|c| c.llm.get("main"))
        .and_then(|p| p.base_url.as_ref())
        .map(|s| s.as_str());
    let existing_model = existing
        .as_ref()
        .and_then(|c| c.llm.get("main"))
        .and_then(|p| p.model.as_deref());

    // Base URL - local providers always show it, cloud providers gate behind "Advanced"
    let base_url = if known.is_local {
        let default = existing_base_url
            .or(known.default_base_url)
            .unwrap_or("http://localhost");
        let input: String = Input::new()
            .with_prompt(format!("Base URL [{}]", default))
            .default(default.to_string())
            .allow_empty(true)
            .validate_with(|input: &String| -> Result<(), String> {
                if input.is_empty() {
                    return Ok(());
                }
                validate_base_url(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("base URL input cancelled")?;
        let url = input.trim();
        if url.is_empty() {
            known.default_base_url.map(|s| s.to_string())
        } else {
            Some(url.to_string())
        }
    } else {
        let show_advanced = Confirm::new()
            .with_prompt("Advanced options? (custom endpoint, proxy)")
            .default(false)
            .interact()
            .unwrap_or(false);

        if show_advanced {
            let default = existing_base_url.unwrap_or("");
            let input: String = Input::new()
                .with_prompt("API endpoint URL (Enter for default)")
                .default(default.to_string())
                .allow_empty(true)
                .validate_with(|input: &String| -> Result<(), String> {
                    if input.is_empty() {
                        return Ok(());
                    }
                    validate_base_url(input).map_err(|e| e.to_string())
                })
                .interact_text()
                .context("base URL input cancelled")?;
            if input.trim().is_empty() {
                None
            } else {
                Some(input.trim().to_string())
            }
        } else {
            existing_base_url.map(|s| s.to_string())
        }
    };

    // Model - prompt if provider is local (user picks which model to run)
    let model = if known.is_local {
        let default_model = existing_model.or(known.default_model).unwrap_or("");
        let input: String = Input::new()
            .with_prompt("Model name")
            .default(default_model.to_string())
            .allow_empty(!default_model.is_empty())
            .interact_text()
            .context("model name input cancelled")?;
        let m = input.trim();
        if m.is_empty() {
            known.default_model.map(|s| s.to_string())
        } else {
            Some(m.to_string())
        }
    } else {
        None
    };

    // API key - skip for providers that don't need one
    if !known.requires_api_key {
        // Test connectivity for local providers
        let effective_url = base_url
            .as_deref()
            .or(known.default_base_url)
            .unwrap_or("http://localhost");
        print!("  Testing {} connection... ", known.display_name);
        match validate_llm_key(known.id, "unused", Some(effective_url)).await {
            Ok(true) => println!("connected"),
            Ok(false) => println!("unreachable"),
            Err(e) => println!("skipped ({e})"),
        }

        return Ok(Some(ProviderResult {
            provider: known.id.to_string(),
            api_key: String::new(),
            model,
            base_url,
            from_env: true,
            verified: false,
        }));
    }

    // API key entry with retry loop
    let provider_id = known.id;
    loop {
        let key_prompt = if existing_has_key {
            format!(
                "{provider_id} API key (Enter to keep existing, or set {env_hint} env var later)"
            )
        } else {
            format!("{provider_id} API key (or set {env_hint} env var later)")
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
                provider: provider_id.to_string(),
                api_key: old_key,
                model: None,
                base_url: base_url.clone(),
                from_env: false,
                verified: false,
            }));
        }

        // Empty key with no existing - skip (will use env var)
        if api_key.is_empty() {
            println!("  Set {env_hint} environment variable before starting the server.");
            return Ok(Some(ProviderResult {
                provider: provider_id.to_string(),
                api_key: String::new(),
                model: None,
                base_url,
                from_env: true,
                verified: false,
            }));
        }

        // Validate the key
        print!("  Testing {provider_id} connection... ");
        match validate_llm_key(provider_id, &api_key, base_url.as_deref()).await {
            Ok(true) => {
                println!("connected");
                return Ok(Some(ProviderResult {
                    provider: provider_id.to_string(),
                    api_key,
                    model: None,
                    base_url: base_url.clone(),
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
                        provider: provider_id.to_string(),
                        api_key,
                        model: None,
                        base_url: base_url.clone(),
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
                        provider: provider_id.to_string(),
                        api_key,
                        model: None,
                        base_url: base_url.clone(),
                        from_env: false,
                        verified: false,
                    }));
                }
            }
        }
    }
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
    let channel_names = ["Telegram", "Discord", "Slack", "WhatsApp", "LINE", "WeChat"];
    let mut defaults = vec![false; 6];

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
    if detected.line_channel_access_token.is_some() || existing_channels.contains_key("line-bot") {
        defaults[4] = true;
    }
    if detected.wechat_appid.is_some() || existing_channels.contains_key("wechat") {
        defaults[5] = true;
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
            4 => {
                // LINE
                let existing_secret = channels
                    .get("line-bot")
                    .and_then(|c| c.settings.get("channel_secret"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let existing_token = channels
                    .get("line-bot")
                    .and_then(|c| c.settings.get("channel_access_token"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(cfg) = setup_line(
                    existing_secret.as_deref(),
                    existing_token.as_deref(),
                    detected.line_channel_secret.as_deref(),
                    detected.line_channel_access_token.as_deref(),
                )
                .await?
                {
                    channels.insert("line-bot".to_string(), cfg);
                }
            }
            5 => {
                // WeChat
                let existing_appid = channels
                    .get("wechat")
                    .and_then(|c| c.settings.get("appid"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let existing_secret = channels
                    .get("wechat")
                    .and_then(|c| c.settings.get("secret"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let existing_token = channels
                    .get("wechat")
                    .and_then(|c| c.settings.get("token"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(cfg) = setup_wechat(
                    existing_appid.as_deref(),
                    existing_secret.as_deref(),
                    existing_token.as_deref(),
                    detected.wechat_appid.as_deref(),
                    detected.wechat_secret.as_deref(),
                    detected.wechat_token.as_deref(),
                )
                .await?
                {
                    channels.insert("wechat".to_string(), cfg);
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
            4 => "line-bot",
            5 => "wechat",
            _ => "",
        })
        .collect();
    let known = [
        "telegram", "discord", "slack", "whatsapp", "line-bot", "wechat",
    ];
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

    let inject_name = Confirm::new()
        .with_prompt("  Show user display names to the bot? (recommended for servers)")
        .default(true)
        .interact()
        .unwrap_or(true);

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
    if inject_name {
        settings.insert("inject_user_name".to_string(), serde_json::json!(true));
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
    println!("  Slack Setup (Socket Mode - no public URL needed)");
    println!();
    println!("  1. Create a Slack app at https://api.slack.com/apps (From scratch)");
    println!("  2. Enable Socket Mode (left sidebar) and create an app-level token");
    println!("     with the connections:write scope. Copy the xapp-... token.");
    println!("  3. Go to Event Subscriptions, enable events, and subscribe to:");
    println!("     message.im, message.channels, message.groups");
    println!("  4. Go to OAuth & Permissions and add bot scopes:");
    println!("     chat:write, files:read");
    println!("  5. Install the app to your workspace and copy the xoxb-... bot token.");
    println!();
    println!("  Full guide: https://opencrust-org.github.io/opencrust/channels/slack.html");
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

    let inject_name = Confirm::new()
        .with_prompt("  Show user display names to the bot? (recommended for shared channels)")
        .default(true)
        .interact()
        .unwrap_or(true);

    let mut settings = HashMap::new();
    settings.insert("bot_token".to_string(), serde_json::json!(bot_token));
    if !app_token.is_empty() {
        settings.insert("app_token".to_string(), serde_json::json!(app_token));
    }
    if inject_name {
        settings.insert("inject_user_name".to_string(), serde_json::json!(true));
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
    println!();

    let modes = ["WhatsApp Web (QR code - personal)", "WhatsApp Business API"];
    let mode_idx = Select::new()
        .with_prompt("Which mode?")
        .items(&modes)
        .default(0)
        .interact()
        .context("mode selection cancelled")?;

    if mode_idx == 0 {
        // WhatsApp Web mode - no credentials needed
        println!();
        println!("  WhatsApp Web will show a QR code when you start the bot.");
        println!("  Scan it with your phone to link your personal WhatsApp.");
        println!("  Requires Node.js to be installed.");
        println!();

        let mut settings = HashMap::new();
        settings.insert("mode".to_string(), serde_json::json!("web"));

        return Ok(Some(ChannelConfig {
            channel_type: "whatsapp".to_string(),
            enabled: Some(true),
            settings,
        }));
    }

    // WhatsApp Business API mode
    println!();
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
    settings.insert("mode".to_string(), serde_json::json!("business"));
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

async fn setup_line(
    existing_secret: Option<&str>,
    existing_token: Option<&str>,
    env_secret: Option<&str>,
    env_token: Option<&str>,
) -> Result<Option<ChannelConfig>> {
    println!();
    println!("  LINE Setup");
    println!("  1. Go to https://developers.line.biz and open your channel");
    println!("  2. Under 'Basic settings', copy the Channel secret");
    println!("  3. Under 'Messaging API', issue a Channel access token (long-lived)");
    println!("  4. Set the webhook URL to: https://<your-host>/webhooks/line");
    println!();

    let secret = prompt_token_with_source(
        "Channel secret",
        existing_secret,
        env_secret,
        "LINE_CHANNEL_SECRET",
    )?;

    if secret.is_empty() {
        println!("  Skipping LINE (no channel secret provided).");
        return Ok(None);
    }

    let access_token = prompt_token_with_source(
        "Channel access token",
        existing_token,
        env_token,
        "LINE_CHANNEL_ACCESS_TOKEN",
    )?;

    if access_token.is_empty() {
        println!("  Skipping LINE (no channel access token provided).");
        return Ok(None);
    }

    // Validate
    print!("  Testing connection... ");
    match validate_line_token(&access_token).await {
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

    let group_policy_choices = &[
        "open (respond to all group messages)",
        "mention (respond only when mentioned)",
        "disabled",
    ];
    let group_idx = Select::new()
        .with_prompt("Group message policy")
        .items(group_policy_choices)
        .default(0)
        .interact()
        .context("selection cancelled")?;
    let group_policy = match group_idx {
        1 => "mention",
        2 => "disabled",
        _ => "open",
    };

    let mut settings = HashMap::new();
    settings.insert("channel_secret".to_string(), serde_json::json!(secret));
    settings.insert(
        "channel_access_token".to_string(),
        serde_json::json!(access_token),
    );
    settings.insert("group_policy".to_string(), serde_json::json!(group_policy));

    Ok(Some(ChannelConfig {
        channel_type: "line".to_string(),
        enabled: Some(true),
        settings,
    }))
}

async fn setup_wechat(
    existing_appid: Option<&str>,
    existing_secret: Option<&str>,
    existing_token: Option<&str>,
    env_appid: Option<&str>,
    env_secret: Option<&str>,
    env_token: Option<&str>,
) -> Result<Option<ChannelConfig>> {
    println!();
    println!("  WeChat Official Account Setup");
    println!("  1. Go to https://mp.weixin.qq.com and open your Official Account");
    println!("  2. Under 'Basic Configuration', copy the AppID and AppSecret");
    println!("  3. Set a Token (any string) for webhook signature verification");
    println!("  4. Set the webhook URL to: https://<your-host>/webhooks/wechat");
    println!();

    let appid = prompt_token_with_source("AppID", existing_appid, env_appid, "WECHAT_APPID")?;

    if appid.is_empty() {
        println!("  Skipping WeChat (no AppID provided).");
        return Ok(None);
    }

    let secret =
        prompt_token_with_source("AppSecret", existing_secret, env_secret, "WECHAT_SECRET")?;

    if secret.is_empty() {
        println!("  Skipping WeChat (no AppSecret provided).");
        return Ok(None);
    }

    let token =
        prompt_token_with_source("Webhook Token", existing_token, env_token, "WECHAT_TOKEN")?;

    if token.is_empty() {
        println!("  Skipping WeChat (no webhook token provided).");
        return Ok(None);
    }

    // WeChat Official Accounts are always 1:1 (is_group is always false),
    // so dm_policy is the relevant setting here.
    let dm_policy_choices = &["open (respond to all messages)", "disabled"];
    let dm_idx = Select::new()
        .with_prompt("DM policy")
        .items(dm_policy_choices)
        .default(0)
        .interact()
        .context("selection cancelled")?;
    let dm_policy = match dm_idx {
        1 => "disabled",
        _ => "open",
    };

    let mut settings = HashMap::new();
    settings.insert("appid".to_string(), serde_json::json!(appid));
    settings.insert("secret".to_string(), serde_json::json!(secret));
    settings.insert("token".to_string(), serde_json::json!(token));
    settings.insert("dm_policy".to_string(), serde_json::json!(dm_policy));

    Ok(Some(ChannelConfig {
        channel_type: "wechat".to_string(),
        enabled: Some(true),
        settings,
    }))
}

async fn section_tools(
    existing: &Option<AppConfig>,
    detected: &DetectedKeys,
) -> Result<Option<opencrust_config::WebSearchConfig>> {
    println!();
    println!("  --- Web Search ---");

    let existing_search = existing.as_ref().and_then(|c| c.tools.web_search.as_ref());

    if let Some(cfg) = existing_search {
        println!("  Current: {} (api key configured)", cfg.provider);
        let choices = &["Keep current", "Change/Configure"];
        let sel = Select::new()
            .with_prompt("Web Search")
            .items(choices)
            .default(0)
            .interact()
            .context("selection cancelled")?;
        if sel == 0 {
            return Ok(None);
        }
    }

    let providers = &["brave", "google", "none"];
    let selection = Select::new()
        .with_prompt("Select search provider")
        .items(providers)
        .default(0)
        .interact()
        .context("selection cancelled")?;

    let provider = providers[selection];
    if provider == "none" {
        return Ok(Some(opencrust_config::WebSearchConfig {
            provider: "none".to_string(),
            api_key: None,
            search_engine_id: None,
        }));
    }

    if provider == "google" {
        setup_google_search(existing_search, detected).await
    } else {
        setup_brave_search(existing_search, detected).await
    }
}

async fn setup_google_search(
    existing: Option<&opencrust_config::WebSearchConfig>,
    detected: &DetectedKeys,
) -> Result<Option<opencrust_config::WebSearchConfig>> {
    println!();
    println!("  Google Custom Search Setup");
    println!("  1. Go to https://developers.google.com/custom-search/v1/introduction");
    println!("  2. Get an API Key and create a Search Engine (CX)");
    println!();

    let existing_key = existing.and_then(|c| c.api_key.as_deref());
    let existing_cx = existing.and_then(|c| c.search_engine_id.as_deref());

    let api_key = prompt_token_with_source(
        "API Key",
        existing_key,
        detected.google_search_key.as_deref(),
        "GOOGLE_SEARCH_KEY",
    )?;

    if api_key.is_empty() {
        return Ok(None);
    }

    let cx_source = detected.google_search_engine_id.as_deref().or(existing_cx);
    let cx_prompt = if let Some(src) = cx_source {
        format!("Search Engine ID (CX) [{}]", mask_token(src))
    } else {
        "Search Engine ID (CX)".to_string()
    };
    let cx: String = Input::new()
        .with_prompt(&cx_prompt)
        .default(cx_source.unwrap_or("").to_string())
        .allow_empty(true)
        .interact_text()
        .context("input cancelled")?;
    let cx = cx.trim().to_string();

    if cx.is_empty() {
        return Ok(None);
    }

    // Validate
    print!("  Testing connection... ");
    match validate_google_search(&api_key, &cx).await {
        Ok(_) => println!("ok"),
        Err(e) => {
            println!("failed ({e})");
            if !Confirm::new()
                .with_prompt("  Save anyway?")
                .default(true)
                .interact()
                .unwrap_or(true)
            {
                return Ok(None);
            }
        }
    }

    Ok(Some(opencrust_config::WebSearchConfig {
        provider: "google".to_string(),
        api_key: Some(api_key),
        search_engine_id: Some(cx),
    }))
}

async fn setup_brave_search(
    existing: Option<&opencrust_config::WebSearchConfig>,
    detected: &DetectedKeys,
) -> Result<Option<opencrust_config::WebSearchConfig>> {
    println!();
    println!("  Brave Search Setup");
    println!("  1. Go to https://api.search.brave.com/app/dashboard");
    println!("  2. Get a subscription token");
    println!();

    let existing_key = existing.and_then(|c| c.api_key.as_deref());

    let api_key = prompt_token_with_source(
        "API Key",
        existing_key,
        detected.brave_api_key.as_deref(),
        "BRAVE_API_KEY",
    )?;

    if api_key.is_empty() {
        return Ok(None);
    }

    // Validate
    print!("  Testing connection... ");
    match validate_brave_search(&api_key).await {
        Ok(_) => println!("ok"),
        Err(e) => {
            println!("failed ({e})");
            if !Confirm::new()
                .with_prompt("  Save anyway?")
                .default(true)
                .interact()
                .unwrap_or(true)
            {
                return Ok(None);
            }
        }
    }

    Ok(Some(opencrust_config::WebSearchConfig {
        provider: "brave".to_string(),
        api_key: Some(api_key),
        search_engine_id: None,
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
        if detected.line_channel_access_token.is_some() {
            println!("    Found LINE_CHANNEL_ACCESS_TOKEN");
        }
        if detected.line_channel_secret.is_some() {
            println!("    Found LINE_CHANNEL_SECRET");
        }
        if detected.wechat_appid.is_some() {
            println!("    Found WECHAT_APPID");
        }
        if detected.wechat_secret.is_some() {
            println!("    Found WECHAT_SECRET");
        }
        if detected.wechat_token.is_some() {
            println!("    Found WECHAT_TOKEN");
        }
        println!();
    }

    // --- Section 1: Provider ---
    let provider_result = section_provider(&existing, &detected).await?;

    // --- Section 2: Channels ---
    let new_channels = section_channels(&existing, &detected).await?;

    // --- Section 3: Tools ---
    let new_search_config = section_tools(&existing, &detected).await?;

    // --- Section 4: Embeddings / RAG ---
    let selected_provider = provider_result.as_ref().map(|p| p.provider.as_str());
    let embedding_config = section_embeddings(selected_provider, config_dir).await?;

    // --- Build config ---
    let mut config = existing.unwrap_or_default();

    // Apply provider changes
    if let Some(pr) = &provider_result {
        let mut llm_config = LlmProviderConfig {
            provider: pr.provider.clone(),
            model: pr.model.clone(),
            api_key: None,
            base_url: pr.base_url.clone(),
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

    // Apply channel changes
    if let Some(channels) = new_channels {
        config.channels = channels;
    }

    // Apply tools changes
    if let Some(search_cfg) = new_search_config {
        config.tools.web_search = Some(search_cfg);
    }

    // Apply embeddings
    if let Some((embed_name, embed_config)) = &embedding_config {
        config
            .embeddings
            .insert(embed_name.clone(), embed_config.clone());
        config.memory.embedding_provider = Some(embed_name.clone());
        config.memory.enabled = true;
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

    if !config.channels.is_empty() {
        let names: Vec<&str> = config.channels.keys().map(|k| k.as_str()).collect();
        println!("  Channels:  {}", names.join(", "));
    }

    if let Some((name, cfg)) = &embedding_config {
        let model = cfg.model.as_deref().unwrap_or("default");
        println!("  Embeddings: {} ({model})", name);
        println!("  RAG:        enabled - use `opencrust doc add <file>` to ingest documents");
    }

    println!();
    println!("  Config written to {}", config_path.display());
    println!("  Run `opencrust start` to launch.");
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Embeddings / RAG wizard section
// ---------------------------------------------------------------------------

/// Check if Ollama is running locally.
async fn is_ollama_available() -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();
    client
        .get("http://localhost:11434/api/tags")
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Pull an Ollama model, showing progress.
async fn ollama_pull_model(model: &str) -> bool {
    println!("  Pulling {model}...");
    let output = tokio::process::Command::new("ollama")
        .args(["pull", model])
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await;
    match output {
        Ok(status) if status.success() => {
            println!("  Done.");
            true
        }
        _ => {
            println!(
                "  Warning: failed to pull {model}. You can pull it manually with `ollama pull {model}`."
            );
            false
        }
    }
}

/// Wizard section for configuring embeddings and RAG.
async fn section_embeddings(
    selected_llm_provider: Option<&str>,
    config_dir: &Path,
) -> Result<Option<(String, EmbeddingProviderConfig)>> {
    println!();
    println!("  -- Document Search (RAG) --");
    println!();

    let enable = Confirm::new()
        .with_prompt("Enable document search? (lets the agent search your files)")
        .default(true)
        .interact()
        .unwrap_or(false);

    if !enable {
        return Ok(None);
    }

    let ollama_available = is_ollama_available().await;
    let using_ollama = selected_llm_provider == Some("ollama");

    // If already using Ollama, default to Ollama embeddings
    if using_ollama || ollama_available {
        let use_ollama = if using_ollama {
            println!("  Ollama detected - will use it for embeddings too.");
            true
        } else {
            let choices = &[
                "Ollama (local, free - already running)",
                "Cohere (API, free tier 1000 calls/month)",
                "Skip for now",
            ];
            let selection = Select::new()
                .with_prompt("Embedding provider")
                .items(choices)
                .default(0)
                .interact()
                .context("selection cancelled")?;
            match selection {
                0 => true,
                1 => {
                    return setup_cohere_embeddings(config_dir).await;
                }
                _ => return Ok(None),
            }
        };

        if use_ollama {
            let model = "nomic-embed-text";
            ollama_pull_model(model).await;

            return Ok(Some((
                "local".to_string(),
                EmbeddingProviderConfig {
                    provider: "ollama".to_string(),
                    model: Some(model.to_string()),
                    api_key: None,
                    base_url: None,
                    dimensions: None,
                    extra: HashMap::new(),
                },
            )));
        }
    }

    // No Ollama available - offer Cohere or skip
    let choices = &["Cohere (API, free tier 1000 calls/month)", "Skip for now"];
    let selection = Select::new()
        .with_prompt("Embedding provider")
        .items(choices)
        .default(0)
        .interact()
        .context("selection cancelled")?;

    if selection == 0 {
        return setup_cohere_embeddings(config_dir).await;
    }

    Ok(None)
}

/// Set up Cohere embeddings with API key.
async fn setup_cohere_embeddings(
    config_dir: &Path,
) -> Result<Option<(String, EmbeddingProviderConfig)>> {
    // Check env first
    let env_key = std::env::var("COHERE_API_KEY").ok();
    let api_key = if let Some(ref key) = env_key {
        println!("  Found COHERE_API_KEY in environment.");
        key.clone()
    } else {
        println!();
        println!("  Get a free API key at: https://dashboard.cohere.com/api-keys");
        let key: String = Password::new()
            .with_prompt("Cohere API key")
            .interact()
            .context("input cancelled")?;
        if key.is_empty() {
            println!("  Skipped.");
            return Ok(None);
        }

        // Store in vault
        let vault_path = config_dir.join("credentials").join("vault.json");
        if opencrust_security::try_vault_set(&vault_path, "COHERE_API_KEY", &key) {
            println!("  Stored in vault.");
        }

        key
    };

    // Quick health check
    let provider =
        opencrust_agents::CohereEmbeddingProvider::new(&api_key, None::<String>, None::<String>);
    match opencrust_agents::EmbeddingProvider::health_check(&provider).await {
        Ok(true) => println!("  Cohere connection verified."),
        _ => {
            println!("  Warning: could not verify Cohere connection. Config will be saved anyway.")
        }
    }

    Ok(Some((
        "cohere-main".to_string(),
        EmbeddingProviderConfig {
            provider: "cohere".to_string(),
            model: Some("embed-english-v3.0".to_string()),
            api_key: None, // resolved from vault or env at runtime
            base_url: None,
            dimensions: None,
            extra: HashMap::new(),
        },
    )))
}

// ---------------------------------------------------------------------------
// MCP server wizard
// ---------------------------------------------------------------------------

/// Interactive wizard for `opencrust mcp add`.
pub async fn run_mcp_add_wizard(config_dir: &Path, pre_selected: Option<&str>) -> Result<()> {
    use crate::mcp_registry::{self, KNOWN_MCP_SERVERS};

    let config_path = config_dir.join("config.yml");
    let mut config = load_existing_config(config_dir).unwrap_or_default();

    // --- Server selection ---
    let known = if let Some(id) = pre_selected {
        mcp_registry::find_known_server(id)
    } else {
        let mut items: Vec<String> = KNOWN_MCP_SERVERS
            .iter()
            .map(|s| format!("{} - {}", s.display_name, s.description))
            .collect();
        items.push("Custom (enter manually)".to_string());

        let selection = Select::new()
            .with_prompt("Select an MCP server")
            .items(&items)
            .default(0)
            .interact()
            .context("selection cancelled")?;

        if selection < KNOWN_MCP_SERVERS.len() {
            Some(&KNOWN_MCP_SERVERS[selection])
        } else {
            None
        }
    };

    let (server_name, mcp_config) = if let Some(server) = known {
        add_known_server(config_dir, server, &config).await?
    } else {
        add_custom_server(config_dir, &config).await?
    };

    // --- Connection validation ---
    let should_test = Confirm::new()
        .with_prompt("Test connection now?")
        .default(true)
        .interact()
        .unwrap_or(true);

    if should_test {
        println!("Connecting to '{server_name}'...");
        let manager = opencrust_agents::McpManager::new();
        let timeout = mcp_config.timeout.unwrap_or(30);

        let result = match mcp_config.transport.as_str() {
            "http" => {
                if let Some(ref url) = mcp_config.url {
                    manager.connect_http(&server_name, url, timeout).await
                } else {
                    Err(opencrust_common::Error::Agent(
                        "HTTP transport but no url".into(),
                    ))
                }
            }
            _ => {
                // Resolve env vars through vault for the test connection
                let resolved_env =
                    resolve_wizard_mcp_env(&server_name, &mcp_config.env, config_dir);
                manager
                    .connect(
                        &server_name,
                        &mcp_config.command,
                        &mcp_config.args,
                        &resolved_env,
                        timeout,
                    )
                    .await
            }
        };

        match result {
            Ok(()) => {
                let tools = manager.tool_info(&server_name).await;
                println!(
                    "  Connected ({} tool{})",
                    tools.len(),
                    if tools.len() == 1 { "" } else { "s" }
                );
                for tool in &tools {
                    let desc = tool.description.as_deref().unwrap_or("");
                    println!("    {} - {desc}", tool.name);
                }
                manager.disconnect(&server_name).await;
            }
            Err(e) => {
                println!("  Connection failed: {e}");
                println!(
                    "  Config will still be saved. Check the server setup and try `opencrust mcp inspect {server_name}`."
                );
            }
        }
    }

    // --- Save config ---
    config.mcp.insert(server_name.clone(), mcp_config);
    let yaml = serde_yaml::to_string(&config).context("failed to serialize config")?;
    std::fs::write(&config_path, &yaml)
        .context(format!("failed to write {}", config_path.display()))?;

    println!();
    println!("  Server '{server_name}' added to config.yml.");
    println!("  Hot-reload will pick it up if the server is running.");
    println!();

    Ok(())
}

/// Add a known server from the registry.
async fn add_known_server(
    config_dir: &Path,
    server: &crate::mcp_registry::KnownMcpServer,
    existing_config: &AppConfig,
) -> Result<(String, McpServerConfig)> {
    println!();
    println!("  -- {} --", server.display_name);
    if !server.setup_instructions.is_empty() {
        println!("  {}", server.setup_instructions);
    }
    println!();

    // Server name
    let default_name = server.id.to_string();
    let name: String = Input::new()
        .with_prompt("Server name in config")
        .default(default_name)
        .interact_text()
        .context("name input cancelled")?;

    if existing_config.mcp.contains_key(&name) {
        anyhow::bail!(
            "MCP server '{name}' already exists in config. Remove it first with `opencrust mcp remove {name}`."
        );
    }

    // Collect secrets
    let mut env = HashMap::new();
    let vault_path = config_dir.join("credentials").join("vault.json");

    for req in server.required_env {
        println!();
        println!("  {}", req.description);

        let value: String = if req.is_secret {
            Password::new()
                .with_prompt(req.key)
                .interact()
                .context("input cancelled")?
        } else {
            Input::new()
                .with_prompt(req.key)
                .interact_text()
                .context("input cancelled")?
        };

        if req.is_secret && !value.is_empty() {
            let vault_key = format!("MCP_{}_{}", name.to_uppercase().replace('-', "_"), req.key);
            if opencrust_security::try_vault_set(&vault_path, &vault_key, &value) {
                println!("  Stored in vault.");
                env.insert(req.key.to_string(), String::new()); // empty = vault sentinel
            } else {
                println!("  Vault unavailable, storing in config (plaintext).");
                env.insert(req.key.to_string(), value);
            }
        } else if !value.is_empty() {
            env.insert(req.key.to_string(), value);
        }
    }

    // For filesystem server, prompt for allowed paths
    let mut args: Vec<String> = server.args.iter().map(|s| s.to_string()).collect();
    if server.id == "filesystem" {
        let paths: String = Input::new()
            .with_prompt("Allowed directory paths (space-separated)")
            .default("/tmp".to_string())
            .interact_text()
            .context("path input cancelled")?;
        for p in paths.split_whitespace() {
            args.push(p.to_string());
        }
    }

    let mcp_config = McpServerConfig {
        command: server.command.to_string(),
        args,
        env,
        transport: server.transport.to_string(),
        url: None,
        enabled: Some(true),
        timeout: None,
    };

    Ok((name, mcp_config))
}

/// Add a custom MCP server.
async fn add_custom_server(
    config_dir: &Path,
    existing_config: &AppConfig,
) -> Result<(String, McpServerConfig)> {
    println!();
    println!("  -- Custom MCP Server --");
    println!();

    let name: String = Input::new()
        .with_prompt("Server name")
        .interact_text()
        .context("name input cancelled")?;

    if existing_config.mcp.contains_key(&name) {
        anyhow::bail!("MCP server '{name}' already exists in config.");
    }

    let transport_choices = &["stdio", "http"];
    let transport_idx = Select::new()
        .with_prompt("Transport")
        .items(transport_choices)
        .default(0)
        .interact()
        .context("transport selection cancelled")?;
    let transport = transport_choices[transport_idx].to_string();

    let (command, args, url) = if transport == "http" {
        let url: String = Input::new()
            .with_prompt("Server URL")
            .interact_text()
            .context("url input cancelled")?;
        (String::new(), vec![], Some(url))
    } else {
        let command: String = Input::new()
            .with_prompt("Command (e.g. npx)")
            .interact_text()
            .context("command input cancelled")?;
        let args_str: String = Input::new()
            .with_prompt("Arguments (space-separated)")
            .default(String::new())
            .interact_text()
            .context("args input cancelled")?;
        let args: Vec<String> = args_str.split_whitespace().map(|s| s.to_string()).collect();
        (command, args, None)
    };

    // Env vars
    let mut env = HashMap::new();
    let vault_path = config_dir.join("credentials").join("vault.json");

    let add_env = Confirm::new()
        .with_prompt("Add environment variables (API tokens, etc.)?")
        .default(false)
        .interact()
        .unwrap_or(false);

    if add_env {
        loop {
            let key: String = Input::new()
                .with_prompt("Env var name (empty to finish)")
                .default(String::new())
                .interact_text()
                .context("env key input cancelled")?;

            if key.is_empty() {
                break;
            }

            let is_secret = Confirm::new()
                .with_prompt("Is this a secret (store in vault)?")
                .default(true)
                .interact()
                .unwrap_or(true);

            let value: String = if is_secret {
                Password::new()
                    .with_prompt(&key)
                    .interact()
                    .context("input cancelled")?
            } else {
                Input::new()
                    .with_prompt(&key)
                    .interact_text()
                    .context("input cancelled")?
            };

            if is_secret && !value.is_empty() {
                let vault_key = format!("MCP_{}_{}", name.to_uppercase().replace('-', "_"), &key);
                if opencrust_security::try_vault_set(&vault_path, &vault_key, &value) {
                    println!("  Stored in vault.");
                    env.insert(key, String::new());
                } else {
                    println!("  Vault unavailable, storing in config (plaintext).");
                    env.insert(key, value);
                }
            } else if !value.is_empty() {
                env.insert(key, value);
            }
        }
    }

    let mcp_config = McpServerConfig {
        command,
        args,
        env,
        transport,
        url,
        enabled: Some(true),
        timeout: None,
    };

    Ok((name, mcp_config))
}

/// Resolve MCP env vars for the wizard's test connection (mirrors bootstrap logic).
fn resolve_wizard_mcp_env(
    server_name: &str,
    env: &HashMap<String, String>,
    config_dir: &Path,
) -> HashMap<String, String> {
    let vault_path = config_dir.join("credentials").join("vault.json");
    let mut resolved = HashMap::new();
    for (key, value) in env {
        if value.is_empty() {
            let vault_key = format!(
                "MCP_{}_{}",
                server_name.to_uppercase().replace('-', "_"),
                key
            );
            if let Some(secret) = opencrust_security::try_vault_get(&vault_path, &vault_key) {
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

/// Remove an MCP server from config and clean up vault entries.
pub fn run_mcp_remove(config_dir: &Path, config: &AppConfig, name: &str) -> Result<()> {
    let config_path = config_dir.join("config.yml");
    let mut config = config.clone();

    if config.mcp.remove(name).is_none() {
        println!("MCP server '{name}' not found in config.yml.");
        println!("If it's in ~/.opencrust/mcp.json, remove it manually.");
        return Ok(());
    }

    // Clean up vault entries matching MCP_{NAME}_*
    let vault_path = config_dir.join("credentials").join("vault.json");
    let prefix = format!("MCP_{}_", name.to_uppercase().replace('-', "_"));

    if opencrust_security::CredentialVault::exists(&vault_path)
        && let Ok(passphrase) = std::env::var("OPENCRUST_VAULT_PASSPHRASE")
        && let Ok(vault) = opencrust_security::CredentialVault::open(&vault_path, &passphrase)
    {
        let keys_to_remove: Vec<String> = vault
            .list_keys()
            .iter()
            .filter(|k| k.starts_with(&prefix))
            .map(|k| k.to_string())
            .collect();
        for key in &keys_to_remove {
            opencrust_security::try_vault_remove(&vault_path, key);
        }
        if !keys_to_remove.is_empty() {
            println!(
                "Removed {} vault credential{}.",
                keys_to_remove.len(),
                if keys_to_remove.len() == 1 { "" } else { "s" }
            );
        }
    }

    let yaml = serde_yaml::to_string(&config).context("failed to serialize config")?;
    std::fs::write(&config_path, &yaml)
        .context(format!("failed to write {}", config_path.display()))?;

    println!("Removed MCP server '{name}' from config.yml.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_base_url_empty() {
        assert!(validate_base_url("").is_ok());
    }

    #[test]
    fn test_validate_base_url_https() {
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
    }

    #[test]
    fn test_validate_base_url_http() {
        assert!(validate_base_url("http://localhost:8080").is_ok());
    }

    #[test]
    fn test_validate_base_url_no_protocol() {
        let result = validate_base_url("api.openai.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must start with"));
    }

    #[test]
    fn test_validate_base_url_wrong_protocol() {
        let result = validate_base_url("ftp://example.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must start with"));
    }

    #[test]
    fn test_validate_base_url_invalid_format() {
        let result = validate_base_url("https://invalid url with spaces");
        assert!(result.is_err());
    }
}
