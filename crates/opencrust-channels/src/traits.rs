use async_trait::async_trait;
use opencrust_common::{Message, Result};
use serde::{Deserialize, Serialize};

/// Unified response type returned by every channel's `OnMessageFn`.
///
/// Each channel handler decides how to deliver the variants:
/// - `Text` — sent as a formatted chat message on all channels.
/// - `Voice` — `text` is persisted to history; `audio` (OGG/Opus bytes) is
///   delivered as a voice/audio message where the channel supports it.
///   Channels that cannot deliver audio (e.g. Slack) fall back to sending
///   the `text` field as a regular text message.
#[derive(Debug, Clone)]
pub enum ChannelResponse {
    /// Plain text response.
    Text(String),
    /// Voice response: `text` for history/fallback, `audio` for playback.
    Voice { text: String, audio: Vec<u8> },
}

impl ChannelResponse {
    /// The text content regardless of variant (used for persistence and fallback).
    pub fn text(&self) -> &str {
        match self {
            Self::Text(t) => t,
            Self::Voice { text, .. } => text,
        }
    }
}

/// Lifecycle management for a messaging channel (connect, disconnect, status).
#[async_trait]
pub trait ChannelLifecycle: Send {
    /// Human-readable display name.
    fn display_name(&self) -> &str;

    /// Start the channel, connecting to the external service.
    async fn connect(&mut self) -> Result<()>;

    /// Gracefully disconnect from the external service.
    async fn disconnect(&mut self) -> Result<()>;

    /// Current connection status.
    fn status(&self) -> ChannelStatus;

    /// Create a lightweight send-only handle for this channel.
    ///
    /// The returned sender is independent of the lifecycle and can be shared
    /// via `Arc` for scheduled message delivery while the channel runs its
    /// polling loop in a separate task.
    fn create_sender(&self) -> Box<dyn ChannelSender>;
}

/// Send-only interface for delivering outbound messages through a channel.
///
/// Designed to be wrapped in `Arc` and shared across tasks (e.g. the scheduler).
#[async_trait]
pub trait ChannelSender: Send + Sync {
    /// Unique identifier for this channel type.
    fn channel_type(&self) -> &str;

    /// Send a message through this channel.
    async fn send_message(&self, message: &Message) -> Result<()>;
}

/// Convenience trait combining lifecycle and send capabilities.
///
/// Kept for backward compatibility with `ChannelRegistry`.
pub trait Channel: ChannelLifecycle + ChannelSender {}
impl<T: ChannelLifecycle + ChannelSender> Channel for T {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Error(String),
}

#[derive(Debug, Clone)]
pub enum ChannelEvent {
    MessageReceived(Message),
    StatusChanged(ChannelStatus),
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_variant_returns_text() {
        let r = ChannelResponse::Text("hello".to_string());
        assert_eq!(r.text(), "hello");
    }

    #[test]
    fn voice_variant_returns_text_field() {
        let r = ChannelResponse::Voice {
            text: "spoken".to_string(),
            audio: vec![0u8, 1, 2],
        };
        assert_eq!(r.text(), "spoken");
    }

    #[test]
    fn voice_variant_text_independent_of_audio() {
        let r = ChannelResponse::Voice {
            text: "words".to_string(),
            audio: vec![],
        };
        assert_eq!(r.text(), "words");
    }
}
