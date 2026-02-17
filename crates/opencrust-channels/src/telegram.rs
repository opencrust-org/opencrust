use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use teloxide::dispatching::UpdateFilterExt;
use teloxide::prelude::*;
use teloxide::types::ChatAction;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::traits::{Channel, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// Callback invoked when the bot receives a text message.
///
/// Arguments: `(chat_id, user_id_string, user_display_name, text)`.
/// Returns the assistant's reply text on success, or an error string.
/// Return `Err("__blocked__")` to silently drop the message (unauthorized user).
pub type OnMessageFn = Arc<
    dyn Fn(
            i64,
            String,
            String,
            String,
        ) -> Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send>>
        + Send
        + Sync,
>;

pub struct TelegramChannel {
    bot_token: String,
    display: String,
    status: ChannelStatus,
    on_message: OnMessageFn,
    bot: Option<Bot>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl TelegramChannel {
    pub fn new(bot_token: String, on_message: OnMessageFn) -> Self {
        Self {
            bot_token,
            display: "Telegram".to_string(),
            status: ChannelStatus::Disconnected,
            on_message,
            bot: None,
            shutdown_tx: None,
        }
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn channel_type(&self) -> &str {
        "telegram"
    }

    fn display_name(&self) -> &str {
        &self.display
    }

    async fn connect(&mut self) -> Result<()> {
        let bot = Bot::new(&self.bot_token);
        self.bot = Some(bot.clone());

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_tx = Some(shutdown_tx);

        let on_message = Arc::clone(&self.on_message);

        tokio::spawn(async move {
            let handler = Update::filter_message()
                .filter_map(|msg: teloxide::types::Message| {
                    let text = msg.text()?.to_string();
                    Some((msg, text))
                })
                .endpoint(
                    move |bot: Bot, (msg, text): (teloxide::types::Message, String)| {
                        let on_message = Arc::clone(&on_message);
                        async move {
                            let chat_id = msg.chat.id;
                            let user = msg.from.as_ref();
                            let user_id = user
                                .map(|u| u.id.0.to_string())
                                .unwrap_or_else(|| "unknown".to_string());
                            let user_name = user
                                .map(|u| u.first_name.clone())
                                .unwrap_or_else(|| "unknown".to_string());

                            info!(
                                "telegram message from {} [uid={}] (chat {}): {} chars",
                                user_name,
                                user_id,
                                chat_id,
                                text.len()
                            );

                            // Send typing indicator
                            let _ = bot.send_chat_action(chat_id, ChatAction::Typing).await;

                            match on_message(chat_id.0, user_id, user_name, text).await {
                                Ok(reply) => {
                                    if let Err(e) = bot.send_message(chat_id, reply).await {
                                        error!("failed to send telegram reply: {e}");
                                    }
                                }
                                Err(e) if e == "__blocked__" => {
                                    // Silently drop — unauthorized user
                                }
                                Err(e) => {
                                    warn!("agent error for telegram chat {}: {e}", chat_id);
                                    let _ = bot
                                        .send_message(
                                            chat_id,
                                            format!("Sorry, an error occurred: {e}"),
                                        )
                                        .await;
                                }
                            }

                            respond(())
                        }
                    },
                );

            let mut dispatcher = Dispatcher::builder(bot, handler)
                .default_handler(|upd| async move {
                    tracing::trace!("unhandled update: {:?}", upd.kind);
                })
                .build();

            let token = dispatcher.shutdown_token();
            tokio::spawn(async move {
                let mut rx = shutdown_rx;
                while rx.changed().await.is_ok() {
                    if *rx.borrow() {
                        if let Err(e) = token.shutdown() {
                            warn!("telegram shutdown token error: {e:?}");
                        }
                        break;
                    }
                }
            });

            info!("telegram bot polling started");
            dispatcher.dispatch().await;
            info!("telegram bot polling stopped");
        });

        self.status = ChannelStatus::Connected;
        info!("telegram channel connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        self.bot = None;
        self.status = ChannelStatus::Disconnected;
        info!("telegram channel disconnected");
        Ok(())
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        let bot = self
            .bot
            .as_ref()
            .ok_or_else(|| opencrust_common::Error::Channel("telegram bot not connected".into()))?;

        let chat_id: i64 = message
            .metadata
            .get("telegram_chat_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                opencrust_common::Error::Channel("missing telegram_chat_id in metadata".into())
            })?;

        let text = match &message.content {
            MessageContent::Text(t) => t.clone(),
            _ => {
                return Err(opencrust_common::Error::Channel(
                    "only text messages are supported for telegram send".into(),
                ));
            }
        };

        bot.send_message(ChatId(chat_id), text)
            .await
            .map_err(|e| opencrust_common::Error::Channel(format!("telegram send failed: {e}")))?;

        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_type_is_telegram() {
        let on_msg: OnMessageFn =
            Arc::new(|_chat_id, _uid, _user, _text| Box::pin(async { Ok("test".to_string()) }));
        let channel = TelegramChannel::new("fake-token".to_string(), on_msg);
        assert_eq!(channel.channel_type(), "telegram");
        assert_eq!(channel.display_name(), "Telegram");
        assert_eq!(channel.status(), ChannelStatus::Disconnected);
    }
}
