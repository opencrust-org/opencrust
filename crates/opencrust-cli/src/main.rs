mod banner;
mod chat;
mod doctor;
mod mcp_registry;
mod migrate;
mod update;
mod wizard;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use opencrust_security::RedactingWriter;
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
        #[arg(long, default_value = "3888")]
        port: u16,

        /// Run as a background daemon
        #[arg(long, short = 'd')]
        daemon: bool,

        /// Show debug info in responses (tool calls, RAG scores, provider)
        #[arg(long)]
        debug: bool,
    },

    /// Stop the running daemon
    Stop,

    /// Restart the daemon (stop if running, then start)
    Restart {
        /// Host to bind to
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Port to listen on
        #[arg(long, default_value = "3888")]
        port: u16,

        /// Run as a background daemon
        #[arg(long, short = 'd')]
        daemon: bool,
    },

    /// Show current status
    Status,

    /// Run the onboarding wizard
    Init,

    /// Interactive terminal chat with the gateway
    Chat {
        /// Gateway URL
        #[arg(long, default_value = "http://127.0.0.1:3888")]
        url: String,

        /// Named agent to use (defaults to gateway default)
        #[arg(long)]
        agent: Option<String>,
    },

    /// Manage channels
    Channel {
        #[command(subcommand)]
        action: ChannelCommands,
    },

    /// Manage plugins (requires --features plugins)
    #[cfg(feature = "plugins")]
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

    /// Manage ingested documents (RAG)
    Doc {
        #[command(subcommand)]
        action: DocCommands,
    },

    /// Run diagnostic checks on the current setup
    Doctor,

    /// Update to the latest release
    Update {
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Roll back to the previous version
    Rollback,

    /// Uninstall OpenCrust (remove binary and data)
    Uninstall {
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,

        /// Keep config and data, only remove the binary
        #[arg(long)]
        keep_data: bool,
    },
}

#[derive(Subcommand)]
enum ChannelCommands {
    /// List configured channels
    List,
    /// Show channel status
    Status { name: String },
}

#[cfg(feature = "plugins")]
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
    /// Install a skill from a URL or local file path
    Install { source: String },
    /// Remove a skill by name
    Remove { name: String },
}

#[derive(Subcommand)]
enum McpCommands {
    /// List configured MCP servers
    List,
    /// Add a new MCP server (interactive wizard)
    Add {
        /// Server name or registry ID (skip selection prompt)
        name: Option<String>,
    },
    /// Remove an MCP server from configuration
    Remove {
        /// Name of the server to remove
        name: String,
    },
    /// Connect to an MCP server and list its tools
    Inspect { name: String },
    /// List resources from a connected MCP server
    Resources { name: String },
    /// List prompts from a connected MCP server
    Prompts { name: String },
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

#[derive(Subcommand)]
enum DocCommands {
    /// Ingest a document for RAG search
    Add {
        /// File path or directory to ingest
        path: String,
    },
    /// Batch-ingest all supported documents in a directory (recursive)
    Ingest {
        /// Directory to walk and ingest (.md, .txt, .pdf, .html, .htm)
        path: String,

        /// Re-ingest documents that are already in the store (removes old copy first)
        #[arg(long)]
        replace: bool,
    },
    /// List ingested documents
    List,
    /// Remove an ingested document
    Remove {
        /// Document name to remove
        name: String,
    },
}

/// Build an embedding provider from config for document ingestion.
fn build_embedding_provider(
    config: &opencrust_config::AppConfig,
) -> Option<Box<dyn opencrust_agents::EmbeddingProvider>> {
    let embed_name = config.memory.embedding_provider.as_ref()?;
    let embed_config = config.embeddings.get(embed_name)?;

    match embed_config.provider.as_str() {
        "cohere" => {
            // Resolve API key: config -> vault -> env
            let api_key = embed_config
                .api_key
                .clone()
                .or_else(|| {
                    let vault_path = opencrust_config::ConfigLoader::default_config_dir()
                        .join("credentials")
                        .join("vault.json");
                    opencrust_security::try_vault_get(&vault_path, "COHERE_API_KEY")
                })
                .or_else(|| std::env::var("COHERE_API_KEY").ok())?;

            Some(Box::new(opencrust_agents::CohereEmbeddingProvider::new(
                api_key,
                embed_config.model.clone(),
                embed_config.base_url.clone(),
            )))
        }
        "ollama" => Some(Box::new(opencrust_agents::OllamaEmbeddingProvider::new(
            embed_config.model.clone(),
            embed_config.base_url.clone(),
        ))),
        _ => None,
    }
}

/// Counts returned by [`run_ingest`].
#[derive(Debug, Default, PartialEq)]
pub struct IngestSummary {
    pub ingested: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Walk `dir` recursively and ingest every `.md / .txt / .pdf / .html / .htm`
/// file into `store`.  Returns a summary of what happened.
///
/// * `replace` – if `true`, remove an existing copy before re-ingesting.
/// * `embedding_provider` – optional provider; when `None` files are stored
///   without vector embeddings and fall back to keyword search.
pub async fn run_ingest(
    store: &opencrust_db::DocumentStore,
    dir: &std::path::Path,
    replace: bool,
    embedding_provider: Option<&dyn opencrust_agents::EmbeddingProvider>,
) -> anyhow::Result<IngestSummary> {
    use std::io::Write as _;

    if !dir.is_dir() {
        anyhow::bail!(
            "'{}' is not a directory. Use `opencrust doc add` for single files.",
            dir.display()
        );
    }

    const SUPPORTED: &[&str] = &["md", "txt", "pdf", "html", "htm"];

    let files: Vec<_> = walkdir::WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| SUPPORTED.contains(&ext.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();

    if files.is_empty() {
        println!("No supported files found in '{}'.", dir.display());
        println!("Supported extensions: .md, .txt, .pdf, .html, .htm");
        return Ok(IngestSummary::default());
    }

    println!("Found {} file(s) to process...", files.len());

    let mut summary = IngestSummary::default();

    for entry in &files {
        let file_path = entry.path();
        let file_name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| file_path.to_string_lossy().to_string());

        // Handle already-ingested documents
        if store.get_document_by_name(&file_name)?.is_some() {
            if replace {
                store.remove_document(&file_name)?;
            } else {
                println!("  skip    {file_name} (already ingested, use --replace to re-ingest)");
                summary.skipped += 1;
                continue;
            }
        }

        // Extract text
        let text = match opencrust_media::extract_text(file_path) {
            Ok(t) => t,
            Err(e) => {
                println!("  fail    {file_name} ({e})");
                summary.failed += 1;
                continue;
            }
        };

        if text.trim().is_empty() {
            println!("  skip    {file_name} (no text content)");
            summary.skipped += 1;
            continue;
        }

        let mime = opencrust_media::detect_mime_type(file_path);
        let chunks = opencrust_media::chunk_text(&text, &opencrust_media::ChunkOptions::default());

        print!("  ingest  {file_name} ({} chunks)...", chunks.len());
        let _ = std::io::stdout().flush();

        let doc_id = match store.add_document(&file_name, Some(&file_path.to_string_lossy()), mime)
        {
            Ok(id) => id,
            Err(e) => {
                println!(" fail ({e})");
                summary.failed += 1;
                continue;
            }
        };

        let mut chunk_error = false;
        let mut warned_embed = false;
        for chunk in &chunks {
            let embedding = if let Some(provider) = embedding_provider {
                match provider
                    .embed_documents(std::slice::from_ref(&chunk.text))
                    .await
                {
                    Ok(mut vecs) if !vecs.is_empty() => Some(vecs.remove(0)),
                    Ok(_) => None,
                    Err(e) => {
                        if !warned_embed {
                            println!(
                                "\n  Warning: embedding failed ({e}), storing without vectors."
                            );
                            warned_embed = true;
                        }
                        None
                    }
                }
            } else {
                None
            };

            let model = embedding_provider.map(|p| p.model().to_string());
            let dims = embedding.as_ref().map(|e| e.len());

            if let Err(e) = store.add_chunk(
                &doc_id,
                chunk.index,
                &chunk.text,
                embedding.as_deref(),
                model.as_deref(),
                dims,
                Some(chunk.token_count),
            ) {
                println!(" fail ({e})");
                let _ = store.remove_document(&file_name);
                chunk_error = true;
                break;
            }
        }

        if chunk_error {
            summary.failed += 1;
            continue;
        }

        store.update_chunk_count(&doc_id, chunks.len())?;
        println!(" done");
        summary.ingested += 1;
    }

    Ok(summary)
}

#[cfg(any(feature = "plugins", test))]
fn validate_plugin_path(path_str: &str) -> Result<PathBuf> {
    if path_str.trim().is_empty() {
        anyhow::bail!("Path cannot be empty");
    }
    let path = PathBuf::from(path_str);
    // On Windows, we might want to ensure it's absolute or canonicalized,
    // but usually PathBuf handles relative paths fine.
    // We just return it as is, wrapped in Result for consistency and future checks.
    Ok(path)
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

#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if handle == 0 {
            return false;
        }

        let mut exit_code = 0;
        let result = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);

        if result == 0 {
            return false;
        }

        exit_code == STILL_ACTIVE as u32
    }
}

#[cfg(not(any(unix, windows)))]
fn is_process_running(_pid: u32) -> bool {
    false
}

/// Find the PID of a process listening on the given port.
#[cfg(unix)]
fn find_pid_on_port(port: u16) -> Option<u32> {
    let output = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{port}")])
        .output()
        .ok()?;
    let own_pid = std::process::id();
    // lsof may return multiple PIDs (e.g. browser clients connected to the port).
    // Filter out our own PID and return all candidates so the caller can kill them.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .find(|&pid| pid != own_pid)
}

/// Find all PIDs listening on / connected to the given port (excluding our own).
#[cfg(unix)]
fn find_pids_on_port(port: u16) -> Vec<u32> {
    let Ok(output) = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{port}")])
        .output()
    else {
        return vec![];
    };
    let own_pid = std::process::id();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .filter(|&pid| pid != own_pid)
        .collect()
}

/// Find the PID of a process listening on the given port (Windows: uses netstat).
#[cfg(windows)]
fn find_pid_on_port(port: u16) -> Option<u32> {
    let output = std::process::Command::new("netstat")
        .args(["-ano"])
        .output()
        .ok()?;
    let own_pid = std::process::id();
    let needle = format!(":{port}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(&needle) && line.contains("LISTENING"))
        .filter_map(|line| line.split_whitespace().last()?.parse::<u32>().ok())
        .find(|&pid| pid != own_pid)
}

/// Find all PIDs listening on the given port (Windows: uses netstat).
#[cfg(windows)]
fn find_pids_on_port(port: u16) -> Vec<u32> {
    let Ok(output) = std::process::Command::new("netstat")
        .args(["-ano"])
        .output()
    else {
        return vec![];
    };
    let own_pid = std::process::id();
    let needle = format!(":{port}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(&needle) && line.contains("LISTENING"))
        .filter_map(|line| line.split_whitespace().last()?.parse::<u32>().ok())
        .filter(|&pid| pid != own_pid)
        .collect()
}

/// Send SIGTERM and wait up to 5s for the process to exit. Returns true if it exited.
#[cfg(unix)]
fn kill_and_wait(pid: u32) -> bool {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if !is_process_running(pid) {
            return true;
        }
    }
    false
}

/// Terminate a process by PID (Windows: uses taskkill).
#[cfg(windows)]
fn kill_and_wait(pid: u32) -> bool {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if !is_process_running(pid) {
            return true;
        }
    }
    false
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Show update notice (non-blocking, from cache)
    if !matches!(cli.command, Commands::Update { .. })
        && let Some(notice) = update::check_for_update_notice()
    {
        eprintln!("{notice}");
        eprintln!();
    }

    // Init tracing for non-daemon mode (daemon reconfigures after fork)
    let init_tracing = |level: &str| {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level)),
            )
            .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
            .with_writer(RedactingWriter::stderr())
            .init();
    };

    let config_loader = opencrust_config::ConfigLoader::new()?;
    config_loader.ensure_dirs()?;
    let config = config_loader.load()?;

    // Handle daemon mode BEFORE creating the tokio runtime. The fork must
    // happen before any async runtime is initialised, otherwise the child
    // inherits stale kqueue/epoll FDs and spawned child processes fail
    // with "Bad file descriptor".
    let is_daemon = matches!(
        &cli.command,
        Commands::Start { daemon: true, .. } | Commands::Restart { daemon: true, .. }
    );
    if is_daemon {
        let is_restart = matches!(&cli.command, Commands::Restart { .. });
        let (host, port) = match &cli.command {
            Commands::Start { host, port, .. } | Commands::Restart { host, port, .. } => {
                (host.clone(), *port)
            }
            _ => unreachable!(),
        };
        let mut config = config;
        config.gateway.host = host;
        config.gateway.port = port;
        if is_restart {
            // Don't init tracing here — try_stop_daemon uses println!,
            // and the daemon child will init its own subscriber after fork.
            try_stop_daemon(config.gateway.port);
        }
        return start_daemon(config);
    }

    // All other commands run inside a tokio runtime.
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    rt.block_on(async_main(cli, config, config_loader, init_tracing))
}

async fn async_main(
    cli: Cli,
    config: opencrust_config::AppConfig,
    config_loader: opencrust_config::ConfigLoader,
    init_tracing: impl Fn(&str),
) -> Result<()> {
    match cli.command {
        Commands::Start {
            host, port, debug, ..
        } => {
            let mut config = config;
            config.gateway.host = host.clone();
            config.gateway.port = port;
            config.debug = debug;
            init_tracing(&cli.log_level);

            // If no config file exists and we're in a terminal, offer to run the wizard
            if !config_loader.config_file_exists() && std::io::stdin().is_terminal() {
                println!();
                println!("  No config file found.");
                println!();
                let choices = &[
                    "Run setup wizard (recommended)",
                    "Start with defaults (no LLM provider)",
                ];
                let selection = dialoguer::Select::new()
                    .with_prompt("How would you like to proceed?")
                    .items(choices)
                    .default(0)
                    .interact()
                    .context("selection cancelled")?;

                if selection == 0 {
                    wizard::run_wizard(config_loader.config_dir()).await?;
                    // Reload config after wizard
                    config = config_loader.load()?;
                    config.gateway.host = host;
                    config.gateway.port = port;
                }
            }

            banner::print_banner(
                &config.gateway.host,
                config.gateway.port,
                &config,
                config_loader.config_dir(),
            );
            update::spawn_background_check();
            let server = opencrust_gateway::GatewayServer::new(config);
            server.run().await?;
        }
        Commands::Stop => {
            init_tracing(&cli.log_level);
            stop_daemon(config.gateway.port)?;
        }
        Commands::Restart { host, port, .. } => {
            init_tracing(&cli.log_level);
            try_stop_daemon(port);
            let mut config = config;
            config.gateway.host = host;
            config.gateway.port = port;
            banner::print_banner(
                &config.gateway.host,
                config.gateway.port,
                &config,
                config_loader.config_dir(),
            );
            update::spawn_background_check();
            let server = opencrust_gateway::GatewayServer::new(config);
            server.run().await?;
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
            wizard::run_wizard(config_loader.config_dir()).await?;
        }
        Commands::Chat { url, agent } => {
            chat::run(url, agent).await?;
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
        #[cfg(feature = "plugins")]
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
                    let source_path = match validate_plugin_path(&path) {
                        Ok(p) => p,
                        Err(e) => {
                            println!("Invalid path: {}", e);
                            return Ok(());
                        }
                    };

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
                SkillCommands::Install { source } => {
                    let installer = opencrust_skills::SkillInstaller::new(&skills_dir);
                    if source.starts_with("http://") || source.starts_with("https://") {
                        match installer.install_from_url(&source).await {
                            Ok(skill) => println!("installed skill: {}", skill.frontmatter.name),
                            Err(e) => println!("error installing skill: {}", e),
                        }
                    } else {
                        match installer.install_from_path(std::path::Path::new(&source)) {
                            Ok(skill) => println!("installed skill: {}", skill.frontmatter.name),
                            Err(e) => println!("error installing skill: {}", e),
                        }
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
                McpCommands::Add { name } => {
                    wizard::run_mcp_add_wizard(config_loader.config_dir(), name.as_deref()).await?;
                }
                McpCommands::Remove { name } => {
                    wizard::run_mcp_remove(config_loader.config_dir(), &config, &name)?;
                }
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
                McpCommands::Resources { name } => {
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
                        Ok(()) => match manager.list_resources(&name).await {
                            Ok(resources) => {
                                println!("Resources from '{name}' ({} total):", resources.len());
                                for r in &resources {
                                    let desc =
                                        r.description.as_deref().unwrap_or("(no description)");
                                    let mime = r
                                        .mime_type
                                        .as_deref()
                                        .map(|m| format!(" [{m}]"))
                                        .unwrap_or_default();
                                    println!("  {}{}", r.uri, mime);
                                    println!("    {} — {desc}", r.name);
                                }
                            }
                            Err(e) => println!("Failed to list resources: {e}"),
                        },
                        Err(e) => println!("Failed to connect: {e}"),
                    }
                    manager.disconnect(&name).await;
                }
                McpCommands::Prompts { name } => {
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
                        Ok(()) => match manager.list_prompts(&name).await {
                            Ok(prompts) => {
                                println!("Prompts from '{name}' ({} total):", prompts.len());
                                for p in &prompts {
                                    let desc =
                                        p.description.as_deref().unwrap_or("(no description)");
                                    println!("  {}", p.name);
                                    println!("    {desc}");
                                    for arg in &p.arguments {
                                        let req = if arg.required { " (required)" } else { "" };
                                        let arg_desc = arg
                                            .description
                                            .as_deref()
                                            .unwrap_or("(no description)");
                                        println!("    - {}{}: {}", arg.name, req, arg_desc);
                                    }
                                }
                            }
                            Err(e) => println!("Failed to list prompts: {e}"),
                        },
                        Err(e) => println!("Failed to connect: {e}"),
                    }
                    manager.disconnect(&name).await;
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
        Commands::Doc { action } => {
            init_tracing(&cli.log_level);
            let data_dir = config.data_dir.clone().unwrap_or_else(|| {
                opencrust_config::ConfigLoader::default_config_dir().join("data")
            });
            std::fs::create_dir_all(&data_dir).ok();
            let memory_db_path = data_dir.join("memory.db");

            let doc_store = opencrust_db::DocumentStore::open(&memory_db_path)
                .context("failed to open document store")?;

            match action {
                DocCommands::Add { path } => {
                    let file_path = std::path::Path::new(&path);
                    if !file_path.exists() {
                        println!("File not found: {path}");
                        return Ok(());
                    }

                    // Extract text
                    let text = match opencrust_media::extract_text(file_path) {
                        Ok(t) => t,
                        Err(e) => {
                            println!("Failed to extract text: {e}");
                            return Ok(());
                        }
                    };

                    if text.trim().is_empty() {
                        println!("No text content found in {path}");
                        return Ok(());
                    }

                    let file_name = file_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.clone());

                    // Check for duplicate
                    if doc_store.get_document_by_name(&file_name)?.is_some() {
                        println!(
                            "Document '{file_name}' already ingested. Remove it first with `opencrust doc remove {file_name}`."
                        );
                        return Ok(());
                    }

                    let mime = opencrust_media::detect_mime_type(file_path);

                    // Chunk the text
                    let chunks = opencrust_media::chunk_text(
                        &text,
                        &opencrust_media::ChunkOptions::default(),
                    );

                    println!("Ingesting '{file_name}' ({} chunks)...", chunks.len());

                    // Add document
                    let doc_id = doc_store.add_document(&file_name, Some(&path), mime)?;

                    // Build embedding provider if available
                    let embedding_provider = build_embedding_provider(&config);

                    // Add chunks with embeddings
                    for chunk in &chunks {
                        let embedding = if let Some(ref provider) = embedding_provider {
                            match provider
                                .embed_documents(std::slice::from_ref(&chunk.text))
                                .await
                            {
                                Ok(mut vecs) if !vecs.is_empty() => Some(vecs.remove(0)),
                                Ok(_) => None,
                                Err(e) => {
                                    if chunk.index == 0 {
                                        println!(
                                            "  Warning: embedding failed ({e}), storing without vectors."
                                        );
                                    }
                                    None
                                }
                            }
                        } else {
                            if chunk.index == 0 {
                                println!(
                                    "  No embedding provider configured. Storing without vectors."
                                );
                                println!(
                                    "  (Add an embeddings section to config.yml for semantic search.)"
                                );
                            }
                            None
                        };

                        let model = embedding_provider.as_ref().map(|p| p.model().to_string());
                        let dims = embedding.as_ref().map(|e| e.len());

                        doc_store.add_chunk(
                            &doc_id,
                            chunk.index,
                            &chunk.text,
                            embedding.as_deref(),
                            model.as_deref(),
                            dims,
                            Some(chunk.token_count),
                        )?;
                    }

                    doc_store.update_chunk_count(&doc_id, chunks.len())?;

                    let has_embeddings = embedding_provider.is_some();
                    println!(
                        "  Done. {} chunks stored{}.",
                        chunks.len(),
                        if has_embeddings {
                            " with embeddings"
                        } else {
                            " (no embeddings)"
                        }
                    );
                }
                DocCommands::Ingest { path, replace } => {
                    let dir_path = std::path::Path::new(&path);
                    if !dir_path.exists() {
                        println!("Path not found: {path}");
                        return Ok(());
                    }
                    let embedding_provider = build_embedding_provider(&config);
                    let summary =
                        run_ingest(&doc_store, dir_path, replace, embedding_provider.as_deref())
                            .await?;
                    println!(
                        "\nIngest complete: {} ingested, {} skipped, {} failed.",
                        summary.ingested, summary.skipped, summary.failed
                    );
                }
                DocCommands::List => {
                    let docs = doc_store.list_documents()?;
                    if docs.is_empty() {
                        println!(
                            "No documents ingested. Use `opencrust doc add <file>` to ingest."
                        );
                    } else {
                        println!("Ingested documents:");
                        for doc in &docs {
                            println!(
                                "  {} - {} chunks, {} ({})",
                                doc.name, doc.chunk_count, doc.mime_type, doc.created_at
                            );
                        }
                    }
                }
                DocCommands::Remove { name } => {
                    if doc_store.remove_document(&name)? {
                        println!("Removed document '{name}' and all its chunks.");
                    } else {
                        println!("Document '{name}' not found.");
                    }
                }
            }
        }
        Commands::Doctor => {
            init_tracing("error");
            let passed = doctor::run_doctor(&config, config_loader.config_dir()).await?;
            if !passed {
                std::process::exit(1);
            }
        }
        Commands::Update { yes } => {
            init_tracing(&cli.log_level);
            match update::run_update(yes).await {
                Ok(_) => {}
                Err(e) => println!("update failed: {}", e),
            }
        }
        Commands::Rollback => {
            init_tracing(&cli.log_level);
            match update::run_rollback() {
                Ok(()) => {}
                Err(e) => println!("rollback failed: {}", e),
            }
        }
        Commands::Uninstall { yes, keep_data } => {
            init_tracing(&cli.log_level);
            run_uninstall(yes, keep_data, &config)?;
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

            // Build a fresh tokio runtime in the daemon child process.
            // This is safe because daemonization happened before any runtime
            // was created, so there are no stale kqueue/epoll FDs.
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

#[cfg(windows)]
fn start_daemon(_config: opencrust_config::AppConfig) -> Result<()> {
    anyhow::bail!(
        "Background daemon mode (-d/--daemon) is not supported on Windows.\nPlease run 'opencrust start' in a separate terminal window."
    );
}

#[cfg(not(any(unix, windows)))]
fn start_daemon(_config: opencrust_config::AppConfig) -> Result<()> {
    anyhow::bail!("daemonization is only supported on Unix systems. Run without --daemon.");
}

/// Best-effort stop: kill the daemon if running, silently do nothing otherwise.
/// Falls back to finding the process by port if no PID file exists.
#[cfg(unix)]
fn try_stop_daemon(port: u16) {
    if let Some(pid) = read_pid() {
        if is_process_running(pid) {
            println!("Stopping daemon (PID {pid})...");
            kill_and_wait(pid);
        }
        let _ = std::fs::remove_file(pid_file_path());
        return;
    }

    // No PID file - kill everything on the port (except ourselves)
    for pid in find_pids_on_port(port) {
        println!("Stopping process on port {port} (PID {pid})...");
        kill_and_wait(pid);
    }
}

#[cfg(windows)]
fn try_stop_daemon(port: u16) {
    if let Some(pid) = read_pid() {
        if is_process_running(pid) {
            println!("Stopping daemon (PID {pid})...");
            kill_and_wait(pid);
        }
        let _ = std::fs::remove_file(pid_file_path());
        return;
    }

    for pid in find_pids_on_port(port) {
        println!("Stopping process on port {port} (PID {pid})...");
        kill_and_wait(pid);
    }
}

#[cfg(not(any(unix, windows)))]
fn try_stop_daemon(_port: u16) {}

#[cfg(unix)]
fn stop_daemon(port: u16) -> Result<()> {
    let pid_path = pid_file_path();

    // Try PID file first
    let pid = if let Some(pid) = read_pid() {
        if !is_process_running(pid) {
            println!("Process {} is not running (removing stale PID file)", pid);
            std::fs::remove_file(&pid_path).ok();
            // Fall through to port-based lookup
            None
        } else {
            Some(pid)
        }
    } else {
        None
    };

    // If no running PID from file, check port
    let pid = match pid {
        Some(p) => p,
        None => find_pid_on_port(port).context(format!(
            "no running OpenCrust process found (no PID file at {}, nothing on port {port})",
            pid_path.display()
        ))?,
    };

    println!("Sending SIGTERM to PID {pid}...");
    if kill_and_wait(pid) {
        println!("OpenCrust stopped.");
        std::fs::remove_file(&pid_path).ok();
    } else {
        println!("Process {pid} did not exit within 5s. It may still be shutting down.");
    }

    Ok(())
}

#[cfg(windows)]
fn stop_daemon(port: u16) -> Result<()> {
    let pid_path = pid_file_path();

    let pid = if let Some(pid) = read_pid() {
        if !is_process_running(pid) {
            println!("Process {} is not running (removing stale PID file)", pid);
            std::fs::remove_file(&pid_path).ok();
            None
        } else {
            Some(pid)
        }
    } else {
        None
    };

    let pid = match pid {
        Some(p) => p,
        None => find_pid_on_port(port).context(format!(
            "no running OpenCrust process found (no PID file at {}, nothing on port {port})",
            pid_path.display()
        ))?,
    };

    println!("Terminating PID {pid}...");
    if kill_and_wait(pid) {
        println!("OpenCrust stopped.");
        std::fs::remove_file(&pid_path).ok();
    } else {
        println!("Process {pid} did not exit within 5s. It may still be shutting down.");
    }

    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn stop_daemon(_port: u16) -> Result<()> {
    anyhow::bail!("daemon stop is not supported on this platform");
}

fn run_uninstall(yes: bool, keep_data: bool, config: &opencrust_config::AppConfig) -> Result<()> {
    let opencrust_dir = opencrust_dir();
    let binary_path = std::env::current_exe().context("failed to determine binary path")?;

    // Show what will be removed
    println!();
    println!("  This will remove:");
    println!("    - OpenCrust binary at {}", binary_path.display());
    if !keep_data {
        if opencrust_dir.exists() {
            println!(
                "    - Configuration and data at {}/",
                opencrust_dir.display()
            );
        }
    } else {
        println!(
            "    (keeping config and data at {}/)",
            opencrust_dir.display()
        );
    }
    println!();

    // Confirm
    if !yes {
        let confirmed = dialoguer::Confirm::new()
            .with_prompt("Are you sure?")
            .default(false)
            .interact()
            .unwrap_or(false);

        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Stop daemon if running
    try_stop_daemon(config.gateway.port);

    // Remove data directory
    if !keep_data && opencrust_dir.exists() {
        std::fs::remove_dir_all(&opencrust_dir)
            .context(format!("failed to remove {}", opencrust_dir.display()))?;
        println!("Removed {}/", opencrust_dir.display());
    }

    // Remove binary - on Unix we can delete our own executable while running.
    // On Windows this may fail, so we handle it gracefully.
    match std::fs::remove_file(&binary_path) {
        Ok(()) => println!("Removed {}", binary_path.display()),
        Err(e) => {
            // On Windows, suggest manual removal
            println!(
                "Could not remove binary ({}). Delete it manually:\n  rm {}",
                e,
                binary_path.display()
            );
        }
    }

    // Also remove the .old backup if it exists (from self-update)
    let old_path = binary_path.with_extension("old");
    if old_path.exists() {
        let _ = std::fs::remove_file(&old_path);
    }

    println!();
    println!("OpenCrust has been uninstalled.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_plugin_path() {
        // Valid paths
        assert!(validate_plugin_path("foo").is_ok());
        assert!(validate_plugin_path("foo/bar").is_ok());
        assert!(validate_plugin_path("foo\\bar").is_ok());
        assert!(validate_plugin_path("/absolute/path").is_ok());
        assert!(validate_plugin_path("C:\\Windows\\System32").is_ok());

        // Invalid paths
        assert!(validate_plugin_path("").is_err());
        assert!(validate_plugin_path("   ").is_err());
    }

    // ── run_ingest tests ─────────────────────────────────────────────────────

    /// Helper: open a fresh DocumentStore in a temp file.
    fn tmp_store() -> (tempfile::NamedTempFile, opencrust_db::DocumentStore) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let s = opencrust_db::DocumentStore::open(f.path()).unwrap();
        (f, s)
    }

    /// Helper: write a file with text content into a temp directory.
    fn write_file(dir: &std::path::Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    /// Test plan item 1: ingests all supported files and counts them.
    /// Unsupported extensions (.rs, .jpg) must be silently ignored.
    #[tokio::test]
    async fn ingest_counts_supported_files_and_ignores_unsupported() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let (_f, store) = tmp_store();

        write_file(tmp_dir.path(), "a.md", "# Hello\nThis is markdown content.");
        write_file(tmp_dir.path(), "b.txt", "Plain text file with content.");
        write_file(
            tmp_dir.path(),
            "c.html",
            "<html><body>HTML content</body></html>",
        );
        write_file(tmp_dir.path(), "ignored.rs", "fn main() {}"); // unsupported
        write_file(tmp_dir.path(), "ignored.jpg", "not an image"); // unsupported

        let summary = run_ingest(&store, tmp_dir.path(), false, None)
            .await
            .unwrap();

        assert_eq!(summary.ingested, 3, "should ingest .md + .txt + .html");
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.failed, 0);

        // Unsupported files must not appear in the store
        assert!(store.get_document_by_name("ignored.rs").unwrap().is_none());
        assert!(store.get_document_by_name("ignored.jpg").unwrap().is_none());
    }

    /// Test plan item 2: re-running without --replace skips already-ingested files.
    #[tokio::test]
    async fn ingest_skips_duplicates_without_replace() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let (_f, store) = tmp_store();

        write_file(tmp_dir.path(), "doc.md", "# Doc\nContent for RAG.");
        write_file(tmp_dir.path(), "doc2.txt", "Another document.");

        // First pass
        let first = run_ingest(&store, tmp_dir.path(), false, None)
            .await
            .unwrap();
        assert_eq!(first.ingested, 2);

        // Second pass — no replace
        let second = run_ingest(&store, tmp_dir.path(), false, None)
            .await
            .unwrap();
        assert_eq!(second.ingested, 0);
        assert_eq!(second.skipped, 2, "both files should be skipped");
        assert_eq!(second.failed, 0);
    }

    /// Test plan item 3: --replace removes old copy and re-ingests.
    #[tokio::test]
    async fn ingest_replaces_duplicates_with_flag() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let (_f, store) = tmp_store();

        write_file(tmp_dir.path(), "doc.md", "# Original\nFirst version.");

        run_ingest(&store, tmp_dir.path(), false, None)
            .await
            .unwrap();

        // Overwrite file on disk with new content
        write_file(tmp_dir.path(), "doc.md", "# Updated\nSecond version.");

        let summary = run_ingest(&store, tmp_dir.path(), true, None)
            .await
            .unwrap();

        assert_eq!(summary.ingested, 1, "should re-ingest with --replace");
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.failed, 0);

        // Verify only one document named doc.md remains
        let docs = store.list_documents().unwrap();
        assert_eq!(docs.iter().filter(|d| d.name == "doc.md").count(), 1);
    }

    /// Test plan item 4: directory with only unsupported files → no files found,
    /// summary is all-zero (not an error).
    #[tokio::test]
    async fn ingest_returns_zero_for_unsupported_only_dir() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let (_f, store) = tmp_store();

        write_file(tmp_dir.path(), "main.rs", "fn main() {}");
        write_file(tmp_dir.path(), "image.png", "binary data");

        let summary = run_ingest(&store, tmp_dir.path(), false, None)
            .await
            .unwrap();

        assert_eq!(
            summary,
            IngestSummary::default(),
            "no files found → zero summary"
        );
        assert!(store.list_documents().unwrap().is_empty());
    }

    /// Test plan item 5: passing a file (not a directory) returns an error.
    #[tokio::test]
    async fn ingest_rejects_file_path() {
        let tmp_file = tempfile::NamedTempFile::new().unwrap();
        let (_f, store) = tmp_store();

        let result = run_ingest(&store, tmp_file.path(), false, None).await;
        assert!(
            result.is_err(),
            "run_ingest should fail when given a file path"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("is not a directory")
        );
    }
}
