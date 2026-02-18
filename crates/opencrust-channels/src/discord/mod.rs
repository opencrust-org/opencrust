//! Discord channel implementation for OpenCrust.
//!
//! Provides a `DiscordChannel` struct that implements the `Channel` trait,
//! connecting to Discord via serenity and following the callback-driven
//! channel pattern used by Telegram/Slack.

pub mod commands;
pub mod config;
pub mod convert;
pub mod handler;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use opencrust_common::{Error, Message, Result};
use serenity::all::{self as serenity_model, CreateAttachment, CreateMessage};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info};

use crate::traits::{Channel, ChannelEvent, ChannelStatus};
use config::DiscordConfig;
use handler::DiscordHandler;

/// Callback invoked when the bot receives a text message from Discord.
///
/// Arguments: `(channel_id, user_id, user_name, text, delta_sender)`.
/// Return `Err("__blocked__")` to silently drop unauthorized messages.
pub type DiscordOnMessageFn = Arc<
    dyn Fn(
            String,
            String,
            String,
            String,
            Option<mpsc::Sender<String>>,
        ) -> Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send>>
        + Send
        + Sync,
>;

/// Discord channel implementation.
///
/// Manages a serenity client lifecycle and bridges Discord events into
/// the OpenCrust `ChannelEvent` system.
pub struct DiscordChannel {
    /// Discord-specific configuration.
    config: DiscordConfig,

    /// Current connection status.
    status: ChannelStatus,

    /// Callback used for incoming Discord text messages.
    on_message: DiscordOnMessageFn,

    /// Broadcast sender for channel events.
    event_tx: broadcast::Sender<ChannelEvent>,

    /// HTTP client for sending messages (available after connect).
    http: Option<std::sync::Arc<serenity_model::Http>>,

    /// Handle to the spawned client task.
    client_handle: Option<tokio::task::JoinHandle<()>>,

    /// Shard manager for graceful shutdown.
    shard_manager: Option<std::sync::Arc<serenity_model::ShardManager>>,
}

impl std::fmt::Debug for DiscordChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordChannel")
            .field("status", &self.status)
            .field("connected", &self.http.is_some())
            .finish()
    }
}
impl DiscordChannel {
    /// Create a new `DiscordChannel` from a `DiscordConfig`.
    pub fn new(config: DiscordConfig, on_message: DiscordOnMessageFn) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            config,
            status: ChannelStatus::Disconnected,
            on_message,
            event_tx,
            http: None,
            client_handle: None,
            shard_manager: None,
        }
    }

    /// Create a `DiscordChannel` from the generic `ChannelConfig` settings.
    pub fn from_settings(
        settings: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<Self> {
        let noop: DiscordOnMessageFn =
            Arc::new(|_channel_id, _user_id, _user_name, _text, _delta_tx| {
                Box::pin(async { Err("discord callback not configured".to_string()) })
            });
        Self::from_settings_with_callback(settings, noop)
    }

    /// Create a `DiscordChannel` from settings and an incoming-message callback.
    pub fn from_settings_with_callback(
        settings: &std::collections::HashMap<String, serde_json::Value>,
        on_message: DiscordOnMessageFn,
    ) -> Result<Self> {
        let config = DiscordConfig::from_settings(settings)?;
        Ok(Self::new(config, on_message))
    }

    /// Subscribe to channel events.
    ///
    /// Returns a broadcast receiver that will receive all `ChannelEvent`s
    /// emitted by this channel (messages, status changes, errors).
    pub fn subscribe(&self) -> broadcast::Receiver<ChannelEvent> {
        self.event_tx.subscribe()
    }

    /// Send a rich embed to a specific Discord channel.
    pub async fn send_embed(
        &self,
        discord_channel_id: u64,
        embed: serenity_model::CreateEmbed,
    ) -> Result<()> {
        let http = self
            .http
            .as_ref()
            .ok_or_else(|| Error::Channel("not connected to Discord".into()))?;

        let channel = serenity_model::ChannelId::new(discord_channel_id);
        let builder = CreateMessage::new().embed(embed);
        channel
            .send_message(http.as_ref(), builder)
            .await
            .map_err(|e| Error::Channel(format!("failed to send embed: {e}")))?;

        Ok(())
    }

    /// Send a file attachment to a specific Discord channel.
    pub async fn send_file(
        &self,
        discord_channel_id: u64,
        filename: impl Into<String>,
        data: Vec<u8>,
    ) -> Result<()> {
        let http = self
            .http
            .as_ref()
            .ok_or_else(|| Error::Channel("not connected to Discord".into()))?;

        let channel = serenity_model::ChannelId::new(discord_channel_id);
        let attachment = CreateAttachment::bytes(data, filename.into());
        let builder = CreateMessage::new().add_file(attachment);
        channel
            .send_message(http.as_ref(), builder)
            .await
            .map_err(|e| Error::Channel(format!("failed to send file: {e}")))?;

        Ok(())
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn channel_type(&self) -> &str {
        "discord"
    }

    fn display_name(&self) -> &str {
        "Discord"
    }

    async fn connect(&mut self) -> Result<()> {
        if matches!(self.status, ChannelStatus::Connected) {
            return Ok(());
        }

        self.status = ChannelStatus::Connecting;
        info!("connecting to Discord...");

        let handler = DiscordHandler::new(
            self.event_tx.clone(),
            "discord".to_string(),
            self.config.guild_ids.clone(),
            Arc::clone(&self.on_message),
        );

        let mut client =
            serenity_model::Client::builder(&self.config.bot_token, self.config.intents)
                .event_handler(handler)
                .await
                .map_err(|e| Error::Channel(format!("failed to build Discord client: {e}")))?;

        // Store the HTTP client for sending messages
        self.http = Some(client.http.clone());

        // Store the shard manager for graceful shutdown
        self.shard_manager = Some(client.shard_manager.clone());

        // Spawn the client in a background task
        let event_tx = self.event_tx.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = client.start().await {
                error!("Discord client error: {e}");
                let _ = event_tx.send(ChannelEvent::Error(format!("Discord client error: {e}")));
            }
        });

        self.client_handle = Some(handle);
        self.status = ChannelStatus::Connected;
        info!("Discord channel started");

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        if matches!(self.status, ChannelStatus::Disconnected) {
            return Ok(());
        }

        info!("disconnecting from Discord...");

        // Signal the shard manager to shut down
        if let Some(shard_manager) = self.shard_manager.take() {
            shard_manager.shutdown_all().await;
        }

        // Wait for the client task to finish
        if let Some(handle) = self.client_handle.take() {
            let _ = handle.await;
        }

        self.http = None;
        self.status = ChannelStatus::Disconnected;

        let _ = self
            .event_tx
            .send(ChannelEvent::StatusChanged(ChannelStatus::Disconnected));

        info!("Discord channel disconnected");
        Ok(())
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        let http = self
            .http
            .as_ref()
            .ok_or_else(|| Error::Channel("not connected to Discord".into()))?;

        // Extract the target Discord channel ID from metadata
        let discord_channel_id = message
            .metadata
            .get("discord_channel_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| {
                Error::Channel("message metadata must contain 'discord_channel_id' to send".into())
            })?;

        let channel = serenity_model::ChannelId::new(discord_channel_id);
        let text =
            convert::to_discord_markdown(&convert::opencrust_content_to_text(&message.content));
        let chunks = convert::split_discord_chunks(&text);
        for chunk in chunks {
            let builder = CreateMessage::new().content(chunk);
            channel
                .send_message(http.as_ref(), builder)
                .await
                .map_err(|e| Error::Channel(format!("failed to send message: {e}")))?;
        }

        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_config() -> DiscordConfig {
        DiscordConfig {
            bot_token: "test-token-not-real".to_string(),
            application_id: 123456789,
            guild_ids: vec![],
            intents: serenity_model::GatewayIntents::default(),
            prefix: None,
        }
    }

    #[test]
    fn new_channel_starts_disconnected() {
        let on_msg: DiscordOnMessageFn = Arc::new(|_ch, _uid, _user, _text, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let channel = DiscordChannel::new(test_config(), on_msg);
        assert_eq!(channel.status(), ChannelStatus::Disconnected);
    }

    #[test]
    fn channel_type_returns_discord() {
        let on_msg: DiscordOnMessageFn = Arc::new(|_ch, _uid, _user, _text, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let channel = DiscordChannel::new(test_config(), on_msg);
        assert_eq!(channel.channel_type(), "discord");
    }

    #[test]
    fn display_name_returns_discord() {
        let on_msg: DiscordOnMessageFn = Arc::new(|_ch, _uid, _user, _text, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let channel = DiscordChannel::new(test_config(), on_msg);
        assert_eq!(channel.display_name(), "Discord");
    }

    #[test]
    fn subscribe_returns_receiver() {
        let on_msg: DiscordOnMessageFn = Arc::new(|_ch, _uid, _user, _text, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let channel = DiscordChannel::new(test_config(), on_msg);
        let _rx = channel.subscribe();
        // Should not panic — validates broadcast channel is working
    }

    #[test]
    fn from_settings_with_valid_config() {
        let mut settings = HashMap::new();
        settings.insert("bot_token".to_string(), serde_json::json!("my-test-token"));
        settings.insert(
            "application_id".to_string(),
            serde_json::json!(123456789_u64),
        );

        let channel = DiscordChannel::from_settings(&settings).expect("should create channel");
        assert_eq!(channel.channel_type(), "discord");
        assert_eq!(channel.status(), ChannelStatus::Disconnected);
    }

    #[test]
    fn from_settings_without_token_fails() {
        let mut settings = HashMap::new();
        settings.insert(
            "application_id".to_string(),
            serde_json::json!(123456789_u64),
        );

        let err = DiscordChannel::from_settings(&settings).expect_err("should fail without token");
        assert!(err.to_string().contains("bot_token"));
    }

    #[tokio::test]
    async fn send_message_without_connection_fails() {
        let on_msg: DiscordOnMessageFn = Arc::new(|_ch, _uid, _user, _text, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let channel = DiscordChannel::new(test_config(), on_msg);
        let msg = opencrust_common::Message::text(
            opencrust_common::SessionId::from_string("test"),
            opencrust_common::ChannelId::from_string("discord"),
            opencrust_common::UserId::from_string("user"),
            opencrust_common::MessageDirection::Outgoing,
            "hello",
        );

        let err = channel
            .send_message(&msg)
            .await
            .expect_err("should fail when not connected");
        assert!(err.to_string().contains("not connected"));
    }
}
