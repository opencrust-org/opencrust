use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use notify::{EventKind, RecursiveMode, Watcher};
use opencrust_channels::{ChannelLifecycle, ChannelSender};
use opencrust_common::{
    ChannelId, Message, MessageContent, MessageDirection, Result, SessionId, UserId,
};
use opencrust_config::{AppConfig, ConfigWatcher};
use opencrust_db::SessionStore;
use opencrust_media::build_tts_provider;
use tokio::net::TcpListener;
use tracing::{info, warn};

#[cfg(target_os = "macos")]
use crate::bootstrap::build_imessage_channels;
use crate::bootstrap::{
    build_agent_runtime, build_channels, build_discord_channels, build_line_channels,
    build_mcp_tools, build_mqtt_channels, build_slack_channels, build_telegram_channels,
    build_wechat_channels, build_whatsapp_channels, build_whatsapp_web_channels, resolve_api_key,
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

        let mut agents = build_agent_runtime(&self.config).await;

        // Connect MCP servers and register their tools
        let (mcp_manager_arc, mcp_tools, mcp_instructions) = build_mcp_tools(&self.config).await;
        for tool in mcp_tools {
            agents.register_tool(tool);
        }

        // Append MCP server instructions to the system prompt
        if let Some(instructions) = &mcp_instructions {
            agents.append_system_prompt(instructions);
        }

        // Register HandoffTool for agent-to-agent delegation (#304).
        // Returns a handle that must be wired after Arc::new(agents).
        let shared_config = Arc::new(RwLock::new(self.config.clone()));
        let (handoff_tool, handoff_handle) =
            opencrust_agents::HandoffTool::new(Arc::clone(&shared_config));
        agents.register_tool(Box::new(handoff_tool));

        let channels = build_channels(&self.config).await;

        // Open session store and register session-dependent tools on the mutable
        // runtime BEFORE wrapping in Arc (register_tool requires &mut self).
        let data_dir =
            self.config.data_dir.clone().unwrap_or_else(|| {
                opencrust_config::ConfigLoader::default_config_dir().join("data")
            });
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            warn!("failed to create data directory: {e}");
        }
        let sessions_db = data_dir.join("sessions.db");
        let session_store_arc = match SessionStore::open(&sessions_db) {
            Ok(store) => {
                let store = Arc::new(store);
                agents.register_tool(Box::new(opencrust_agents::ScheduleHeartbeat::new(
                    Arc::clone(&store),
                )));
                agents.register_tool(Box::new(opencrust_agents::CancelHeartbeat::new(
                    Arc::clone(&store),
                )));
                agents.register_tool(Box::new(opencrust_agents::ListHeartbeats::new(Arc::clone(
                    &store,
                ))));
                info!("session store opened at {}", sessions_db.display());
                Some(store)
            }
            Err(e) => {
                warn!("failed to open session store: {e}");
                None
            }
        };

        // Wrap in Arc now that all &mut setup is complete, then wire the HandoffTool.
        let agents = Arc::new(agents);
        handoff_handle.wire(&agents);
        let mut state = AppState::new(self.config, Arc::clone(&agents), channels);
        state.mcp_manager_arc = Some(Arc::clone(&mcp_manager_arc));

        if let Some(store) = session_store_arc {
            state.set_session_store(store);
        }

        // Wire TTS provider from voice config.
        // Key resolution: vault → voice.api_key → VOICE_API_KEY env → openai provider key.
        let voice_cfg = &state.config.voice;
        let voice_api_key = resolve_api_key(
            voice_cfg.api_key.as_deref(),
            "VOICE_API_KEY",
            "VOICE_API_KEY",
        )
        .or_else(|| {
            // Fall back to the explicitly-named "openai" LLM provider key so we
            // don't accidentally send an Anthropic key to an OpenAI endpoint.
            state
                .config
                .llm
                .get("openai")
                .and_then(|p| p.api_key.clone())
        });

        if let Some(provider) = build_tts_provider(
            voice_cfg.tts_provider.as_deref(),
            voice_api_key,
            voice_cfg.model.clone(),
            voice_cfg.voice.clone(),
            voice_cfg.tts_base_url.clone(),
        ) {
            info!("TTS provider '{}' initialised", provider.name());
            state.set_tts_provider(provider);
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

        // Reference the Arc-wrapped MCP manager for health monitoring
        let mcp_manager_arc = state.mcp_manager_arc.clone();

        // Warn early if no gateway API key is set - Google integration endpoints
        // will reject requests with 403.
        if state.config.gateway.api_key.is_none() {
            warn!(
                "no gateway API key configured - Google integration endpoints will return 403. Set OPENCRUST_GATEWAY_API_KEY or gateway.api_key in config.yml"
            );
        }

        let state = Arc::new(state);

        // Spawn background tasks
        state.spawn_session_cleanup();
        state.spawn_config_applier();

        // Watch dna.md and skills directory for hot-reload
        let config_dir = opencrust_config::ConfigLoader::default_config_dir();
        spawn_dna_watcher(Arc::clone(&state), config_dir.clone());
        spawn_skills_watcher(Arc::clone(&state), config_dir);

        // Spawn MCP health monitor for auto-reconnect
        if let Some(ref arc) = mcp_manager_arc {
            arc.spawn_health_monitor();
        }

        // Start configured Discord channels
        let discord_channels = build_discord_channels(&state.config, &state);
        for mut channel in discord_channels {
            let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
            state
                .channel_senders
                .insert(sender.channel_type().to_string(), sender);
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
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            let mut tick_count: u32 = 0;
            loop {
                interval.tick().await;
                if let Err(e) = run_scheduler(&scheduler_state).await {
                    tracing::error!("Scheduler error: {e}");
                }
                tick_count = tick_count.wrapping_add(1);
                // Cleanup old completed/failed/cancelled tasks every ~hour (720 * 5s)
                if tick_count.is_multiple_of(720)
                    && let Some(store) = &scheduler_state.session_store
                {
                    match store.cleanup_completed_tasks(7) {
                        Ok(n) if n > 0 => info!("cleaned up {n} old scheduled tasks"),
                        Err(e) => tracing::error!("task cleanup failed: {e}"),
                        _ => {}
                    }
                    match store.cleanup_stale_sessions(90) {
                        Ok(n) if n > 0 => info!("cleaned up {n} stale sessions"),
                        Err(e) => tracing::error!("session cleanup failed: {e}"),
                        _ => {}
                    }
                }
            }
        });

        // Start configured Telegram channels
        let telegram_channels = build_telegram_channels(&state.config, &state);
        for mut channel in telegram_channels {
            let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
            state
                .channel_senders
                .insert(sender.channel_type().to_string(), sender);
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
            let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
            state
                .channel_senders
                .insert(sender.channel_type().to_string(), sender);
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
                let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
                state
                    .channel_senders
                    .insert(sender.channel_type().to_string(), sender);
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

        // Build WhatsApp Business channels (webhook-driven - no persistent connection)
        let whatsapp_channels = build_whatsapp_channels(&state.config, &state);
        for channel in &whatsapp_channels {
            let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
            state
                .channel_senders
                .insert(sender.channel_type().to_string(), sender);
            info!(
                "whatsapp channel ready (webhook mode, phone_number_id={})",
                channel.phone_number_id()
            );
        }
        let whatsapp_state: opencrust_channels::whatsapp::webhook::WhatsAppState =
            Arc::new(whatsapp_channels);

        // Start WhatsApp Web channels (sidecar-driven, QR code pairing)
        let whatsapp_web_channels = build_whatsapp_web_channels(&state.config, &state);
        for mut channel in whatsapp_web_channels {
            let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
            state
                .channel_senders
                .insert(sender.channel_type().to_string(), sender);
            tokio::spawn(async move {
                if let Err(e) = channel.connect().await {
                    warn!("whatsapp-web channel failed to connect: {e}");
                    return;
                }
                shutdown_signal().await;
                channel.disconnect().await.ok();
            });
        }

        // Start LINE channels (webhook mode)
        let mut line_channels_raw = build_line_channels(&state.config, &state);
        for channel in &mut line_channels_raw {
            if let Err(e) = channel.connect().await {
                warn!("line channel failed to connect: {e}");
            }
        }
        let line_channels: Vec<Arc<opencrust_channels::line::LineChannel>> =
            line_channels_raw.into_iter().map(Arc::new).collect();
        for channel in &line_channels {
            let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
            state
                .channel_senders
                .insert(sender.channel_type().to_string(), sender);
            info!("line channel ready (webhook mode)");
        }
        let line_state: opencrust_channels::line::webhook::LineWebhookState =
            Arc::new(line_channels);

        let wechat_channels = build_wechat_channels(&state.config, &state);
        for channel in &wechat_channels {
            let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
            state
                .channel_senders
                .insert(sender.channel_type().to_string(), sender);
            info!("wechat channel ready (webhook mode)");
        }
        let wechat_state: opencrust_channels::wechat::webhook::WeChatWebhookState =
            Arc::new(wechat_channels);

        // Start MQTT channels (persistent TCP connection to broker)
        let mut mqtt_channels = build_mqtt_channels(&state.config, &state);
        for mut channel in mqtt_channels.drain(..) {
            let sender: Arc<dyn ChannelSender> = Arc::from(channel.create_sender());
            // Key by channel name to support multiple mqtt instances
            state
                .channel_senders
                .insert(format!("mqtt-{}", sender.channel_type()), sender);
            tokio::spawn(async move {
                if let Err(e) = channel.connect().await {
                    warn!("mqtt channel failed to connect: {e}");
                    return;
                }
                shutdown_signal().await;
                channel.disconnect().await.ok();
            });
        }

        let state_for_shutdown = Arc::clone(&state);
        let app = build_router(state, whatsapp_state, line_state, wechat_state);

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

/// Watch `{config_dir}/dna.md` for changes and hot-reload DNA content into
/// the agent runtime. On delete, DNA content is cleared.
fn spawn_dna_watcher(state: Arc<AppState>, config_dir: PathBuf) {
    let dna_filename = std::ffi::OsStr::new("dna.md");
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel::<()>(8);

    let watcher_result =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                let dominated = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                );
                if dominated {
                    let touches_dna = event
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(|f| f == dna_filename).unwrap_or(false));
                    if touches_dna {
                        let _ = notify_tx.try_send(());
                    }
                }
            }
        });

    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(e) => {
            warn!("failed to create dna.md watcher: {e}");
            return;
        }
    };

    if let Err(e) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
        warn!("failed to watch config dir for dna.md: {e}");
        return;
    }

    info!("watching dna.md for hot-reload");

    let dna_path = config_dir.join("dna.md");
    tokio::spawn(async move {
        let _watcher = watcher; // prevent drop
        loop {
            if notify_rx.recv().await.is_none() {
                break;
            }
            // Debounce
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            while notify_rx.try_recv().is_ok() {}

            match std::fs::read_to_string(&dna_path) {
                Ok(content) if !content.trim().is_empty() => {
                    state.agents.set_dna_content(Some(content));
                    info!("dna.md reloaded");
                }
                Ok(_) => {
                    state.agents.set_dna_content(None);
                    info!("dna.md is empty, cleared DNA content");
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    state.agents.set_dna_content(None);
                    info!("dna.md removed, cleared DNA content");
                }
                Err(e) => {
                    warn!("failed to read dna.md: {e}");
                }
            }
        }
    });
}

/// Watch `{config_dir}/skills/` for `*.md` changes and hot-reload skill
/// definitions into the agent runtime.
fn spawn_skills_watcher(state: Arc<AppState>, config_dir: PathBuf) {
    let skills_dir = config_dir.join("skills");
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel::<()>(8);

    let watcher_result =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                let dominated = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                );
                if dominated {
                    let touches_skill = event
                        .paths
                        .iter()
                        .any(|p| p.extension().and_then(|e| e.to_str()) == Some("md"));
                    if touches_skill {
                        let _ = notify_tx.try_send(());
                    }
                }
            }
        });

    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(e) => {
            warn!("failed to create skills watcher: {e}");
            return;
        }
    };

    // Ensure the skills directory exists so the watcher can attach immediately,
    // even on a fresh install where no skills have been added yet.
    if let Err(e) = std::fs::create_dir_all(&skills_dir) {
        warn!("failed to create skills directory: {e}");
        return;
    }

    if let Err(e) = watcher.watch(&skills_dir, RecursiveMode::NonRecursive) {
        warn!("failed to watch skills dir: {e}");
        return;
    }

    info!("watching skills directory for hot-reload");

    tokio::spawn(async move {
        let _watcher = watcher; // prevent drop
        loop {
            if notify_rx.recv().await.is_none() {
                break;
            }
            // Debounce
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            while notify_rx.try_recv().is_ok() {}

            let scanner = opencrust_skills::SkillScanner::new(&skills_dir);
            match scanner.discover() {
                Ok(skills) => {
                    let count = skills.len();
                    state.agents.index_skills(skills).await;
                    if count > 0 {
                        info!("skills reloaded ({} skill(s))", count);
                    } else {
                        info!("skills directory empty, cleared skills");
                    }
                }
                Err(e) => {
                    warn!("failed to re-scan skills directory: {e}");
                }
            }
        }
    });
}

async fn run_scheduler(state: &AppState) -> Result<()> {
    let store = match &state.session_store {
        Some(s) => s,
        None => return Ok(()),
    };

    let tasks = store.poll_due_tasks()?;

    if tasks.is_empty() {
        return Ok(());
    }

    info!("scheduler executing {} due tasks", tasks.len());

    for task in tasks {
        if let Err(e) = execute_scheduled_task(state, store, &task).await {
            tracing::error!("Scheduled task {} failed: {e}", task.id);
            match store.retry_or_fail_task(&task.id) {
                Ok(true) => {
                    info!(
                        "task {} queued for retry (attempt {})",
                        task.id,
                        task.retry_count + 1
                    );
                }
                Ok(false) => {
                    tracing::error!("task {} permanently failed after max retries", task.id);
                }
                Err(fe) => {
                    tracing::error!("failed to update retry state for task {}: {fe}", task.id);
                }
            }
        }
    }
    Ok(())
}

async fn execute_scheduled_task(
    state: &AppState,
    store: &Arc<SessionStore>,
    task: &opencrust_db::ScheduledTask,
) -> Result<()> {
    // Resolve delivery channel: use override only if a sender is actually registered,
    // otherwise fall back to the session's original channel.
    let delivery_channel = if let Some(ref override_ch) = task.deliver_to_channel {
        if state.channel_senders.contains_key(override_ch.as_str()) {
            override_ch.as_str()
        } else {
            tracing::warn!(
                "deliver_to_channel '{}' not registered, falling back to session channel '{}'",
                override_ch,
                task.channel_id
            );
            &task.channel_id
        }
    } else {
        &task.channel_id
    };

    let message = Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: SessionId::from_string(&task.session_id),
        channel_id: ChannelId::from_string(delivery_channel),
        user_id: UserId::from_string(&task.user_id),
        direction: MessageDirection::Incoming,
        content: MessageContent::System(task.payload.clone()),
        timestamp: chrono::Utc::now(),
        metadata: task.session_metadata.clone(),
    };

    // 1. Persist system message to history so agent has context
    store.append_message(
        &task.session_id,
        "system",
        &task.payload,
        message.timestamp,
        &task.session_metadata,
    )?;

    // 2. Hydrate session history with the ORIGINAL session channel to avoid
    //    corrupting the session's channel_id when delivering cross-channel.
    state
        .hydrate_session_history(
            &task.session_id,
            Some(&task.channel_id),
            Some(task.user_id.as_str()),
        )
        .await;

    let history = state.session_history(&task.session_id);
    let continuity_key = state
        .continuity_key(Some(task.user_id.as_str()))
        .map(|k| k.as_str().to_string());

    // Apply tool allowlist and per-session tool call budget for scheduled tasks
    let guardrails = state.current_config().guardrails.clone();
    state.agents.set_session_tool_config(
        &task.session_id,
        guardrails.allowed_tools.clone(),
        guardrails.session_tool_call_budget,
    );

    let response_text = state
        .agents
        .process_heartbeat(
            &task.session_id,
            &task.payload,
            &history,
            continuity_key.as_deref(),
            Some(task.user_id.as_str()),
            task.heartbeat_depth,
        )
        .await?;

    let response_msg = Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: SessionId::from_string(&task.session_id),
        channel_id: ChannelId::from_string(delivery_channel),
        user_id: UserId::from_string("genesis"),
        direction: MessageDirection::Outgoing,
        content: MessageContent::Text(response_text.clone()),
        timestamp: chrono::Utc::now(),
        metadata: task.session_metadata.clone(),
    };

    // 3. Persist assistant response regardless of outbound channel availability.
    store.append_message(
        &task.session_id,
        "assistant",
        &response_text,
        response_msg.timestamp,
        &task.session_metadata,
    )?;

    // 4. Best-effort delivery to channel adapter via sender handle.
    if let Some(sender) = state.channel_senders.get(delivery_channel) {
        if let Err(e) = sender.send_message(&response_msg).await {
            tracing::error!("Failed to send scheduled response: {e}");
        }
    } else {
        tracing::warn!(
            "Scheduled response persisted but no channel sender registered for: {}",
            delivery_channel
        );
    }

    // 5. Complete task and reschedule if recurring.
    //    Only reschedule if the task was still pending (not cancelled during execution).
    {
        let was_completed = store.complete_task(&task.id)?;
        if was_completed {
            match store.reschedule_recurring_task(task) {
                Ok(Some(new_id)) => {
                    info!("recurring task {} rescheduled as {}", task.id, new_id);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!("failed to reschedule recurring task {}: {e}", task.id);
                }
            }
        } else {
            info!(
                "task {} was cancelled during execution, skipping reschedule",
                task.id
            );
        }
    }

    Ok(())
}
