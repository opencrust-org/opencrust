use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use opencrust_agents::{AgentRuntime, ChatMessage};
use opencrust_channels::ChannelRegistry;
use opencrust_config::AppConfig;
use opencrust_db::SessionStore;
use tokio::sync::{Mutex, watch};
use tracing::{info, warn};
use uuid::Uuid;

/// How long a disconnected session is kept for resume.
const SESSION_TTL: Duration = Duration::from_secs(3600); // 1 hour
/// How often the cleanup task runs.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(300); // 5 minutes

/// Shared application state accessible from all request handlers.
pub struct AppState {
    pub config: AppConfig,
    pub channels: ChannelRegistry,
    pub agents: AgentRuntime,
    pub sessions: DashMap<String, SessionState>,
    /// In-flight A2A tasks keyed by task ID.
    pub a2a_tasks: DashMap<String, opencrust_agents::a2a::A2ATask>,
    /// MCP server connection manager (legacy, for backward compat).
    pub mcp_manager: Option<opencrust_agents::McpManager>,
    /// MCP manager wrapped in Arc for health monitoring.
    pub mcp_manager_arc: Option<Arc<opencrust_agents::McpManager>>,
    pub session_store: Option<Arc<Mutex<SessionStore>>>,
    /// Receives hot-reloaded config updates. `None` if watcher is not active.
    config_rx: Option<watch::Receiver<AppConfig>>,
}

/// Per-connection session tracking.
pub struct SessionState {
    pub id: String,
    pub user_id: Option<String>,
    pub channel_id: Option<String>,
    pub history: Vec<ChatMessage>,
    /// Whether a WebSocket is currently attached.
    pub connected: bool,
    /// When the session was created.
    pub created_at: Instant,
    /// Last time the session had activity (message or pong).
    pub last_active: Instant,
    /// Rolling conversation summary for context recovery.
    pub summary: Option<String>,
}

impl AppState {
    pub fn new(config: AppConfig, agents: AgentRuntime, channels: ChannelRegistry) -> Self {
        Self {
            config,
            channels,
            agents,
            sessions: DashMap::new(),
            a2a_tasks: DashMap::new(),
            mcp_manager: None,
            mcp_manager_arc: None,
            session_store: None,
            config_rx: None,
        }
    }

    /// Attach a persistent session store used to hydrate and persist chat history.
    pub fn set_session_store(&mut self, store: Arc<Mutex<SessionStore>>) {
        self.session_store = Some(store);
    }

    /// Attach a config watch receiver for hot-reload support.
    pub fn set_config_watcher(&mut self, rx: watch::Receiver<AppConfig>) {
        self.config_rx = Some(rx);
    }

    /// Get the latest config, preferring the hot-reloaded version if available.
    pub fn current_config(&self) -> AppConfig {
        if let Some(rx) = &self.config_rx {
            rx.borrow().clone()
        } else {
            self.config.clone()
        }
    }

    pub fn create_session(&self) -> String {
        let id = Uuid::new_v4().to_string();
        self.create_session_with_id(id.clone());
        id
    }

    /// Create a session with a specific ID (used by channels like Telegram
    /// where the external chat ID determines the session key).
    pub fn create_session_with_id(&self, id: String) {
        let now = Instant::now();
        self.sessions.insert(
            id.clone(),
            SessionState {
                id,
                user_id: None,
                channel_id: None,
                history: Vec::new(),
                connected: true,
                created_at: now,
                last_active: now,
                summary: None,
            },
        );
    }

    /// Resolve the continuity key used by the cross-channel memory bus.
    /// When disabled, returns `None` and memory remains session-scoped.
    pub fn continuity_key(&self, _user_id: Option<&str>) -> Option<String> {
        if self.config.memory.shared_continuity {
            Some("bus:shared-global".to_string())
        } else {
            None
        }
    }

    /// Return the current summary for a session, if any.
    pub fn session_summary(&self, session_id: &str) -> Option<String> {
        self.sessions
            .get(session_id)
            .and_then(|s| s.summary.clone())
    }

    /// Update the rolling conversation summary for a session.
    pub fn update_session_summary(&self, session_id: &str, summary: &str) {
        if let Some(mut session) = self.sessions.get_mut(session_id) {
            session.summary = Some(summary.to_string());
        }
    }

    /// Return a cloned history snapshot for a session.
    pub fn session_history(&self, session_id: &str) -> Vec<ChatMessage> {
        self.sessions
            .get(session_id)
            .map(|s| s.history.clone())
            .unwrap_or_default()
    }

    /// Ensure a session is present in memory and hydrate recent history from persistent storage.
    pub async fn hydrate_session_history(
        &self,
        session_id: &str,
        channel_id: Option<&str>,
        user_id: Option<&str>,
    ) {
        if !self.sessions.contains_key(session_id) {
            self.create_session_with_id(session_id.to_string());
        }

        if let Some(mut session) = self.sessions.get_mut(session_id) {
            if let Some(channel) = channel_id {
                session.channel_id = Some(channel.to_string());
            }
            if let Some(user) = user_id {
                session.user_id = Some(user.to_string());
            }
            session.connected = true;
            session.last_active = Instant::now();
        }

        let Some(store) = &self.session_store else {
            return;
        };

        let should_load = self
            .sessions
            .get(session_id)
            .map(|s| s.history.is_empty())
            .unwrap_or(false);

        let channel = channel_id.unwrap_or("web");
        let user = user_id.unwrap_or("anonymous");
        let metadata = self
            .continuity_key(user_id)
            .map(|k| serde_json::json!({ "continuity_key": k }))
            .unwrap_or_else(|| serde_json::json!({}));

        let mut loaded_history = Vec::new();
        let mut loaded_summary: Option<String> = None;
        {
            let guard = store.lock().await;
            if let Err(e) = guard.upsert_session(session_id, channel, user, &metadata) {
                warn!("failed to upsert session {session_id} in session store: {e}");
            }

            if should_load {
                match guard.load_recent_messages(session_id, 100) {
                    Ok(messages) => {
                        loaded_history = messages
                            .into_iter()
                            .filter_map(|m| match m.direction.as_str() {
                                "user" => Some(ChatMessage {
                                    role: opencrust_agents::ChatRole::User,
                                    content: opencrust_agents::MessagePart::Text(m.content),
                                }),
                                "assistant" => Some(ChatMessage {
                                    role: opencrust_agents::ChatRole::Assistant,
                                    content: opencrust_agents::MessagePart::Text(m.content),
                                }),
                                _ => None,
                            })
                            .collect();
                    }
                    Err(e) => {
                        warn!("failed to load session history for {session_id}: {e}");
                    }
                }

                // Load summary from session metadata
                if let Ok(Some(meta)) = guard.load_session_metadata(session_id) {
                    loaded_summary = meta
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
        }

        if should_load && let Some(mut session) = self.sessions.get_mut(session_id) {
            if !loaded_history.is_empty() {
                session.history = loaded_history;
            }
            if loaded_summary.is_some() && session.summary.is_none() {
                session.summary = loaded_summary;
            }
        }
    }

    /// Append a user/assistant turn to in-memory state and persistent session storage.
    pub async fn persist_turn(
        &self,
        session_id: &str,
        channel_id: Option<&str>,
        user_id: Option<&str>,
        user_text: &str,
        assistant_text: &str,
    ) {
        if !self.sessions.contains_key(session_id) {
            self.create_session_with_id(session_id.to_string());
        }

        if let Some(mut session) = self.sessions.get_mut(session_id) {
            if let Some(channel) = channel_id {
                session.channel_id = Some(channel.to_string());
            }
            if let Some(user) = user_id {
                session.user_id = Some(user.to_string());
            }
            session.last_active = Instant::now();
            session.history.push(ChatMessage {
                role: opencrust_agents::ChatRole::User,
                content: opencrust_agents::MessagePart::Text(user_text.to_string()),
            });
            session.history.push(ChatMessage {
                role: opencrust_agents::ChatRole::Assistant,
                content: opencrust_agents::MessagePart::Text(assistant_text.to_string()),
            });
        }

        let Some(store) = &self.session_store else {
            return;
        };

        let channel = channel_id.unwrap_or("web");
        let user = user_id.unwrap_or("anonymous");

        // Build metadata including continuity key and summary if present
        let summary = self.session_summary(session_id);
        let mut meta_map = serde_json::Map::new();
        if let Some(k) = self.continuity_key(user_id) {
            meta_map.insert("continuity_key".to_string(), serde_json::json!(k));
        }
        if let Some(s) = &summary {
            meta_map.insert("summary".to_string(), serde_json::json!(s));
        }
        let metadata = serde_json::Value::Object(meta_map);

        let guard = store.lock().await;
        if let Err(e) = guard.upsert_session(session_id, channel, user, &metadata) {
            warn!("failed to upsert session {session_id}: {e}");
            return;
        }
        if let Err(e) = guard.append_message(
            session_id,
            "user",
            user_text,
            chrono::Utc::now(),
            &serde_json::json!({ "channel_id": channel, "user_id": user }),
        ) {
            warn!("failed to persist user message for {session_id}: {e}");
        }
        if let Err(e) = guard.append_message(
            session_id,
            "assistant",
            assistant_text,
            chrono::Utc::now(),
            &serde_json::json!({ "channel_id": channel, "user_id": user }),
        ) {
            warn!("failed to persist assistant message for {session_id}: {e}");
        }

        // Prune old messages to prevent unbounded DB growth (keep last 500)
        if let Err(e) = guard.prune_old_messages(session_id, 500) {
            warn!("failed to prune old messages for {session_id}: {e}");
        }
    }

    /// Mark a session as disconnected (but don't remove it yet).
    pub fn disconnect_session(&self, session_id: &str) {
        if let Some(mut session) = self.sessions.get_mut(session_id) {
            session.connected = false;
            session.last_active = Instant::now();
        }
    }

    /// Try to resume an existing disconnected session. Returns `true` if resumed.
    pub fn resume_session(&self, session_id: &str) -> bool {
        if let Some(mut session) = self.sessions.get_mut(session_id) {
            session.connected = true;
            session.last_active = Instant::now();
            true
        } else {
            false
        }
    }

    /// Remove sessions that have been disconnected longer than the TTL.
    pub fn cleanup_expired_sessions(&self) -> usize {
        let now = Instant::now();
        let mut removed = 0;

        self.sessions.retain(|_id, session| {
            if !session.connected && now.duration_since(session.last_active) > SESSION_TTL {
                removed += 1;
                false
            } else {
                true
            }
        });

        if removed > 0 {
            info!("cleaned up {removed} expired sessions");
        }
        removed
    }

    /// Spawn a background task that periodically cleans up expired sessions.
    pub fn spawn_session_cleanup(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
            loop {
                interval.tick().await;
                state.cleanup_expired_sessions();
            }
        });
    }

    /// Spawn a background task that logs hot-reloaded config changes.
    /// Note: Agent-level settings (system_prompt, max_tokens) will take effect
    /// on next restart. Provider and channel changes also require restart.
    pub fn spawn_config_applier(self: &Arc<Self>) {
        let Some(mut rx) = self.config_rx.clone() else {
            return;
        };

        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let new_config = rx.borrow().clone();

                if let Some(prompt) = &new_config.agent.system_prompt {
                    info!(
                        "config reloaded: system_prompt updated (len={})",
                        prompt.len()
                    );
                }
                if let Some(max_tokens) = new_config.agent.max_tokens {
                    info!("config reloaded: max_tokens={max_tokens}");
                }
                if let Some(level) = &new_config.log_level {
                    info!("config reloaded: log_level={level}");
                }
            }

            warn!("config watcher channel closed");
        });
    }
}

pub type SharedState = Arc<AppState>;

#[cfg(test)]
mod tests {
    use super::*;
    use opencrust_agents::AgentRuntime;
    use opencrust_channels::ChannelRegistry;
    use opencrust_config::AppConfig;

    fn test_state() -> AppState {
        AppState::new(
            AppConfig::default(),
            AgentRuntime::new(),
            ChannelRegistry::new(),
        )
    }

    #[test]
    fn create_session_returns_unique_ids() {
        let state = test_state();
        let id1 = state.create_session();
        let id2 = state.create_session();
        assert_ne!(id1, id2);
        assert_eq!(state.sessions.len(), 2);
    }

    #[test]
    fn disconnect_and_resume_session_round_trip() {
        let state = test_state();
        let id = state.create_session();

        // Initially connected
        assert!(state.sessions.get(&id).unwrap().connected);

        state.disconnect_session(&id);
        assert!(!state.sessions.get(&id).unwrap().connected);

        let resumed = state.resume_session(&id);
        assert!(resumed);
        assert!(state.sessions.get(&id).unwrap().connected);
    }

    #[test]
    fn resume_nonexistent_session_returns_false() {
        let state = test_state();
        assert!(!state.resume_session("does-not-exist"));
    }

    #[test]
    fn cleanup_expired_sessions_removes_only_disconnected_expired() {
        let state = test_state();

        // Create two sessions
        let active_id = state.create_session();
        let expired_id = state.create_session();

        // Disconnect the expired session and backdate its last_active
        state.disconnect_session(&expired_id);
        if let Some(mut session) = state.sessions.get_mut(&expired_id) {
            session.last_active = Instant::now() - Duration::from_secs(7200);
        }

        let removed = state.cleanup_expired_sessions();
        assert_eq!(removed, 1);
        assert!(state.sessions.contains_key(&active_id));
        assert!(!state.sessions.contains_key(&expired_id));
    }

    #[test]
    fn cleanup_does_not_remove_connected_sessions() {
        let state = test_state();
        let id = state.create_session();

        // Backdate but keep connected
        if let Some(mut session) = state.sessions.get_mut(&id) {
            session.last_active = Instant::now() - Duration::from_secs(7200);
        }

        let removed = state.cleanup_expired_sessions();
        assert_eq!(removed, 0);
        assert!(state.sessions.contains_key(&id));
    }

    #[test]
    fn session_history_returns_empty_for_unknown_session() {
        let state = test_state();
        let history = state.session_history("nonexistent");
        assert!(history.is_empty());
    }

    #[test]
    fn continuity_key_with_shared_continuity_enabled() {
        let mut config = AppConfig::default();
        config.memory.shared_continuity = true;
        let state = AppState::new(config, AgentRuntime::new(), ChannelRegistry::new());
        let key = state.continuity_key(Some("user1"));
        assert_eq!(key, Some("bus:shared-global".to_string()));
    }

    #[test]
    fn continuity_key_with_shared_continuity_disabled() {
        let mut config = AppConfig::default();
        config.memory.shared_continuity = false;
        let state = AppState::new(config, AgentRuntime::new(), ChannelRegistry::new());
        let key = state.continuity_key(Some("user1"));
        assert_eq!(key, None);
    }
}
