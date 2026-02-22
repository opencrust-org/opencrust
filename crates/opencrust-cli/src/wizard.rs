use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};
use dialoguer::{Input, Password, Select};
use opencrust_config::{AppConfig, LlmProviderConfig};
use tracing::info;

/// Try to load existing config from the config directory.
fn load_existing_config(config_dir: &Path) -> Option<AppConfig> {
    let loader = opencrust_config::ConfigLoader::with_dir(config_dir);
    if loader.config_file_exists() {
        loader.load().ok()
    } else {
        None
    }
}

/// Print a summary of the current configuration.
fn print_config_summary(config: &AppConfig) {
    if let Some(main) = config.llm.get("main") {
        println!("  Provider:      {}", main.provider);
        if main.api_key.is_some() {
            println!("  API key:       configured");
        } else {
            println!("  API key:       not set (using env var)");
        }
    } else {
        println!("  Provider:      not configured");
    }

    if let Some(prompt) = &config.agent.system_prompt {
        let display = if prompt.len() > 60 {
            format!("{}...", &prompt[..57])
        } else {
            prompt.clone()
        };
        println!("  System prompt: {display}");
    }

    let channels: Vec<&str> = config.channels.keys().map(|k| k.as_str()).collect();
    if !channels.is_empty() {
        println!("  Channels:      {}", channels.join(", "));
    }

    let mcp_count = config.mcp.len();
    if mcp_count > 0 {
        println!("  MCP servers:   {mcp_count}");
    }

    println!();
}

/// Run the interactive onboarding wizard. Writes config.yml and optionally
/// stores the API key in the credential vault.
///
/// When an existing config is found, values are pre-filled as defaults
/// so the user can press Enter to keep them.
pub fn run_wizard(config_dir: &Path) -> Result<()> {
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

    println!();
    println!("  OpenCrust Setup Wizard");
    println!("  ----------------------");
    println!();

    // If config already exists, show summary and offer to keep it
    if let Some(ref cfg) = existing {
        println!("  Current configuration:");
        println!();
        print_config_summary(cfg);

        let choices = &["Keep current config (no changes)", "Reconfigure"];
        let selection = Select::new()
            .with_prompt("What would you like to do?")
            .items(choices)
            .default(0)
            .interact()
            .context("selection cancelled")?;

        if selection == 0 {
            println!();
            println!("  Config unchanged.");
            println!();
            return Ok(());
        }

        println!();
        println!("  Press Enter to keep current values.");
        println!();
    }

    // Extract existing values for pre-filling
    let existing_provider = existing
        .as_ref()
        .and_then(|c| c.llm.get("main"))
        .map(|p| p.provider.clone());
    let existing_prompt = existing
        .as_ref()
        .and_then(|c| c.agent.system_prompt.clone());
    let existing_has_key = existing
        .as_ref()
        .and_then(|c| c.llm.get("main"))
        .and_then(|p| p.api_key.as_ref())
        .is_some();

    // --- Provider selection ---
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

    // --- API key ---
    let env_hint = match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "sansa" => "SANSA_API_KEY",
        _ => "API_KEY",
    };

    let key_prompt = if existing_has_key {
        format!(
            "Enter your {provider} API key (Enter to keep existing, or set {env_hint} env var later)"
        )
    } else {
        format!("Enter your {provider} API key (or set {env_hint} env var later)")
    };

    let api_key: String = Password::new()
        .with_prompt(&key_prompt)
        .allow_empty_password(true)
        .interact()
        .context("API key input cancelled")?;

    let api_key = api_key.trim().to_string();

    // If user pressed Enter with existing key, preserve the old one
    let (api_key, kept_existing_key) = if api_key.is_empty() && existing_has_key {
        let old_key = existing
            .as_ref()
            .and_then(|c| c.llm.get("main"))
            .and_then(|p| p.api_key.clone())
            .unwrap_or_default();
        println!("  Keeping existing API key.");
        (old_key, true)
    } else {
        (api_key, false)
    };

    // --- Vault storage ---
    let store_in_vault = if !api_key.is_empty() && !kept_existing_key {
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
    } else if kept_existing_key {
        // Keep existing storage method - store in config like before
        1
    } else {
        2 // skip
    };

    // --- System prompt ---
    let default_prompt =
        existing_prompt.unwrap_or_else(|| "You are a helpful personal AI assistant.".to_string());
    let system_prompt: String = Input::new()
        .with_prompt("System prompt (optional)")
        .default(default_prompt)
        .allow_empty(true)
        .interact_text()
        .context("system prompt input cancelled")?;

    // --- Build config ---
    // Start from existing config to preserve channels, memory, mcp, etc.
    let mut config = existing.unwrap_or_default();

    let mut llm_config = LlmProviderConfig {
        provider: provider.to_string(),
        model: None,
        api_key: None,
        base_url: None,
        extra: Default::default(),
    };

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
                    vault.set(env_hint, &api_key);
                    vault.save().context("failed to save vault")?;
                    println!("  API key encrypted in vault.");
                    println!("  Set OPENCRUST_VAULT_PASSPHRASE env var for server mode.");
                }
                Err(e) => {
                    println!("  Warning: vault creation failed ({e}), storing in config instead.");
                    llm_config.api_key = Some(api_key.clone());
                }
            }
        }
        1 => {
            // Plaintext in config
            llm_config.api_key = Some(api_key.clone());
        }
        _ => {
            // Skip - user will use env var
            println!("  Set {env_hint} environment variable before starting the server.");
        }
    }

    // Update only the wizard-managed fields, preserve everything else
    config.llm.insert("main".to_string(), llm_config);
    config.agent.system_prompt = if system_prompt.is_empty() {
        None
    } else {
        Some(system_prompt)
    };

    let config_path = config_dir.join("config.yml");
    let yaml = serde_yaml::to_string(&config).context("failed to serialize config")?;
    std::fs::write(&config_path, &yaml)
        .context(format!("failed to write {}", config_path.display()))?;

    info!("config written to {}", config_path.display());
    println!();
    println!("  Config written to {}", config_path.display());
    println!("  Run `opencrust start` to launch the gateway.");
    println!();

    Ok(())
}
