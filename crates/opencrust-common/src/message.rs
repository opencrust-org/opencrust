use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{ChannelId, SessionId, UserId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: SessionId,
    pub channel_id: ChannelId,
    pub user_id: UserId,
    pub direction: MessageDirection,
    pub content: MessageContent,
    pub timestamp: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageContent {
    Text(String),
    Image { url: String, caption: Option<String> },
    Audio { url: String, duration_secs: Option<f64> },
    Video { url: String, caption: Option<String> },
    File { url: String, filename: String },
    Location { latitude: f64, longitude: f64 },
    Reaction { emoji: String, target_message_id: String },
    System(String),
}

impl Message {
    pub fn text(
        session_id: SessionId,
        channel_id: ChannelId,
        user_id: UserId,
        direction: MessageDirection,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id,
            channel_id,
            user_id,
            direction,
            content: MessageContent::Text(text.into()),
            timestamp: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }
}
