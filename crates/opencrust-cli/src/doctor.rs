use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use opencrust_config::AppConfig;

// ---------------------------------------------------------------------------
// Result helpers
// ---------------------------------------------------------------------------

enum Check {
    Pass(String),
    Fail(String),
    Warn(String),
    Skip(String),
}

impl Check {
    fn print(&self, label: &str) {
        match self {
            Check::Pass(msg) => println!("  {} {label}: {msg}", "[pass]".green()),
            Check::Fail(msg) => println!("  {} {label}: {msg}", "[FAIL]".red().bold()),
            Check::Warn(msg) => println!("  {} {label}: {msg}", "[warn]".yellow()),
            Check::Skip(msg) => println!("  {} {label}: {msg}", "[skip]".dimmed()),
        }
    }

    fn is_fail(&self) -> bool {
        matches!(self, Check::Fail(_))
    }
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

fn check_config(config_dir: &Path) -> Check {
    let yaml = config_dir.join("config.yml");
    let toml = config_dir.join("config.toml");
    if yaml.exists() {
        Check::Pass(format!("config.yml found at {}", yaml.display()))
    } else if toml.exists() {
        Check::Pass(format!("config.toml found at {}", toml.display()))
    } else {
        Check::Warn(format!(
            "no config file found in {} — using defaults",
            config_dir.display()
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_data_dir(config: &AppConfig) -> std::path::PathBuf {
    config
        .data_dir
        .clone()
        .or_else(|| dirs::home_dir().map(|h| h.join(".opencrust").join("data")))
        .unwrap_or_else(|| ".opencrust/data".into())
}

fn check_data_dir(config: &AppConfig) -> Check {
    let data_dir = resolve_data_dir(config);

    if !data_dir.exists() {
        return Check::Warn(format!(
            "{} does not exist (will be created on first run)",
            data_dir.display()
        ));
    }

    // Probe writability with a temp file
    let probe = data_dir.join(".doctor_write_probe");
    match std::fs::write(&probe, b"probe") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Check::Pass(format!("{} exists and is writable", data_dir.display()))
        }
        Err(e) => Check::Fail(format!("{} is not writable: {e}", data_dir.display())),
    }
}

fn check_vault() -> Check {
    let vault_path = match dirs::home_dir() {
        Some(h) => h.join(".opencrust").join("vault.json"),
        None => return Check::Skip("cannot determine home directory".into()),
    };

    if !opencrust_security::CredentialVault::exists(&vault_path) {
        return Check::Skip("vault.json not found — vault not configured".into());
    }

    // Vault exists; verify it can be opened with OPENCRUST_VAULT_PASSPHRASE
    match std::env::var("OPENCRUST_VAULT_PASSPHRASE") {
        Ok(pass) => match opencrust_security::CredentialVault::open(&vault_path, &pass) {
            Ok(_) => Check::Pass("vault.json accessible".into()),
            Err(e) => Check::Fail(format!("vault.json exists but could not be opened: {e}")),
        },
        Err(_) => Check::Warn(
            "vault.json found but OPENCRUST_VAULT_PASSPHRASE is not set — skipping open test"
                .into(),
        ),
    }
}

async fn check_llm_providers(config: &AppConfig) -> Vec<(String, Check)> {
    if config.llm.is_empty() {
        return vec![(
            "LLM providers".into(),
            Check::Warn("no LLM providers configured".into()),
        )];
    }

    // build_agent_runtime is infallible and logs warnings for bad config entries.
    let runtime = opencrust_gateway::bootstrap::build_agent_runtime(config).await;

    match runtime.health_check_all().await {
        Ok(results) => {
            if results.is_empty() {
                return vec![(
                    "LLM providers".into(),
                    Check::Warn(
                        "no providers could be initialized — check API keys in config".into(),
                    ),
                )];
            }
            results
                .into_iter()
                .map(|(id, ok)| {
                    let check = if ok {
                        Check::Pass("reachable".into())
                    } else {
                        Check::Fail("health check failed — check API key and connectivity".into())
                    };
                    (format!("LLM provider [{id}]"), check)
                })
                .collect()
        }
        Err(e) => vec![(
            "LLM providers".into(),
            Check::Fail(format!("could not run health checks: {e}")),
        )],
    }
}

fn check_channels(config: &AppConfig) -> Vec<(String, Check)> {
    if config.channels.is_empty() {
        return vec![(
            "channels".into(),
            Check::Warn("no channels configured".into()),
        )];
    }

    config
        .channels
        .iter()
        .map(|(name, ch)| {
            let enabled = ch.enabled.unwrap_or(true);
            if !enabled {
                return (
                    format!("channel [{name}]"),
                    Check::Skip("disabled in config".into()),
                );
            }

            // Check for the known credential field(s) required per channel type.
            let required_keys: &[&str] = match ch.channel_type.as_str() {
                "telegram" => &["bot_token"],
                "discord" => &["bot_token"],
                "slack" => &["bot_token", "app_token"],
                "whatsapp" => &["access_token", "phone_number_id"],
                "line" => &["channel_access_token", "channel_secret"],
                "imessage" => &[],
                // Unknown type: fall back to any non-empty setting value.
                _ => &[],
            };

            let missing: Vec<&str> = if required_keys.is_empty() {
                // For unknown types or iMessage (no token needed), pass if any setting is set.
                if ch
                    .settings
                    .values()
                    .any(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false))
                    || ch.channel_type == "imessage"
                {
                    vec![]
                } else {
                    vec!["(any setting)"]
                }
            } else {
                required_keys
                    .iter()
                    .filter(|&&k| {
                        ch.settings
                            .get(k)
                            .and_then(|v| v.as_str())
                            .map(|s| s.is_empty())
                            .unwrap_or(true)
                    })
                    .copied()
                    .collect()
            };

            if missing.is_empty() {
                (
                    format!("channel [{name}]"),
                    Check::Pass(format!("type={}", ch.channel_type)),
                )
            } else {
                (
                    format!("channel [{name}]"),
                    Check::Warn(format!(
                        "type={} — missing or empty: {}",
                        ch.channel_type,
                        missing.join(", ")
                    )),
                )
            }
        })
        .collect()
}

async fn check_mcp_servers(config: &AppConfig) -> Vec<(String, Check)> {
    let loader = match opencrust_config::ConfigLoader::new() {
        Ok(l) => l,
        Err(e) => {
            return vec![(
                "MCP servers".into(),
                Check::Fail(format!("could not load MCP config: {e}")),
            )];
        }
    };
    let mcp_configs = loader.merged_mcp_config(config);

    if mcp_configs.is_empty() {
        return vec![(
            "MCP servers".into(),
            Check::Skip("no MCP servers configured".into()),
        )];
    }

    let mut results = Vec::new();
    for (name, server) in &mcp_configs {
        let enabled = server.enabled.unwrap_or(true);
        if !enabled {
            results.push((
                format!("MCP [{name}]"),
                Check::Skip("disabled in config".into()),
            ));
            continue;
        }

        let manager = opencrust_agents::McpManager::new();
        let timeout_secs = server.timeout.unwrap_or(5).min(10);
        let check = match server.transport.as_str() {
            "http" => match &server.url {
                Some(url) => match manager.connect_http(name, url, timeout_secs).await {
                    Ok(()) => {
                        let tools = manager.tool_info(name).await;
                        manager.disconnect(name).await;
                        Check::Pass(format!("connected via HTTP ({} tools)", tools.len()))
                    }
                    Err(e) => Check::Fail(format!("could not connect: {e}")),
                },
                None => Check::Fail("transport=http but no url configured".into()),
            },
            _ => match manager
                .connect(
                    name,
                    &server.command,
                    &server.args,
                    &server.env,
                    timeout_secs,
                )
                .await
            {
                Ok(()) => {
                    let tools = manager.tool_info(name).await;
                    manager.disconnect(name).await;
                    Check::Pass(format!("connected ({} tools)", tools.len()))
                }
                Err(e) => Check::Fail(format!("could not connect: {e}")),
            },
        };
        results.push((format!("MCP [{name}]"), check));
    }
    results
}

fn check_sqlite_integrity(db_path: &Path, label: &str) -> Check {
    if !db_path.exists() {
        return Check::Skip(format!(
            "{} not found — will be created on first run",
            db_path.display()
        ));
    }

    match opencrust_db::SessionStore::open(db_path) {
        Ok(store) => {
            let conn = store.connection().unwrap();
            match conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
                Ok(ref s) if s == "ok" => Check::Pass("integrity_check passed".into()),
                Ok(s) => Check::Fail(format!("integrity_check returned: {s}")),
                Err(e) => Check::Fail(format!("could not run integrity_check: {e}")),
            }
        }
        Err(e) => Check::Fail(format!("could not open {label}: {e}")),
    }
}

fn check_database(config: &AppConfig) -> (Check, Check) {
    let data_dir = resolve_data_dir(config);
    let sessions = check_sqlite_integrity(&data_dir.join("sessions.db"), "sessions.db");
    let memory = check_sqlite_integrity(&data_dir.join("memory.db"), "memory.db");
    (sessions, memory)
}

fn check_dna_md(config_dir: &Path) -> Check {
    let dna_path = config_dir.join("dna.md");
    if dna_path.exists() {
        Check::Pass(format!("found at {}", dna_path.display()))
    } else {
        Check::Warn(format!(
            "dna.md not found at {} — agent will use default personality",
            dna_path.display()
        ))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run all diagnostic checks and print a report.
/// Returns `true` if all checks passed (no failures), `false` otherwise.
pub async fn run_doctor(config: &AppConfig, config_dir: &Path) -> Result<bool> {
    println!("OpenCrust Doctor  v{}\n", env!("CARGO_PKG_VERSION"));

    let mut any_failed = false;

    macro_rules! report {
        ($label:expr, $check:expr) => {{
            let c = $check;
            if c.is_fail() {
                any_failed = true;
            }
            c.print($label);
        }};
    }

    // 1. Config file syntax
    report!("Config file", check_config(config_dir));

    // 2. Data directory
    report!("Data directory", check_data_dir(config));

    // 3. Credential vault
    report!("Credential vault", check_vault());

    // 4. LLM providers
    println!();
    for (label, check) in check_llm_providers(config).await {
        if check.is_fail() {
            any_failed = true;
        }
        check.print(&label);
    }

    // 5. Channels
    println!();
    for (label, check) in check_channels(config) {
        if check.is_fail() {
            any_failed = true;
        }
        check.print(&label);
    }

    // 6. MCP servers
    println!();
    for (label, check) in check_mcp_servers(config).await {
        if check.is_fail() {
            any_failed = true;
        }
        check.print(&label);
    }

    // 7. Database integrity
    println!();
    let (sessions_check, memory_check) = check_database(config);
    report!("Database (sessions.db)", sessions_check);
    report!("Database (memory.db)", memory_check);

    // 8. dna.md
    report!("dna.md", check_dna_md(config_dir));

    // Summary
    println!();
    if any_failed {
        println!(
            "Result: {} — review {} items above.",
            "issues found".red().bold(),
            "[FAIL]".red().bold()
        );
    } else {
        println!("Result: {}", "all checks passed.".green().bold());
    }

    Ok(!any_failed)
}
