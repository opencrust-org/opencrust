mod migrate;
mod wizard;

use std::path::PathBuf;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "opencrust",
    version,
    about = "OpenCrust - Personal AI Assistant"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info", global = true)]
    log_level: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the gateway server
    Start {
        /// Host to bind to
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Port to listen on
        #[arg(long, default_value = "3000")]
        port: u16,

        /// Run as a background daemon
        #[arg(long, short = 'd')]
        daemon: bool,
    },

    /// Stop the running daemon
    Stop,

    /// Show current status
    Status,

    /// Run the onboarding wizard
    Init,

    /// Manage channels
    Channel {
        #[command(subcommand)]
        action: ChannelCommands,
    },

    /// Manage plugins
    Plugin {
        #[command(subcommand)]
        action: PluginCommands,
    },

    /// Manage skills
    Skill {
        #[command(subcommand)]
        action: SkillCommands,
    },

    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        action: McpCommands,
    },

    /// Migrate data from other platforms
    Migrate {
        #[command(subcommand)]
        action: MigrateCommands,
    },
}

#[derive(Subcommand)]
enum ChannelCommands {
    /// List configured channels
    List,
    /// Show channel status
    Status { name: String },
}

#[derive(Subcommand)]
enum PluginCommands {
    /// List installed plugins
    List,
    /// Install a plugin
    Install { path: String },
    /// Remove a plugin
    Remove { name: String },
    /// Watch plugin directory and hot-reload on change
    Watch,
}

#[derive(Subcommand)]
enum SkillCommands {
    /// List installed skills
    List,
    /// Install a skill from a URL
    Install { url: String },
    /// Remove a skill by name
    Remove { name: String },
}

#[derive(Subcommand)]
enum McpCommands {
    /// List configured MCP servers
    List,
    /// Connect to an MCP server and list its tools
    Inspect { name: String },
}

#[derive(Subcommand)]
enum MigrateCommands {
    /// Import data from OpenClaw
    Openclaw {
        /// Preview changes without importing
        #[arg(long)]
        dry_run: bool,

        /// Path to OpenClaw config directory (default: ~/.config/openclaw/)
        #[arg(long)]
        source: Option<String>,
    },
}

fn opencrust_dir() -> PathBuf {
    opencrust_config::ConfigLoader::default_config_dir()
}

fn pid_file_path() -> PathBuf {
    opencrust_dir().join("opencrust.pid")
}

#[cfg(unix)]
fn log_file_path() -> PathBuf {
    opencrust_dir().join("opencrust.log")
}

/// Read the PID from the PID file.
fn read_pid() -> Option<u32> {
    let path = pid_file_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Check if a process with the given PID is running.
#[cfg(unix)]
fn is_process_running(pid: u32) -> bool {
    // Signal 0 checks existence without sending a signal
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn is_process_running(_pid: u32) -> bool {
    false
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Init tracing for non-daemon mode (daemon reconfigures after fork)
    let init_tracing = |level: &str| {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level)),
            )
            .init();
    };

    let config_loader = opencrust_config::ConfigLoader::new()?;
    config_loader.ensure_dirs()?;
    let config = config_loader.load()?;

    match cli.command {
        Commands::Start { host, port, daemon } => {
            let mut config = config;
            config.gateway.host = host;
            config.gateway.port = port;

            if daemon {
                start_daemon(config)?;
            } else {
                init_tracing(&cli.log_level);
                let server = opencrust_gateway::GatewayServer::new(config);
                server.run().await?;
            }
        }
        Commands::Stop => {
            init_tracing(&cli.log_level);
            stop_daemon()?;
        }
        Commands::Status => {
            init_tracing(&cli.log_level);

            // Check PID file first
            if let Some(pid) = read_pid() {
                if is_process_running(pid) {
                    println!("OpenCrust daemon is running (PID {})", pid);
                } else {
                    println!(
                        "OpenCrust daemon is not running (stale PID file for PID {})",
                        pid
                    );
                    // Clean up stale PID file
                    let _ = std::fs::remove_file(pid_file_path());
                }
            } else {
                println!("No daemon PID file found.");
            }

            // Also try the HTTP status endpoint
            println!();
            println!("Gateway status:");
            let client = reqwest::Client::new();
            match client
                .get(format!(
                    "http://{}:{}/api/status",
                    config.gateway.host, config.gateway.port
                ))
                .send()
                .await
            {
                Ok(resp) => {
                    let body = resp.json::<serde_json::Value>().await?;
                    println!("{}", serde_json::to_string_pretty(&body)?);
                }
                Err(_) => {
                    println!("Gateway is not responding.");
                }
            }
        }
        Commands::Init => {
            init_tracing(&cli.log_level);
            wizard::run_wizard(config_loader.config_dir())?;
        }
        Commands::Channel { action } => {
            init_tracing(&cli.log_level);
            match action {
                ChannelCommands::List => {
                    println!("Configured channels:");
                    if config.channels.is_empty() {
                        println!("  (none - add channels to config.yml)");
                    }
                    for (name, ch) in &config.channels {
                        let enabled = ch.enabled.unwrap_or(true);
                        let status = if enabled { "enabled" } else { "disabled" };
                        println!("  {} [{}] - {}", name, ch.channel_type, status);
                    }
                }
                ChannelCommands::Status { name } => match config.channels.get(&name) {
                    Some(ch) => println!(
                        "{}: type={}, enabled={}",
                        name,
                        ch.channel_type,
                        ch.enabled.unwrap_or(true)
                    ),
                    None => println!("channel '{}' not found in config", name),
                },
            }
        }
        Commands::Plugin { action } => {
            init_tracing(&cli.log_level);
            match action {
                PluginCommands::List => {
                    let loader = opencrust_plugins::PluginLoader::new(
                        config_loader.config_dir().join("plugins"),
                    );
                    match loader.discover() {
                        Ok(plugins) => {
                            println!("Installed plugins:");
                            if plugins.is_empty() {
                                println!("  (none)");
                            }
                            for p in plugins {
                                let caps = p
                                    .capabilities()
                                    .iter()
                                    .map(|c| format!("{:?}", c))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                println!("  {} - {} [caps: {}]", p.name(), p.description(), caps);
                            }
                        }
                        Err(e) => println!("error scanning plugins: {}", e),
                    }
                }
                PluginCommands::Install { path } => {
                    let source_path = PathBuf::from(path);
                    if !source_path.exists() {
                        println!("Source path not found: {}", source_path.display());
                        return Ok(());
                    }

                    let manifest_path = if source_path.is_dir() {
                        source_path.join("plugin.toml")
                    } else if source_path.file_name().unwrap_or_default() == "plugin.toml" {
                        source_path.clone()
                    } else {
                        println!(
                            "Install expects a directory with plugin.toml or path to plugin.toml"
                        );
                        return Ok(());
                    };

                    if !manifest_path.exists() {
                        println!("plugin.toml not found at {}", manifest_path.display());
                        return Ok(());
                    }

                    let manifest =
                        match opencrust_plugins::PluginManifest::from_file(&manifest_path) {
                            Ok(m) => m,
                            Err(e) => {
                                println!("Invalid manifest: {}", e);
                                return Ok(());
                            }
                        };

                    let plugins_dir = config_loader.config_dir().join("plugins");
                    let target_dir = plugins_dir.join(&manifest.plugin.name);

                    if target_dir.exists() {
                        println!(
                            "Plugin '{}' already installed. Use remove first.",
                            manifest.plugin.name
                        );
                        return Ok(());
                    }

                    std::fs::create_dir_all(&target_dir)?;

                    std::fs::copy(&manifest_path, target_dir.join("plugin.toml"))?;

                    let source_dir = manifest_path.parent().unwrap();
                    let wasm_name = format!("{}.wasm", manifest.plugin.name);
                    let wasm_source = source_dir.join(&wasm_name);
                    let wasm_generic = source_dir.join("plugin.wasm");

                    if wasm_source.exists() {
                        std::fs::copy(&wasm_source, target_dir.join(&wasm_name))?;
                    } else if wasm_generic.exists() {
                        std::fs::copy(&wasm_generic, target_dir.join("plugin.wasm"))?;
                    } else {
                        println!(
                            "Warning: WASM file not found in source directory. Copied only manifest."
                        );
                    }

                    println!("Installed plugin: {}", manifest.plugin.name);
                }
                PluginCommands::Remove { name } => {
                    let plugins_dir = config_loader.config_dir().join("plugins");
                    let target_dir = plugins_dir.join(&name);

                    if !target_dir.exists() {
                        println!("Plugin '{}' not found.", name);
                        return Ok(());
                    }

                    std::fs::remove_dir_all(&target_dir)?;
                    println!("Removed plugin: {}", name);
                }
                PluginCommands::Watch => {
                    let plugins_dir = config_loader.config_dir().join("plugins");
                    std::fs::create_dir_all(&plugins_dir)?;

                    let mut registry = opencrust_plugins::PluginRegistry::from_dir(&plugins_dir);
                    let count = registry.reload()?;
                    println!(
                        "Watching plugins directory: {}",
                        plugins_dir.as_path().display()
                    );
                    println!("Loaded {} plugin(s). Press Ctrl+C to stop.", count);

                    registry.start_hot_reload()?;
                    tokio::signal::ctrl_c().await?;
                    println!("Stopped plugin watcher.");
                }
            }
        }
        Commands::Skill { action } => {
            init_tracing(&cli.log_level);
            let skills_dir = config_loader.config_dir().join("skills");
            match action {
                SkillCommands::List => {
                    let scanner = opencrust_skills::SkillScanner::new(&skills_dir);
                    match scanner.discover() {
                        Ok(skills) => {
                            println!("Installed skills:");
                            if skills.is_empty() {
                                println!("  (none)");
                            }
                            for s in skills {
                                let triggers = if s.frontmatter.triggers.is_empty() {
                                    String::new()
                                } else {
                                    format!(" [triggers: {}]", s.frontmatter.triggers.join(", "))
                                };
                                println!(
                                    "  {} - {}{}",
                                    s.frontmatter.name, s.frontmatter.description, triggers
                                );
                            }
                        }
                        Err(e) => println!("error scanning skills: {}", e),
                    }
                }
                SkillCommands::Install { url } => {
                    let installer = opencrust_skills::SkillInstaller::new(&skills_dir);
                    match installer.install_from_url(&url).await {
                        Ok(skill) => println!("installed skill: {}", skill.frontmatter.name),
                        Err(e) => println!("error installing skill: {}", e),
                    }
                }
                SkillCommands::Remove { name } => {
                    let installer = opencrust_skills::SkillInstaller::new(&skills_dir);
                    match installer.remove(&name) {
                        Ok(true) => println!("removed skill: {}", name),
                        Ok(false) => println!("skill '{}' not found", name),
                        Err(e) => println!("error removing skill: {}", e),
                    }
                }
            }
        }
        Commands::Mcp { action } => {
            init_tracing(&cli.log_level);
            let loader = opencrust_config::ConfigLoader::new()?;
            let mcp_configs = loader.merged_mcp_config(&config);
            match action {
                McpCommands::List => {
                    println!("Configured MCP servers:");
                    if mcp_configs.is_empty() {
                        println!("  (none — add servers to config.yml or ~/.opencrust/mcp.json)");
                    }
                    for (name, server) in &mcp_configs {
                        let enabled = server.enabled.unwrap_or(true);
                        let status = if enabled { "enabled" } else { "disabled" };
                        println!(
                            "  {} [{}] {} {:?} (timeout: {}s)",
                            name,
                            status,
                            server.command,
                            server.args,
                            server.timeout.unwrap_or(30),
                        );
                    }
                }
                McpCommands::Inspect { name } => {
                    let Some(server_config) = mcp_configs.get(&name) else {
                        println!("MCP server '{}' not found in config", name);
                        return Ok(());
                    };

                    println!("Connecting to MCP server '{name}'...");
                    let manager = opencrust_agents::McpManager::new();
                    let timeout_secs = server_config.timeout.unwrap_or(30);

                    match manager
                        .connect(
                            &name,
                            &server_config.command,
                            &server_config.args,
                            &server_config.env,
                            timeout_secs,
                        )
                        .await
                    {
                        Ok(()) => {
                            let tools = manager.tool_info(&name).await;
                            println!("Tools from '{name}' ({} total):", tools.len());
                            for tool in &tools {
                                let desc =
                                    tool.description.as_deref().unwrap_or("(no description)");
                                println!("  {name}.{}", tool.name);
                                println!("    {desc}");
                            }
                            manager.disconnect(&name).await;
                        }
                        Err(e) => {
                            println!("Failed to connect: {e}");
                        }
                    }
                }
            }
        }
        Commands::Migrate { action } => {
            init_tracing(&cli.log_level);
            match action {
                MigrateCommands::Openclaw { dry_run, source } => {
                    let opencrust_dir = config_loader.config_dir().to_path_buf();
                    match migrate::migrate_openclaw(source.as_deref(), dry_run, &opencrust_dir) {
                        Ok(report) => report.print_summary(),
                        Err(e) => println!("migration failed: {}", e),
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
fn start_daemon(config: opencrust_config::AppConfig) -> Result<()> {
    use daemonize::Daemonize;
    use std::fs::File;

    let pid_path = pid_file_path();
    let log_path = log_file_path();

    let stdout = File::create(&log_path)
        .context(format!("failed to create log file: {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .context("failed to clone log file handle")?;

    let daemonize = Daemonize::new()
        .pid_file(&pid_path)
        .stdout(stdout)
        .stderr(stderr)
        .working_directory(".");

    match daemonize.start() {
        Ok(()) => {
            // We are now in the child (daemon) process.
            // Re-init tracing to write to the log file.
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::new(
                    config.log_level.as_deref().unwrap_or("info"),
                ))
                .with_ansi(false)
                .init();

            tracing::info!("daemon started (PID file: {})", pid_path.display());

            // Build a new tokio runtime in the daemon process
            let rt = tokio::runtime::Runtime::new()
                .context("failed to create tokio runtime in daemon")?;
            rt.block_on(async {
                let server = opencrust_gateway::GatewayServer::new(config);
                if let Err(e) = server.run().await {
                    tracing::error!("gateway error: {e}");
                }
            });

            // Clean up PID file on exit
            let _ = std::fs::remove_file(&pid_path);
            Ok(())
        }
        Err(e) => {
            anyhow::bail!("failed to daemonize: {e}");
        }
    }
}

#[cfg(not(unix))]
fn start_daemon(_config: opencrust_config::AppConfig) -> Result<()> {
    anyhow::bail!("daemonization is only supported on Unix systems. Run without --daemon.");
}

#[cfg(unix)]
fn stop_daemon() -> Result<()> {
    let pid_path = pid_file_path();

    let pid = read_pid().context(format!("no PID file found at {}", pid_path.display()))?;

    if !is_process_running(pid) {
        println!("Process {} is not running (removing stale PID file)", pid);
        std::fs::remove_file(&pid_path).ok();
        return Ok(());
    }

    println!("Sending SIGTERM to PID {}...", pid);
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }

    // Wait briefly for the process to exit
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if !is_process_running(pid) {
            println!("OpenCrust daemon stopped.");
            std::fs::remove_file(&pid_path).ok();
            return Ok(());
        }
    }

    println!(
        "Process {} did not exit within 5s. It may still be shutting down.",
        pid
    );
    Ok(())
}

#[cfg(not(unix))]
fn stop_daemon() -> Result<()> {
    anyhow::bail!("daemon stop is only supported on Unix systems");
}
