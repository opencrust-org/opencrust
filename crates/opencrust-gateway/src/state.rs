use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use opencrust_agents::{AgentRuntime, ChatMessage};
use opencrust_channels::ChannelRegistry;
use opencrust_config::{
    AppConfig,
    model::{GuardrailsConfig, RateLimitConfig},
};
use opencrust_db::SessionStore;
use tokio::sync::{Mutex, watch};
use tracing::{info, warn};
use uuid::Uuid;

/// How long a disconnected session is kept for resume.
const SESSION_TTL: Duration = Duration::from_secs(3600); // 1 hour
/// How often the cleanup task runs.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(300); // 5 minutes
/// Sliding window for per-user rate limiting.
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// Per-user rate limit tracking entry.
struct UserRateLimitEntry {
    /// Timestamps of recent messages within the sliding window.
    timestamps: VecDeque<Instant>,
    /// If set, the user is in cooldown until this instant.
    cooldown_until: Option<Instant>,
}

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
    /// Runtime Google OAuth client configuration set from the web UI.
    google_oauth_runtime_config: RwLock<Option<GoogleOAuthRuntimeConfig>>,
    /// Receives hot-reloaded config updates. `None` if watcher is not active.
    config_rx: Option<watch::Receiver<AppConfig>>,
    /// Per-user rate limit state, keyed by user_id.
    user_rate_limits: DashMap<String, UserRateLimitEntry>,
    /// Accumulated token counts per session (input + output), reset on session eviction.
    session_token_counts: DashMap<String, u32>,
}

#[derive(Debug, Clone)]
pub struct GoogleOAuthRuntimeConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: Option<String>,
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
            google_oauth_runtime_config: RwLock::new(None),
            config_rx: None,
            user_rate_limits: DashMap::new(),
            session_token_counts: DashMap::new(),
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

    /// Read runtime Google OAuth config from memory.
    pub fn google_oauth_runtime_config(&self) -> Option<GoogleOAuthRuntimeConfig> {
        self.google_oauth_runtime_config
            .read()
            .ok()
            .and_then(|cfg| cfg.clone())
    }

    /// Check and update per-user rate limits.
    ///
    /// Returns `Err` with a human-readable message if the user should be throttled.
    /// On success, records the current message timestamp for future checks.
    ///
    /// Logic:
    /// 1. If user is in cooldown → reject immediately.
    /// 2. Evict timestamps older than 60 s from the sliding window.
    /// 3. If the last `per_user_burst` messages all arrived within 1 s and
    ///    `cooldown_secs > 0` → place user in cooldown and reject.
    /// 4. If message count in window >= `per_user_per_minute` → reject.
    /// 5. Record timestamp and allow.
    pub fn check_user_rate_limit(
        &self,
        user_id: &str,
        config: &RateLimitConfig,
    ) -> std::result::Result<(), String> {
        let now = Instant::now();

        let mut entry = self
            .user_rate_limits
            .entry(user_id.to_string())
            .or_insert_with(|| UserRateLimitEntry {
                timestamps: VecDeque::new(),
                cooldown_until: None,
            });

        // 1. Active cooldown check.
        if let Some(until) = entry.cooldown_until {
            if now < until {
                let remaining = (until - now).as_secs() + 1;
                return Err(format!(
                    "rate limit: please wait {remaining}s before sending another message"
                ));
            }
            entry.cooldown_until = None;
        }

        // 2. Evict timestamps outside the sliding window.
        while entry
            .timestamps
            .front()
            .map(|t| now.duration_since(*t) > RATE_LIMIT_WINDOW)
            .unwrap_or(false)
        {
            entry.timestamps.pop_front();
        }

        // 3. Burst detection → cooldown.
        let burst = config.per_user_burst as usize;
        if config.cooldown_secs > 0 && burst > 0 && entry.timestamps.len() >= burst {
            let oldest_burst = entry.timestamps[entry.timestamps.len() - burst];
            if now.duration_since(oldest_burst) < Duration::from_secs(1) {
                entry.cooldown_until = Some(now + Duration::from_secs(config.cooldown_secs as u64));
                return Err(format!(
                    "rate limit: too many messages too fast — please wait {}s",
                    config.cooldown_secs
                ));
            }
        }

        // 4. Per-minute cap.
        if entry.timestamps.len() >= config.per_user_per_minute as usize {
            return Err(format!(
                "rate limit: you have sent too many messages — maximum {} per minute",
                config.per_user_per_minute
            ));
        }

        // 5. Record and allow.
        entry.timestamps.push_back(now);
        Ok(())
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

    /// Persist token usage for a completed agent turn to the session store.
    ///
    /// Also increments the in-memory session token counter used for budget checks.
    pub async fn persist_usage(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) {
        // Track in-memory session token total for fast budget checks.
        let total = input_tokens.saturating_add(output_tokens);
        self.session_token_counts
            .entry(session_id.to_string())
            .and_modify(|c| *c = c.saturating_add(total))
            .or_insert(total);

        let Some(store) = &self.session_store else {
            return;
        };

        // Look up user_id and channel_id from the session for attribution.
        let (user_id, channel_id) = self
            .sessions
            .get(session_id)
            .map(|s| {
                (
                    s.user_id.clone().unwrap_or_else(|| "anonymous".to_string()),
                    s.channel_id
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            })
            .unwrap_or_else(|| ("anonymous".to_string(), "unknown".to_string()));

        let guard = store.lock().await;
        if let Err(e) = guard.record_usage(
            session_id,
            opencrust_db::UsageAttribution {
                user_id: &user_id,
                channel_id: &channel_id,
                provider,
                model,
            },
            input_tokens,
            output_tokens,
        ) {
            warn!("failed to record usage for session {session_id}: {e}");
        }
    }

    /// Check token budgets before processing a message.
    ///
    /// Checks (in order):
    /// 1. Session token budget (in-memory, fast)
    /// 2. User daily token budget (DB query)
    /// 3. User monthly token budget (DB query)
    ///
    /// Returns `Err` with a human-readable message if any budget is exceeded.
    pub async fn check_token_budget(
        &self,
        session_id: &str,
        user_id: &str,
        config: &GuardrailsConfig,
    ) -> std::result::Result<(), String> {
        // 1. Session budget (in-memory).
        if let Some(budget) = config.token_budget_session {
            let used = self
                .session_token_counts
                .get(session_id)
                .map(|c| *c)
                .unwrap_or(0);
            if used >= budget {
                return Err(format!(
                    "token budget exceeded: this session has used {used} tokens (limit: {budget})"
                ));
            }
        }

        // 2 & 3. User daily / monthly budgets (DB query).
        let needs_db =
            config.token_budget_user_daily.is_some() || config.token_budget_user_monthly.is_some();

        if needs_db {
            let Some(store) = &self.session_store else {
                return Ok(());
            };
            let guard = store.lock().await;

            if let Some(daily_budget) = config.token_budget_user_daily {
                match guard.query_usage_for_user(user_id, Some("today")) {
                    Ok(usage) if usage.total_tokens >= daily_budget as u64 => {
                        return Err(format!(
                            "token budget exceeded: you have used {} tokens today (daily limit: {daily_budget})",
                            usage.total_tokens
                        ));
                    }
                    Err(e) => warn!("failed to query daily usage for {user_id}: {e}"),
                    _ => {}
                }
            }

            if let Some(monthly_budget) = config.token_budget_user_monthly {
                match guard.query_usage_for_user(user_id, Some("month")) {
                    Ok(usage) if usage.total_tokens >= monthly_budget as u64 => {
                        return Err(format!(
                            "token budget exceeded: you have used {} tokens this month (monthly limit: {monthly_budget})",
                            usage.total_tokens
                        ));
                    }
                    Err(e) => warn!("failed to query monthly usage for {user_id}: {e}"),
                    _ => {}
                }
            }
        }

        Ok(())
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

        // Keep summaries and token counts in sync with active sessions.
        self.session_summaries
            .retain(|session_id, _| self.sessions.contains_key(session_id));
        self.session_token_counts
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

    fn rate_limit_config(per_minute: u32, burst: u32, cooldown: u32) -> RateLimitConfig {
        RateLimitConfig {
            per_second: 1,
            burst_size: 60,
            per_user_per_minute: per_minute,
            per_user_burst: burst,
            cooldown_secs: cooldown,
        }
    }

    #[test]
    fn rate_limit_allows_messages_within_limit() {
        let state = test_state();
        let cfg = rate_limit_config(5, 3, 0);
        for _ in 0..5 {
            assert!(state.check_user_rate_limit("user1", &cfg).is_ok());
        }
    }

    #[test]
    fn rate_limit_rejects_when_per_minute_exceeded() {
        let state = test_state();
        let cfg = rate_limit_config(3, 10, 0);
        for _ in 0..3 {
            assert!(state.check_user_rate_limit("user1", &cfg).is_ok());
        }
        let result = state.check_user_rate_limit("user1", &cfg);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too many messages"));
    }

    #[test]
    fn rate_limit_tracks_users_independently() {
        let state = test_state();
        let cfg = rate_limit_config(2, 10, 0);
        assert!(state.check_user_rate_limit("alice", &cfg).is_ok());
        assert!(state.check_user_rate_limit("alice", &cfg).is_ok());
        assert!(state.check_user_rate_limit("alice", &cfg).is_err());
        // bob is unaffected
        assert!(state.check_user_rate_limit("bob", &cfg).is_ok());
    }

    #[test]
    fn rate_limit_cooldown_blocks_after_burst() {
        let state = test_state();
        // burst=2, cooldown=30: send 2 messages in <1s → cooldown triggered
        let cfg = rate_limit_config(20, 2, 30);
        assert!(state.check_user_rate_limit("user1", &cfg).is_ok());
        assert!(state.check_user_rate_limit("user1", &cfg).is_ok());
        // Third message triggers burst detection (2 msgs in <1s)
        let result = state.check_user_rate_limit("user1", &cfg);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("wait") && err.contains("30s"));
        // Subsequent message is also blocked by cooldown
        assert!(state.check_user_rate_limit("user1", &cfg).is_err());
    }

    #[test]
    fn continuity_key_with_shared_continuity_disabled() {
        let mut config = AppConfig::default();
        config.memory.shared_continuity = false;
        let state = AppState::new(config, AgentRuntime::new(), ChannelRegistry::new());
        let key = state.continuity_key(Some("user1"));
        assert_eq!(key, None);
    }

    fn guardrails_config(
        session_budget: Option<u32>,
        daily_budget: Option<u32>,
        monthly_budget: Option<u32>,
    ) -> opencrust_config::model::GuardrailsConfig {
        opencrust_config::model::GuardrailsConfig {
            token_budget_session: session_budget,
            token_budget_user_daily: daily_budget,
            token_budget_user_monthly: monthly_budget,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn token_budget_allows_when_no_limits_set() {
        let state = test_state();
        let cfg = guardrails_config(None, None, None);
        assert!(state.check_token_budget("s1", "user1", &cfg).await.is_ok());
    }

    #[tokio::test]
    async fn token_budget_session_rejects_when_exceeded() {
        let state = test_state();
        let cfg = guardrails_config(Some(100), None, None);

        // Simulate 100 tokens used in session
        state.session_token_counts.insert("s1".to_string(), 100);

        let result = state.check_token_budget("s1", "user1", &cfg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("session has used 100 tokens"));
    }

    #[tokio::test]
    async fn token_budget_session_allows_when_under_limit() {
        let state = test_state();
        let cfg = guardrails_config(Some(100), None, None);

        state.session_token_counts.insert("s1".to_string(), 99);
        assert!(state.check_token_budget("s1", "user1", &cfg).await.is_ok());
    }

    #[tokio::test]
    async fn token_budget_session_token_counts_increment_on_persist() {
        let state = test_state();
        // No session store — persist_usage still increments in-memory counter
        state
            .persist_usage("s1", "anthropic", "claude", 60, 40)
            .await;
        let count = state
            .session_token_counts
            .get("s1")
            .map(|c| *c)
            .unwrap_or(0);
        assert_eq!(count, 100);

        state
            .persist_usage("s1", "anthropic", "claude", 30, 20)
            .await;
        let count = state
            .session_token_counts
            .get("s1")
            .map(|c| *c)
            .unwrap_or(0);
        assert_eq!(count, 150);
    }

    #[tokio::test]
    async fn token_budget_cleanup_removes_evicted_session_counts() {
        let state = Arc::new(test_state());
        let id = state.create_session();
        state.session_token_counts.insert(id.clone(), 500);

        state.disconnect_session(&id);
        if let Some(mut session) = state.sessions.get_mut(&id) {
            session.last_active = Instant::now() - Duration::from_secs(7200);
        }
        state.cleanup_expired_sessions();

        assert!(!state.session_token_counts.contains_key(&id));
    }
}
