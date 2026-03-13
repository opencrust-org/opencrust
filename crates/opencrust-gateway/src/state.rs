use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Send-only handles for each active channel, keyed by channel type.
    /// Populated during startup and used by the scheduler for outbound delivery.
    pub channel_senders: DashMap<String, Arc<dyn opencrust_channels::ChannelSender>>,
    /// In-flight A2A tasks keyed by task ID.
    pub a2a_tasks: DashMap<String, opencrust_agents::a2a::A2ATask>,
    /// MCP manager wrapped in Arc for health monitoring and resource access.
    pub mcp_manager_arc: Option<Arc<opencrust_agents::McpManager>>,
    pub session_store: Option<Arc<Mutex<SessionStore>>>,
    /// Per-session rolling summary string used by long-context agent flows.
    session_summaries: DashMap<String, String>,
    /// Runtime connection state for Google Workspace integration.
    google_workspace_integration_connected: AtomicBool,
    /// Connected Google account email (if known).
    google_workspace_email: RwLock<Option<String>>,
    /// Pending Google OAuth state values (anti-CSRF), keyed by state token.
    google_oauth_states: DashMap<String, Instant>,
    /// Pending Codex OAuth state values (anti-CSRF + PKCE verifier).
    codex_oauth_states: DashMap<String, CodexOAuthState>,
    /// Runtime Google OAuth client configuration set from the web UI.
    google_oauth_runtime_config: RwLock<Option<GoogleOAuthRuntimeConfig>>,
    /// Receives hot-reloaded config updates. `None` if watcher is not active.
    config_rx: Option<watch::Receiver<AppConfig>>,
}

#[derive(Debug, Clone)]
pub struct GoogleOAuthRuntimeConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexOAuthState {
    pub code_verifier: String,
    pub redirect_uri: String,
    pub opener_origin: String,
    pub created_at: Instant,
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
}

impl AppState {
    pub fn new(config: AppConfig, agents: AgentRuntime, channels: ChannelRegistry) -> Self {
        Self {
            config,
            channels,
            agents,
            sessions: DashMap::new(),
            channel_senders: DashMap::new(),
            a2a_tasks: DashMap::new(),
            mcp_manager_arc: None,
            session_store: None,
            session_summaries: DashMap::new(),
            google_workspace_integration_connected: AtomicBool::new(false),
            google_workspace_email: RwLock::new(None),
            google_oauth_states: DashMap::new(),
            codex_oauth_states: DashMap::new(),
            google_oauth_runtime_config: RwLock::new(None),
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

    /// Read current Google Workspace integration connection state.
    pub fn google_workspace_connected(&self) -> bool {
        self.google_workspace_integration_connected
            .load(Ordering::Relaxed)
    }

    /// Update Google Workspace integration connection state.
    pub fn set_google_workspace_connected(&self, connected: bool) {
        self.google_workspace_integration_connected
            .store(connected, Ordering::Relaxed);
        if !connected && let Ok(mut email) = self.google_workspace_email.write() {
            *email = None;
        }
    }

    /// Return the connected Google account email if available.
    pub fn google_workspace_email(&self) -> Option<String> {
        self.google_workspace_email
            .read()
            .ok()
            .and_then(|email| email.clone())
    }

    /// Set Google integration identity and mark connected.
    pub fn set_google_workspace_identity(&self, email: Option<String>) {
        self.google_workspace_integration_connected
            .store(true, Ordering::Relaxed);
        if let Ok(mut slot) = self.google_workspace_email.write() {
            *slot = email;
        }
    }

    /// Create and track a one-time OAuth state token.
    pub fn issue_google_oauth_state(&self) -> String {
        let state = Uuid::new_v4().to_string();
        self.google_oauth_states
            .insert(state.clone(), Instant::now());
        state
    }

    /// Validate and consume a pending OAuth state token.
    pub fn consume_google_oauth_state(&self, state: &str, max_age: Duration) -> bool {
        self.google_oauth_states
            .remove(state)
            .map(|(_, created_at)| created_at.elapsed() <= max_age)
            .unwrap_or(false)
    }

    /// Set runtime Google OAuth config from UI.
    pub fn set_google_oauth_runtime_config(&self, config: GoogleOAuthRuntimeConfig) {
        if let Ok(mut slot) = self.google_oauth_runtime_config.write() {
            *slot = Some(config);
        }
    }

    pub fn issue_codex_oauth_state(
        &self,
        state: String,
        code_verifier: String,
        redirect_uri: String,
        opener_origin: String,
    ) {
        self.codex_oauth_states.insert(
            state,
            CodexOAuthState {
                code_verifier,
                redirect_uri,
                opener_origin,
                created_at: Instant::now(),
            },
        );
    }

    pub fn consume_codex_oauth_state(
        &self,
        state: &str,
        max_age: Duration,
    ) -> Option<CodexOAuthState> {
        self.codex_oauth_states.remove(state).and_then(|(_, value)| {
            if value.created_at.elapsed() <= max_age {
                Some(value)
            } else {
                None
            }
        })
    }

    pub fn codex_oauth_target_origin(&self, state: &str) -> Option<String> {
        self.codex_oauth_states
            .get(state)
            .map(|pending| pending.opener_origin.clone())
    }

    /// Read runtime Google OAuth config from memory.
    pub fn google_oauth_runtime_config(&self) -> Option<GoogleOAuthRuntimeConfig> {
        self.google_oauth_runtime_config
            .read()
            .ok()
            .and_then(|cfg| cfg.clone())
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
        self.session_summaries.remove(&id);
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

    /// Return a cloned history snapshot for a session.
    pub fn session_history(&self, session_id: &str) -> Vec<ChatMessage> {
        self.sessions
            .get(session_id)
            .map(|s| s.history.clone())
            .unwrap_or_default()
    }

    /// Return the latest in-memory summary for a session, if any.
    pub fn session_summary(&self, session_id: &str) -> Option<String> {
        self.session_summaries
            .get(session_id)
            .map(|summary| summary.clone())
    }

    /// Update the in-memory summary for a session.
    pub fn update_session_summary(&self, session_id: &str, summary: &str) {
        if summary.trim().is_empty() {
            self.session_summaries.remove(session_id);
            return;
        }
        self.session_summaries
            .insert(session_id.to_string(), summary.to_string());
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
            }
        }

        if should_load
            && !loaded_history.is_empty()
            && let Some(mut session) = self.sessions.get_mut(session_id)
        {
            session.history = loaded_history;
        }
    }

    /// Append a user/assistant turn to in-memory state and persistent session storage.
    ///
    /// `channel_metadata` is an optional JSON object with channel-specific routing
    /// fields (e.g. `telegram_chat_id`, `discord_channel_id`) that get persisted
    /// alongside the continuity key so scheduled heartbeats can deliver responses.
    pub async fn persist_turn(
        &self,
        session_id: &str,
        channel_id: Option<&str>,
        user_id: Option<&str>,
        user_text: &str,
        assistant_text: &str,
        channel_metadata: Option<serde_json::Value>,
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
        let mut metadata = self
            .continuity_key(user_id)
            .map(|k| serde_json::json!({ "continuity_key": k }))
            .unwrap_or_else(|| serde_json::json!({}));

        if let Some(extra) = channel_metadata
            && let (Some(base), Some(extra_obj)) = (metadata.as_object_mut(), extra.as_object())
        {
            for (k, v) in extra_obj {
                base.insert(k.clone(), v.clone());
            }
        }

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

        // Keep summaries in sync with active sessions.
        self.session_summaries
            .retain(|session_id, _| self.sessions.contains_key(session_id));

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
