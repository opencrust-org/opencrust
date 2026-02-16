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
    },

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .init();

    let config_loader = opencrust_config::ConfigLoader::new()?;
    config_loader.ensure_dirs()?;
    let config = config_loader.load()?;

    match cli.command {
        Commands::Start { host, port } => {
            let mut config = config;
            config.gateway.host = host;
            config.gateway.port = port;

            let server = opencrust_gateway::GatewayServer::new(config);
            server.run().await?;
        }
        Commands::Status => {
            println!("OpenCrust status: checking gateway...");
            let client = reqwest::Client::new();
            let mut request = client.get(format!(
                "http://{}:{}/api/status",
                config.gateway.host, config.gateway.port
            ));

            if let Some(api_key) = &config.gateway.api_key {
                request = request.header("Authorization", api_key);
<<<<<<< fix-unauthenticated-status-endpoint-10376317256874345217
            }

            match request.send().await {
                Ok(resp) => {
                    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                        println!("Error: Unauthorized. Please check your api_key in config.yml.");
                    } else {
                        let body = resp.json::<serde_json::Value>().await?;
                        println!("{}", serde_json::to_string_pretty(&body)?);
                    }
                }
                Err(_) => {
                    println!("Gateway is not running.");
                }
=======
>>>>>>> main
            }

            let resp = request.send().await.map_err(|_| {
                anyhow::anyhow!("Gateway is not running at {}:{}", config.gateway.host, config.gateway.port)
            })?;

            if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                anyhow::bail!("Unauthorized. Please check your api_key in config.yml.");
            }

            let body = resp.json::<serde_json::Value>().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        Commands::Init => {
            println!("OpenCrust setup wizard");
            println!("Config directory: {}", config_loader.config_dir().display());
            println!("Directories created. Edit config.yml to get started.");
        }
        Commands::Channel { action } => match action {
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
        },
        Commands::Plugin { action } => match action {
            PluginCommands::List => {
                let loader = opencrust_plugins::PluginLoader::new(
                    config_loader.config_dir().join("plugins"),
                );
                match loader.discover() {
                    Ok(manifests) => {
                        println!("Installed plugins:");
                        if manifests.is_empty() {
                            println!("  (none)");
                        }
                        for m in manifests {
                            println!(
                                "  {} v{} - {}",
                                m.name,
                                m.version,
                                m.description.unwrap_or_default()
                            );
                        }
                    }
                    Err(e) => println!("error scanning plugins: {}", e),
                }
            }
            PluginCommands::Install { path } => {
                println!("TODO: install plugin from {}", path);
            }
        },
    }

    Ok(())
}
