use std::sync::Arc;

use opencrust_common::Result;
use opencrust_config::{AppConfig, ConfigWatcher};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::bootstrap::{
    build_agent_runtime, build_channels, build_slack_channels, build_telegram_channels,
    build_whatsapp_channels, spawn_discord_listener,
};
use crate::router::build_router;
use crate::state::AppState;

/// The main gateway server that binds to a port and serves the API + WebSocket.
pub struct GatewayServer {
    config: AppConfig,
}

impl GatewayServer {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub async fn run(self) -> Result<()> {
        let addr = format!("{}:{}", self.config.gateway.host, self.config.gateway.port);

        let agents = build_agent_runtime(&self.config);
        let (channels, discord_rx) = build_channels(&self.config).await;
        let mut state = AppState::new(self.config, agents, channels);

        // Start config hot-reload watcher
        let config_path = dirs::home_dir()
            .map(|h| h.join(".opencrust").join("config.yml"))
            .unwrap_or_else(|| ".opencrust/config.yml".into());

        if config_path.exists() {
            match ConfigWatcher::start(config_path.clone(), state.current_config()) {
                Ok((_watcher, rx)) => {
                    // Keep watcher alive for the process lifetime.
                    let watcher = Box::new(_watcher);
                    Box::leak(watcher);

                    state.set_config_watcher(rx);
                    info!("config hot-reload enabled for {}", config_path.display());
                }
                Err(e) => {
                    warn!("config watcher failed to start: {e}");
                }
            }
        }

        let state = Arc::new(state);

        // Spawn background tasks
        state.spawn_session_cleanup();
        state.spawn_config_applier();

        // Start Discord message listener if connected
        if let Some(rx) = discord_rx {
            spawn_discord_listener(Arc::clone(&state), rx);
        }

        // Start configured Telegram channels
        let telegram_channels = build_telegram_channels(&state.config, &state);
        for mut channel in telegram_channels {
            tokio::spawn(async move {
                if let Err(e) = channel.connect().await {
                    warn!("telegram channel failed to connect: {e}");
                    return;
                }
                shutdown_signal().await;
                channel.disconnect().await.ok();
            });
        }

        // Start configured Slack channels
        let slack_channels = build_slack_channels(&state.config, &state);
        for mut channel in slack_channels {
            tokio::spawn(async move {
                if let Err(e) = channel.connect().await {
                    warn!("slack channel failed to connect: {e}");
                    return;
                }
                shutdown_signal().await;
                channel.disconnect().await.ok();
            });
        }

        // Build WhatsApp channels (webhook-driven — no persistent connection)
        let whatsapp_channels = build_whatsapp_channels(&state.config, &state);
        for channel in &whatsapp_channels {
            info!(
                "whatsapp channel ready (webhook mode, phone_number_id={})",
                channel.phone_number_id()
            );
        }
        let whatsapp_state: opencrust_channels::whatsapp::webhook::WhatsAppState =
            Arc::new(whatsapp_channels);

        let app = build_router(state, whatsapp_state);

        let listener = TcpListener::bind(&addr).await?;
        info!("OpenCrust gateway listening on {}", addr);

        // Graceful shutdown on Ctrl-C / SIGTERM
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| opencrust_common::Error::Gateway(format!("server error: {e}")))?;

        info!("gateway shut down gracefully");
        Ok(())
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => info!("received Ctrl+C, shutting down"),
        () = terminate => info!("received SIGTERM, shutting down"),
    }
}
