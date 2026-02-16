use async_trait::async_trait;
use opencrust_common::{Message, Result};
use serde::{Deserialize, Serialize};

/// Every messaging channel (Discord, Telegram, Slack, etc.) implements this trait.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Unique identifier for this channel type (e.g. "discord", "telegram").
    fn channel_type(&self) -> &str;

    /// Human-readable display name.
    fn display_name(&self) -> &str;

    /// Start the channel, connecting to the external service.
    async fn connect(&mut self) -> Result<()>;

    /// Gracefully disconnect from the external service.
    async fn disconnect(&mut self) -> Result<()>;

    /// Send a message through this channel.
    async fn send_message(&self, message: &Message) -> Result<()>;

    /// Current connection status.
    fn status(&self) -> ChannelStatus;
}

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
