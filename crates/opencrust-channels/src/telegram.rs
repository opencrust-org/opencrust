use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use teloxide::dispatching::UpdateFilterExt;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, InputFile, ParseMode};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::telegram_fmt::to_telegram_markdown;
use crate::traits::{Channel, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// Media attachment extracted from an incoming Telegram message.
#[derive(Debug, Clone)]
pub enum MediaAttachment {
    Photo {
        data: Vec<u8>,
        caption: Option<String>,
    },
    Document {
        data: Vec<u8>,
        filename: Option<String>,
        mime_type: Option<String>,
        caption: Option<String>,
    },
    Voice {
        data: Vec<u8>,
        duration: u32,
    },
}

/// Callback invoked when the bot receives a message.
///
/// Arguments: `(chat_id, user_id_string, user_display_name, text, attachment, delta_sender)`.
/// When `delta_sender` is `Some`, the callback should send text deltas through it
/// for streaming display. The callback still returns the final complete text.
/// Return `Err("__blocked__")` to silently drop the message (unauthorized user).
pub type OnMessageFn = Arc<
    dyn Fn(
            i64,
            String,
            String,
            String,
            Option<MediaAttachment>,
            Option<mpsc::Sender<String>>,
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

/// Download a file from Telegram by its file_id.
async fn download_telegram_file(
    bot: &Bot,
    file_id: &teloxide::types::FileId,
) -> std::result::Result<Vec<u8>, String> {
    let file = bot
        .get_file(file_id.clone())
        .await
        .map_err(|e| format!("telegram get_file failed: {e}"))?;

    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        bot.token(),
        file.path
    );

    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("telegram file download failed: {e}"))?;

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("telegram file read failed: {e}"))?;

    Ok(bytes.to_vec())
}

/// Extracts chat ID and user info from a message.
/// Returns None if the message should be ignored (e.g. from a bot or missing sender).
fn extract_message_info(msg: &teloxide::types::Message) -> Option<(i64, String, String)> {
    // Ignore messages without a sender (e.g. channel posts)
    let user = msg.from.as_ref()?;

    // Telegram "Group Anonymous Bot" ID used for anonymous admins.
    const ANONYMOUS_BOT_ID: u64 = 1087968824;

    // Ignore bots to prevent loops, but allow anonymous admins.
    if user.is_bot && user.id.0 != ANONYMOUS_BOT_ID {
        // Only log at debug/trace level to avoid spam, or warn if unexpected
        return None;
    }

    let chat_id = msg.chat.id;
    let user_id = user.id.0.to_string();
    let user_name = user.first_name.clone();

    Some((chat_id.0, user_id, user_name))
}

/// Extract text and optional media attachment from a Telegram message.
/// Returns None if the message type is unsupported.
async fn extract_content(
    bot: &Bot,
    msg: &teloxide::types::Message,
) -> Option<(String, Option<MediaAttachment>)> {
    // Photos (take the largest resolution)
    if let Some(photo) = msg.photo().and_then(|p| p.last()) {
        {
            let caption = msg.caption().map(|c| c.to_string());
            match download_telegram_file(bot, &photo.file.id).await {
                Ok(data) => {
                    let text = caption.clone().unwrap_or_default();
                    return Some((text, Some(MediaAttachment::Photo { data, caption })));
                }
                Err(e) => {
                    warn!("telegram: failed to download photo: {e}");
                    return None;
                }
            }
        }
    }

    // Documents
    if let Some(doc) = msg.document() {
        let caption = msg.caption().map(|c| c.to_string());
        match download_telegram_file(bot, &doc.file.id).await {
            Ok(data) => {
                let text = caption.clone().unwrap_or_default();
                let filename = doc.file_name.clone();
                let mime_type = doc.mime_type.as_ref().map(|m| m.to_string());
                return Some((
                    text,
                    Some(MediaAttachment::Document {
                        data,
                        filename,
                        mime_type,
                        caption,
                    }),
                ));
            }
            Err(e) => {
                warn!("telegram: failed to download document: {e}");
                return None;
            }
        }
    }

    // Voice messages
    if let Some(voice) = msg.voice() {
        match download_telegram_file(bot, &voice.file.id).await {
            Ok(data) => {
                let duration = voice.duration.seconds();
                return Some((
                    String::new(),
                    Some(MediaAttachment::Voice { data, duration }),
                ));
            }
            Err(e) => {
                warn!("telegram: failed to download voice: {e}");
                return None;
            }
        }
    }

    // Plain text
    if let Some(text) = msg.text() {
        return Some((text.to_string(), None));
    }

    // Unsupported message type
    None
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
            let handler = Update::filter_message().endpoint(
                move |bot: Bot, msg: teloxide::types::Message| {
                    let on_message = Arc::clone(&on_message);
                    async move {
                        let (chat_id_raw, user_id, user_name) =
                            match extract_message_info(&msg) {
                                Some(info) => info,
                                None => return respond(()),
                            };

                        // Extract content (text + optional media)
                        let (text, attachment) = match extract_content(&bot, &msg).await {
                            Some(content) => content,
                            None => return respond(()),
                        };

                        // ChatId wrapper for teloxide calls
                        let chat_id = ChatId(chat_id_raw);

                        let kind = match &attachment {
                            Some(MediaAttachment::Photo { .. }) => "photo",
                            Some(MediaAttachment::Document { .. }) => "document",
                            Some(MediaAttachment::Voice { .. }) => "voice",
                            None => "text",
                        };
                        info!(
                            "telegram {kind} from {} [uid={}] (chat {}): {} chars",
                            user_name,
                            user_id,
                            chat_id,
                            text.len()
                        );

                        // Send typing indicator
                        let _ = bot.send_chat_action(chat_id, ChatAction::Typing).await;

                        // Create streaming channel
                        let (delta_tx, mut delta_rx) = mpsc::channel::<String>(64);

                        // Spawn callback
                        let callback_handle = tokio::spawn({
                            let on_message = Arc::clone(&on_message);
                            let user_id = user_id.clone();
                            let user_name = user_name.clone();
                            let text = text.clone();
                            async move {
                                on_message(
                                    chat_id.0,
                                    user_id,
                                    user_name,
                                    text,
                                    attachment,
                                    Some(delta_tx),
                                )
                                .await
                            }
                        });

                        // Consume streaming deltas and edit message.
                        // Buffer for 1s before sending the first message so short
                        // responses appear as a single formatted message instead of
                        // flashing the first word then replacing it.
                        let mut accumulated = String::new();
                        let mut msg_id: Option<teloxide::types::MessageId> = None;
                        let mut last_edit = tokio::time::Instant::now();
                        let mut first_delta_at: Option<tokio::time::Instant> = None;

                        loop {
                            tokio::select! {
                                delta = delta_rx.recv() => {
                                    match delta {
                                        Some(text) => {
                                            accumulated.push_str(&text);
                                            if first_delta_at.is_none() {
                                                first_delta_at = Some(tokio::time::Instant::now());
                                            }

                                            if msg_id.is_none() {
                                                // Only send after 1s buffer period
                                                if first_delta_at.unwrap().elapsed() >= Duration::from_secs(1) {
                                                    match bot.send_message(chat_id, &accumulated).await {
                                                        Ok(sent) => {
                                                            msg_id = Some(sent.id);
                                                            last_edit = tokio::time::Instant::now();
                                                        }
                                                        Err(e) => {
                                                            error!("failed to send streaming message: {e}");
                                                            break;
                                                        }
                                                    }
                                                }
                                            } else if last_edit.elapsed() >= Duration::from_millis(1000)
                                                && let Some(id) = msg_id
                                            {
                                                let _ = bot
                                                    .edit_message_text(chat_id, id, &accumulated)
                                                    .await;
                                                last_edit = tokio::time::Instant::now();
                                            }
                                        }
                                        None => break, // Sender dropped - callback finished
                                    }
                                }
                                _ = tokio::time::sleep(Duration::from_secs(4)) => {
                                    // Keep typing indicator alive during pauses (e.g. tool execution)
                                    let _ = bot.send_chat_action(chat_id, ChatAction::Typing).await;
                                }
                            }
                        }

                        // Get callback result
                        let result = callback_handle
                            .await
                            .unwrap_or_else(|e| Err(format!("task panic: {e}")));

                        match result {
                            Ok(final_text) => {
                                if let Some(id) = msg_id {
                                    // Final edit with MarkdownV2 formatting
                                    let formatted = to_telegram_markdown(&final_text);
                                    let edit_result = bot
                                        .edit_message_text(chat_id, id, &formatted)
                                        .parse_mode(ParseMode::MarkdownV2)
                                        .await;
                                    if edit_result.is_err() {
                                        // Fallback: plain text
                                        let _ = bot
                                            .edit_message_text(chat_id, id, &final_text)
                                            .await;
                                    }
                                } else {
                                    // No streaming happened (command response) - send directly
                                    let formatted = to_telegram_markdown(&final_text);
                                    let send_result = bot
                                        .send_message(chat_id, &formatted)
                                        .parse_mode(ParseMode::MarkdownV2)
                                        .await;
                                    if send_result.is_err() {
                                        // Fallback: plain text
                                        let _ =
                                            bot.send_message(chat_id, &final_text).await;
                                    }
                                }
                            }
                            Err(e) if e == "__blocked__" => {
                                // Silently drop - unauthorized user
                            }
                            Err(e) => {
                                if let Some(id) = msg_id {
                                    let _ = bot
                                        .edit_message_text(
                                            chat_id,
                                            id,
                                            format!("Sorry, an error occurred: {e}"),
                                        )
                                        .await;
                                } else {
                                    warn!(
                                        "agent error for telegram chat {}: {e}",
                                        chat_id
                                    );
                                    let _ = bot
                                        .send_message(
                                            chat_id,
                                            format!("Sorry, an error occurred: {e}"),
                                        )
                                        .await;
                                }
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

        let tg_chat_id = ChatId(chat_id);

        match &message.content {
            MessageContent::Text(text) => {
                let formatted = to_telegram_markdown(text);
                let send_result = bot
                    .send_message(tg_chat_id, &formatted)
                    .parse_mode(ParseMode::MarkdownV2)
                    .await;
                if send_result.is_err() {
                    // Fallback: plain text
                    bot.send_message(tg_chat_id, text).await.map_err(|e| {
                        opencrust_common::Error::Channel(format!("telegram send failed: {e}"))
                    })?;
                }
            }
            MessageContent::Image { url, caption } => {
                bot.send_photo(
                    tg_chat_id,
                    InputFile::url(url.parse().map_err(|e| {
                        opencrust_common::Error::Channel(format!("invalid image url: {e}"))
                    })?),
                )
                .caption(caption.as_deref().unwrap_or(""))
                .await
                .map_err(|e| {
                    opencrust_common::Error::Channel(format!("telegram send_photo failed: {e}"))
                })?;
            }
            MessageContent::File { url, filename } => {
                bot.send_document(
                    tg_chat_id,
                    InputFile::url(url.parse().map_err(|e| {
                        opencrust_common::Error::Channel(format!("invalid file url: {e}"))
                    })?),
                )
                .caption(filename)
                .await
                .map_err(|e| {
                    opencrust_common::Error::Channel(format!("telegram send_document failed: {e}"))
                })?;
            }
            _ => {
                return Err(opencrust_common::Error::Channel(
                    "unsupported message content type for telegram send".into(),
                ));
            }
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

    #[test]
    fn channel_type_is_telegram() {
        let on_msg: OnMessageFn =
            Arc::new(|_chat_id, _uid, _user, _text, _attachment, _delta_tx| {
                Box::pin(async { Ok("test".to_string()) })
            });
        let channel = TelegramChannel::new("fake-token".to_string(), on_msg);
        assert_eq!(channel.channel_type(), "telegram");
        assert_eq!(channel.display_name(), "Telegram");
        assert_eq!(channel.status(), ChannelStatus::Disconnected);
    }

    #[test]
    fn test_extract_message_info_private() {
        // Construct a private message JSON
        let json = r#"{
            "message_id": 1,
            "date": 1620000000,
            "chat": {
                "id": 12345,
                "type": "private",
                "first_name": "Alice"
            },
            "from": {
                "id": 111,
                "is_bot": false,
                "first_name": "Alice",
                "username": "alice"
            },
            "text": "hello"
        }"#;
        let msg: teloxide::types::Message =
            serde_json::from_str(json).expect("failed to parse json");

        let info = extract_message_info(&msg).expect("should extract info");
        assert_eq!(info.0, 12345);
        assert_eq!(info.1, "111");
        assert_eq!(info.2, "Alice");
    }

    #[test]
    fn test_extract_message_info_group() {
        // Construct a group message JSON (negative chat_id)
        let json = r#"{
            "message_id": 2,
            "date": 1620000000,
            "chat": {
                "id": -987654321,
                "type": "supergroup",
                "title": "My Group"
            },
            "from": {
                "id": 222,
                "is_bot": false,
                "first_name": "Bob"
            },
            "text": "hello group"
        }"#;
        let msg: teloxide::types::Message =
            serde_json::from_str(json).expect("failed to parse json");

        let info = extract_message_info(&msg).expect("should extract info");
        assert_eq!(info.0, -987654321);
        assert_eq!(info.1, "222");
        assert_eq!(info.2, "Bob");
    }

    #[test]
    fn test_extract_message_info_bot_ignored() {
        // Message from a bot
        let json = r#"{
            "message_id": 3,
            "date": 1620000000,
            "chat": {
                "id": 12345,
                "type": "private"
            },
            "from": {
                "id": 333,
                "is_bot": true,
                "first_name": "SomeBot"
            },
            "text": "I am a bot"
        }"#;
        let msg: teloxide::types::Message =
            serde_json::from_str(json).expect("failed to parse json");

        let info = extract_message_info(&msg);
        assert!(info.is_none(), "should ignore bot messages");
    }

    #[test]
    fn test_extract_message_info_anonymous_admin_allowed() {
        // Message from Group Anonymous Bot (ID 1087968824)
        let json = r#"{
            "message_id": 5,
            "date": 1620000000,
            "chat": {
                "id": -987654321,
                "type": "supergroup",
                "title": "My Group"
            },
            "from": {
                "id": 1087968824,
                "is_bot": true,
                "first_name": "Group Anonymous Bot",
                "username": "GroupAnonymousBot"
            },
            "sender_chat": {
                 "id": -987654321,
                 "type": "supergroup",
                 "title": "My Group"
            },
            "text": "admin command"
        }"#;
        let msg: teloxide::types::Message =
            serde_json::from_str(json).expect("failed to parse json");

        let info = extract_message_info(&msg).expect("should allow anonymous admin");
        assert_eq!(info.0, -987654321);
        assert_eq!(info.1, "1087968824");
        assert_eq!(info.2, "Group Anonymous Bot");
    }

    #[test]
    fn test_extract_message_info_channel_post_ignored() {
        // Channel post often lacks 'from' or behaves differently.
        // If we simulate a message without 'from' (if possible in teloxide types).
        // Standard messages usually have 'from', but let's try to omit it.
        // teloxide::types::Message 'from' is Option<User>.
        let json = r#"{
            "message_id": 4,
            "date": 1620000000,
            "chat": {
                "id": -1001234567890,
                "type": "channel",
                "title": "My Channel"
            },
            "text": "channel post"
        }"#;
        let msg: teloxide::types::Message =
            serde_json::from_str(json).expect("failed to parse json");

        let info = extract_message_info(&msg);
        assert!(
            info.is_none(),
            "should ignore messages without sender (channel posts)"
        );
    }
}
