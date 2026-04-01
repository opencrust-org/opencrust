pub mod chatdb;
pub mod sender;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::traits::{ChannelLifecycle, ChannelSender, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// Group filter closure for iMessage channels.
/// Argument: `is_mentioned` (always `false` - iMessage has no mention concept).
/// Returns `true` if the message should be processed.
pub type IMessageGroupFilter = Arc<dyn Fn(bool) -> bool + Send + Sync>;

/// Callback invoked when the bot receives a text message from iMessage.
///
/// Arguments: `(session_key, sender_id, text, is_group, delta_tx)`.
/// `delta_tx` is always `None` for iMessage (no streaming support).
/// Return `Err("__blocked__")` to silently drop the message (unauthorized user).
pub type IMessageOnMessageFn = Arc<
    dyn Fn(
            String,
            String,
            String,
            bool,
            Option<mpsc::Sender<String>>,
        ) -> Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send>>
        + Send
        + Sync,
>;

/// Maximum backoff duration on consecutive poll failures.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

pub struct IMessageChannel {
    poll_interval: Duration,
    status: ChannelStatus,
    on_message: IMessageOnMessageFn,
    group_filter: IMessageGroupFilter,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl IMessageChannel {
    pub fn new(poll_interval_secs: u64, on_message: IMessageOnMessageFn) -> Self {
        Self::with_group_filter(poll_interval_secs, on_message, Arc::new(|_| true))
    }

    pub fn with_group_filter(
        poll_interval_secs: u64,
        on_message: IMessageOnMessageFn,
        group_filter: IMessageGroupFilter,
    ) -> Self {
        Self {
            poll_interval: Duration::from_secs(poll_interval_secs),
            status: ChannelStatus::Disconnected,
            on_message,
            group_filter,
            shutdown_tx: None,
        }
    }
}

/// Lightweight send-only handle for iMessage. Stateless (uses osascript).
pub struct IMessageSender;

#[async_trait]
impl ChannelSender for IMessageSender {
    fn channel_type(&self) -> &str {
        "imessage"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        imessage_send_message(message).await
    }
}

#[async_trait]
impl ChannelLifecycle for IMessageChannel {
    fn display_name(&self) -> &str {
        "iMessage"
    }

    fn create_sender(&self) -> Box<dyn ChannelSender> {
        Box::new(IMessageSender)
    }

    async fn connect(&mut self) -> Result<()> {
        let db_path = chatdb::default_chat_db_path();
        let mut db = chatdb::ChatDb::open(&db_path).map_err(|e| {
            opencrust_common::Error::Channel(format!("imessage connect failed: {e}"))
        })?;

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        self.shutdown_tx = Some(shutdown_tx);

        let on_message = Arc::clone(&self.on_message);
        let group_filter = Arc::clone(&self.group_filter);
        let poll_interval = self.poll_interval;

        tokio::spawn(async move {
            info!(
                "imessage poll loop started (interval = {}s)",
                poll_interval.as_secs()
            );

            let mut consecutive_failures: u32 = 0;

            loop {
                let sleep_duration = if consecutive_failures == 0 {
                    poll_interval
                } else {
                    let backoff = poll_interval.saturating_mul(1 << consecutive_failures.min(5));
                    backoff.min(MAX_BACKOFF)
                };

                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration) => {}
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("imessage poll loop shutting down");
                            break;
                        }
                    }
                }

                match db.poll() {
                    Ok(messages) => {
                        if consecutive_failures > 0 {
                            info!(
                                "imessage: recovered after {consecutive_failures} consecutive failure(s)"
                            );
                            consecutive_failures = 0;
                        }

                        for msg in messages {
                            let is_group = msg.group_name.is_some();
                            let session_key = if let Some(ref group) = msg.group_name {
                                format!("imessage-group-{group}")
                            } else {
                                format!("imessage-{}", msg.sender)
                            };

                            // Apply group filter (no mention detection for iMessage)
                            if is_group && !group_filter(false) {
                                continue;
                            }

                            info!(
                                "imessage from {} ({} chars, rowid={}{}) session={}",
                                msg.sender,
                                msg.text.len(),
                                msg.rowid,
                                if is_group {
                                    format!(", group={}", msg.group_name.as_deref().unwrap_or(""))
                                } else {
                                    String::new()
                                },
                                session_key,
                            );

                            let on_message = Arc::clone(&on_message);
                            let sender = msg.sender.clone();
                            let text = msg.text;
                            let group_name = msg.group_name.clone();

                            tokio::spawn(async move {
                                // First arg = session key (group_name for groups, sender for DMs)
                                // Second arg = actual sender identity (always the person)
                                let session_key = if let Some(ref group) = group_name {
                                    group.clone()
                                } else {
                                    sender.clone()
                                };

                                let result =
                                    on_message(session_key, sender.clone(), text, is_group, None)
                                        .await;

                                match result {
                                    Ok(response) => {
                                        let response = crate::hints::format_hints(&response);
                                        let send_result = if let Some(ref group) = group_name {
                                            sender::send_imessage_group(group, &response).await
                                        } else {
                                            sender::send_imessage(&sender, &response).await
                                        };
                                        if let Err(e) = send_result {
                                            error!(
                                                "imessage: failed to send reply to {}: {e}",
                                                group_name.as_deref().unwrap_or(&sender)
                                            );
                                        }
                                    }
                                    Err(e) if e == "__blocked__" => {
                                        // Silently drop — unauthorized user
                                    }
                                    Err(e) => {
                                        warn!("imessage: agent error for {sender}: {e}");
                                        let error_msg = format!("Sorry, an error occurred: {e}");
                                        let send_result = if let Some(ref group) = group_name {
                                            sender::send_imessage_group(group, &error_msg).await
                                        } else {
                                            sender::send_imessage(&sender, &error_msg).await
                                        };
                                        let _ = send_result;
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        warn!("imessage: poll failed (attempt {consecutive_failures}): {e}");

                        // Try to reopen the database connection
                        match db.reopen() {
                            Ok(()) => {
                                info!("imessage: reopened chat.db after poll failure");
                            }
                            Err(reopen_err) => {
                                warn!("imessage: failed to reopen chat.db: {reopen_err}");
                            }
                        }
                    }
                }
            }

            info!("imessage poll loop stopped");
        });

        self.status = ChannelStatus::Connected;
        info!("imessage channel connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        self.status = ChannelStatus::Disconnected;
        info!("imessage channel disconnected");
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

#[async_trait]
impl ChannelSender for IMessageChannel {
    fn channel_type(&self) -> &str {
        "imessage"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        imessage_send_message(message).await
    }
}

/// Shared send logic used by both `IMessageChannel` and `IMessageSender`.
async fn imessage_send_message(message: &Message) -> Result<()> {
    let to = message
        .metadata
        .get("imessage_sender")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            opencrust_common::Error::Channel("missing imessage_sender in metadata".into())
        })?;

    let text = match &message.content {
        MessageContent::Text(t) => t.clone(),
        _ => {
            return Err(opencrust_common::Error::Channel(
                "only text messages are supported for imessage send".into(),
            ));
        }
    };

    sender::send_imessage(to, &text)
        .await
        .map_err(|e| opencrust_common::Error::Channel(format!("imessage send failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_type_is_imessage() {
        let on_msg: IMessageOnMessageFn = Arc::new(|_from, _user, _text, _is_group, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let channel = IMessageChannel::new(2, on_msg);
        assert_eq!(channel.channel_type(), "imessage");
        assert_eq!(channel.display_name(), "iMessage");
        assert_eq!(channel.status(), ChannelStatus::Disconnected);
    }

    #[test]
    fn imessage_group_filter_blocks() {
        let filter: IMessageGroupFilter = Arc::new(|_mentioned| false);
        assert!(!filter(false));
    }

    #[test]
    fn max_backoff_is_30s() {
        assert_eq!(MAX_BACKOFF, Duration::from_secs(30));
    }
}
