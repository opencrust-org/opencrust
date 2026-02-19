use std::sync::Arc;

use opencrust_common::{
    ChannelId, Message, MessageContent, MessageDirection, Result, SessionId, UserId,
};
use opencrust_config::{AppConfig, ConfigWatcher};
use opencrust_db::SessionStore;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{info, warn};

#[cfg(target_os = "macos")]
use crate::bootstrap::build_imessage_channels;
use crate::bootstrap::{
    build_agent_runtime, build_channels, build_discord_channels, build_mcp_tools,
    build_slack_channels, build_telegram_channels, build_whatsapp_channels,
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

        let mut agents = build_agent_runtime(&self.config);

        // Connect MCP servers and register their tools
        let (mcp_manager, mcp_tools) = build_mcp_tools(&self.config).await;
        for tool in mcp_tools {
            agents.register_tool(tool);
        }

        let channels = build_channels(&self.config).await;
        let mut state = AppState::new(self.config, agents, channels);
        state.mcp_manager = Some(mcp_manager);

        // Initialize persistent session storage used by channel memory bus hydration.
        let data_dir = state
            .config
            .data_dir
            .clone()
            .or_else(|| dirs::home_dir().map(|h| h.join(".opencrust").join("data")))
            .unwrap_or_else(|| ".opencrust/data".into());
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            warn!("failed to create data directory: {e}");
        }
        let sessions_db = data_dir.join("sessions.db");
        match SessionStore::open(&sessions_db) {
            Ok(store) => {
                let store = Arc::new(Mutex::new(store));
                state.set_session_store(Arc::clone(&store));
                state
                    .agents
                    .register_tool(Box::new(opencrust_agents::ScheduleHeartbeat::new(store)));
                info!("session store opened at {}", sessions_db.display());
            }
            Err(e) => {
                warn!("failed to open session store: {e}");
            }
        }

        // Start config hot-reload watcher
        let config_path = opencrust_config::ConfigLoader::default_config_dir().join("config.yml");

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

        // Wrap MCP manager in Arc for health monitor before moving into state
        let mcp_manager_arc = state.mcp_manager.take().map(Arc::new);
        if let Some(ref arc) = mcp_manager_arc {
            state.mcp_manager_arc = Some(Arc::clone(arc));
        }

        let state = Arc::new(state);

        // Spawn background tasks
        state.spawn_session_cleanup();
        state.spawn_config_applier();

        // Spawn MCP health monitor for auto-reconnect
        if let Some(ref arc) = mcp_manager_arc {
            arc.spawn_health_monitor();
        }

        // Start configured Discord channels
        let discord_channels = build_discord_channels(&state.config, &state);
        for mut channel in discord_channels {
            tokio::spawn(async move {
                if let Err(e) = channel.connect().await {
                    warn!("discord channel failed to connect: {e}");
                    return;
                }
                shutdown_signal().await;
                channel.disconnect().await.ok();
            });
        }

        // Start background scheduler loop
        let scheduler_state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                if let Err(e) = run_scheduler(&scheduler_state).await {
                    tracing::error!("Scheduler error: {e}");
                }
            }
        });

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

        // Start configured iMessage channels (macOS only)
        #[cfg(target_os = "macos")]
        {
            let imessage_channels = build_imessage_channels(&state.config, &state);
            for mut channel in imessage_channels {
                tokio::spawn(async move {
                    if let Err(e) = channel.connect().await {
                        warn!("imessage channel failed to connect: {e}");
                        return;
                    }
                    shutdown_signal().await;
                    channel.disconnect().await.ok();
                });
            }
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

        let state_for_shutdown = Arc::clone(&state);
        let app = build_router(state, whatsapp_state);

        let listener = TcpListener::bind(&addr).await?;
        info!("OpenCrust gateway listening on {}", addr);

        // Graceful shutdown on Ctrl-C / SIGTERM.
        // `into_make_service_with_connect_info` injects ConnectInfo<SocketAddr>
        // so that the rate-limiter can extract per-client IP addresses.
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| opencrust_common::Error::Gateway(format!("server error: {e}")))?;

        // Disconnect MCP servers on shutdown
        if let Some(ref manager) = state_for_shutdown.mcp_manager_arc {
            info!("disconnecting MCP servers...");
            manager.disconnect_all().await;
        } else if let Some(ref manager) = state_for_shutdown.mcp_manager {
            info!("disconnecting MCP servers...");
            manager.disconnect_all().await;
        }

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
async fn run_scheduler(state: &AppState) -> Result<()> {
    let store_mutex = match &state.session_store {
        Some(s) => s,
        None => return Ok(()),
    };

    let tasks = {
        let store = store_mutex.lock().await;
        store.poll_due_tasks()?
    };

    if tasks.is_empty() {
        return Ok(());
    }

    info!("scheduler executing {} due tasks", tasks.len());

    for task in tasks {
        if let Err(e) = execute_scheduled_task(state, store_mutex, &task).await {
            tracing::error!("Scheduled task {} failed: {e} — marking as failed", task.id);
            let store = store_mutex.lock().await;
            if let Err(fe) = store.fail_task(&task.id) {
                tracing::error!("Failed to mark task {} as failed: {fe}", task.id);
            }
        }
    }
    Ok(())
}

async fn execute_scheduled_task(
    state: &AppState,
    store_mutex: &Arc<Mutex<SessionStore>>,
    task: &opencrust_db::ScheduledTask,
) -> Result<()> {
    let channel_type = &task.channel_id;

    let message = Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: SessionId::from_string(&task.session_id),
        channel_id: ChannelId::from_string(channel_type),
        user_id: UserId::from_string(&task.user_id),
        direction: MessageDirection::Incoming,
        content: MessageContent::System(task.payload.clone()),
        timestamp: chrono::Utc::now(),
        metadata: task.session_metadata.clone(),
    };

    // 1. Persist system message to history so agent has context
    {
        let store = store_mutex.lock().await;
        store.append_message(
            &task.session_id,
            "system",
            &task.payload,
            message.timestamp,
            &task.session_metadata,
        )?;
    }

    // 2. Hydrate session history and invoke agent runtime
    state
        .hydrate_session_history(
            &task.session_id,
            Some(channel_type.as_str()),
            Some(task.user_id.as_str()),
        )
        .await;

    let history = state.session_history(&task.session_id);
    let continuity_key = state
        .continuity_key(Some(task.user_id.as_str()))
        .map(|k| k.as_str().to_string());

    let response_text = state
        .agents
        .process_heartbeat(
            &task.session_id,
            &task.payload,
            &history,
            continuity_key.as_deref(),
            Some(task.user_id.as_str()),
        )
        .await?;

    let response_msg = Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: SessionId::from_string(&task.session_id),
        channel_id: ChannelId::from_string(channel_type),
        user_id: UserId::from_string("genesis"),
        direction: MessageDirection::Outgoing,
        content: MessageContent::Text(response_text.clone()),
        timestamp: chrono::Utc::now(),
        metadata: task.session_metadata.clone(),
    };

    // 3. Persist assistant response regardless of outbound channel availability.
    {
        let store = store_mutex.lock().await;
        store.append_message(
            &task.session_id,
            "assistant",
            &response_text,
            response_msg.timestamp,
            &task.session_metadata,
        )?;
    }

    // 4. Best-effort delivery to channel adapter.
    if let Some(channel) = state.channels.get(channel_type.as_str()) {
        if let Err(e) = channel.send_message(&response_msg).await {
            tracing::error!("Failed to send scheduled response: {e}");
        }
    } else {
        tracing::warn!(
            "Scheduled response persisted but no channel adapter registered for: {}",
            channel_type
        );
    }

    // 5. Complete task
    {
        let store = store_mutex.lock().await;
        store.complete_task(&task.id)?;
    }

    Ok(())
}
