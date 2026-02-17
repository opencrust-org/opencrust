use std::sync::Arc;

use dashmap::DashMap;
use opencrust_agents::{AgentRuntime, ChatMessage};
use opencrust_channels::ChannelRegistry;
use opencrust_config::AppConfig;
use uuid::Uuid;

/// Shared application state accessible from all request handlers.
pub struct AppState {
    pub config: AppConfig,
    pub channels: ChannelRegistry,
    pub agents: AgentRuntime,
    pub sessions: DashMap<String, SessionState>,
}

/// Per-connection session tracking.
pub struct SessionState {
    pub id: String,
    pub user_id: Option<String>,
    pub channel_id: Option<String>,
    pub history: Vec<ChatMessage>,
}

impl AppState {
    pub fn new(config: AppConfig, agents: AgentRuntime) -> Self {
        Self {
            config,
            channels: ChannelRegistry::new(),
            agents,
            sessions: DashMap::new(),
        }
    }

    pub fn create_session(&self) -> String {
        let id = Uuid::new_v4().to_string();
        self.sessions.insert(
            id.clone(),
            SessionState {
                id: id.clone(),
                user_id: None,
                channel_id: None,
                history: Vec::new(),
            },
        );
        id
    }
}

pub type SharedState = Arc<AppState>;
