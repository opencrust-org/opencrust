pub mod api;
pub mod fmt;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::StreamExt;
use reqwest::Client;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::traits::{Channel, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// Callback invoked when the bot receives a text message from Slack.
///
/// Arguments: `(channel_id, user_id, user_name, text, delta_sender)`.
/// When `delta_sender` is `Some`, the callback should send text deltas through it
/// for streaming display. The callback still returns the final complete text.
/// Return `Err("__blocked__")` to silently drop the message (unauthorized user).
pub type SlackOnMessageFn = Arc<
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

pub struct SlackChannel {
    bot_token: String,
    app_token: String,
    display: String,
    status: ChannelStatus,
    on_message: SlackOnMessageFn,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl SlackChannel {
    pub fn new(bot_token: String, app_token: String, on_message: SlackOnMessageFn) -> Self {
        Self {
            bot_token,
            app_token,
            display: "Slack".to_string(),
            status: ChannelStatus::Disconnected,
            on_message,
            shutdown_tx: None,
        }
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn channel_type(&self) -> &str {
        "slack"
    }

    fn display_name(&self) -> &str {
        &self.display
    }

    async fn connect(&mut self) -> Result<()> {
        let client = Client::new();
        let bot_token = self.bot_token.clone();
        let app_token = self.app_token.clone();
        let on_message = Arc::clone(&self.on_message);

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_tx = Some(shutdown_tx);

        tokio::spawn(async move {
            run_socket_mode(client, bot_token, app_token, on_message, shutdown_rx).await;
        });

        self.status = ChannelStatus::Connected;
        info!("slack channel connected (Socket Mode)");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        self.status = ChannelStatus::Disconnected;
        info!("slack channel disconnected");
        Ok(())
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        let channel_id = message
            .metadata
            .get("slack_channel_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                opencrust_common::Error::Channel("missing slack_channel_id in metadata".into())
            })?;

        let text = match &message.content {
            MessageContent::Text(t) => t.clone(),
            _ => {
                return Err(opencrust_common::Error::Channel(
                    "only text messages are supported for slack send".into(),
                ));
            }
        };

        let client = Client::new();
        let formatted = fmt::to_slack_mrkdwn(&text);
        api::post_message(&client, &self.bot_token, channel_id, &formatted)
            .await
            .map_err(|e| opencrust_common::Error::Channel(format!("slack send failed: {e}")))?;

        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

/// Main Socket Mode event loop with automatic reconnection.
async fn run_socket_mode(
    client: Client,
    bot_token: String,
    app_token: String,
    on_message: SlackOnMessageFn,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        if *shutdown_rx.borrow() {
            info!("slack: shutdown requested, stopping Socket Mode");
            return;
        }

        let ws_url = match api::open_connection(&client, &app_token).await {
            Ok(url) => url,
            Err(e) => {
                warn!("slack: failed to open connection: {e}, retrying in 5s");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    _ = shutdown_rx.changed() => return,
                }
            }
        };

        info!("slack: connecting to Socket Mode WebSocket");
        let ws_stream = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((stream, _)) => stream,
            Err(e) => {
                warn!("slack: WebSocket connect failed: {e}, retrying in 5s");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    _ = shutdown_rx.changed() => return,
                }
            }
        };

        let (ws_write, mut ws_read) = ws_stream.split();
        let ws_write = Arc::new(tokio::sync::Mutex::new(ws_write));

        info!("slack: Socket Mode WebSocket connected");

        let should_reconnect;

        loop {
            tokio::select! {
                msg = ws_read.next() => {
                    match msg {
                        Some(Ok(ws_msg)) => {
                            if let tokio_tungstenite::tungstenite::Message::Text(text) = ws_msg {
                                let handled = handle_socket_event(
                                    &text,
                                    &client,
                                    &bot_token,
                                    &on_message,
                                    &ws_write,
                                ).await;
                                if let HandleResult::Reconnect = handled {
                                    should_reconnect = true;
                                    break;
                                }
                            }
                        }
                        Some(Err(e)) => {
                            warn!("slack: WebSocket error: {e}");
                            should_reconnect = true;
                            break;
                        }
                        None => {
                            info!("slack: WebSocket stream ended");
                            should_reconnect = true;
                            break;
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("slack: shutdown during read loop");
                        return;
                    }
                }
            }
        }

        if !should_reconnect || *shutdown_rx.borrow() {
            return;
        }

        info!("slack: reconnecting in 2s...");
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {},
            _ = shutdown_rx.changed() => return,
        }
    }
}

enum HandleResult {
    Ok,
    Reconnect,
}

type WsWriter = Arc<
    tokio::sync::Mutex<
        futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            tokio_tungstenite::tungstenite::Message,
        >,
    >,
>;

async fn handle_socket_event(
    raw: &str,
    client: &Client,
    bot_token: &str,
    on_message: &SlackOnMessageFn,
    ws_write: &WsWriter,
) -> HandleResult {
    let envelope: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            warn!("slack: failed to parse event: {e}");
            return HandleResult::Ok;
        }
    };

    let msg_type = envelope.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "hello" => {
            info!("slack: received hello — Socket Mode active");
            HandleResult::Ok
        }
        "disconnect" => {
            info!("slack: received disconnect — will reconnect");
            HandleResult::Reconnect
        }
        "events_api" => {
            // Acknowledge the envelope immediately
            if let Some(envelope_id) = envelope.get("envelope_id").and_then(|v| v.as_str()) {
                let ack = serde_json::json!({ "envelope_id": envelope_id });
                use futures::SinkExt;
                let mut writer = ws_write.lock().await;
                if let Err(e) = writer
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        ack.to_string().into(),
                    ))
                    .await
                {
                    warn!("slack: failed to send ack: {e}");
                }
            }

            // Extract the event payload
            let payload = match envelope.get("payload") {
                Some(p) => p,
                None => return HandleResult::Ok,
            };

            let event = match payload.get("event") {
                Some(e) => e,
                None => return HandleResult::Ok,
            };

            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if event_type != "message" {
                return HandleResult::Ok;
            }

            // Skip bot messages and message_changed subtypes
            if event.get("bot_id").is_some() || event.get("subtype").is_some() {
                return HandleResult::Ok;
            }

            let channel_id = event
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let user_id = event
                .get("user")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let text = event
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if text.trim().is_empty() {
                return HandleResult::Ok;
            }

            info!(
                "slack: message from {} in {}: {} chars",
                user_id,
                channel_id,
                text.len()
            );

            // Spawn message processing with streaming
            let client = client.clone();
            let bot_token = bot_token.to_string();
            let on_message = Arc::clone(on_message);

            tokio::spawn(async move {
                let (delta_tx, mut delta_rx) = mpsc::channel::<String>(64);

                let cb_channel = channel_id.clone();
                let cb_user = user_id.clone();
                let cb_text = text.clone();

                let callback_handle = tokio::spawn(async move {
                    // user_name = user_id for now (would need users.info call for display name)
                    on_message(cb_channel, cb_user.clone(), cb_user, cb_text, Some(delta_tx)).await
                });

                // Stream deltas: post initial message, then update it
                let mut accumulated = String::new();
                let mut msg_ts: Option<String> = None;
                let mut last_update = tokio::time::Instant::now();
                let mut first_delta_at: Option<tokio::time::Instant> = None;

                while let Some(delta) = delta_rx.recv().await {
                    accumulated.push_str(&delta);
                    if first_delta_at.is_none() {
                        first_delta_at = Some(tokio::time::Instant::now());
                    }

                    if msg_ts.is_none() {
                        // Buffer 1s before sending first message
                        if first_delta_at.unwrap().elapsed() >= Duration::from_secs(1) {
                            match api::post_message(
                                &client,
                                &bot_token,
                                &channel_id,
                                &accumulated,
                            )
                            .await
                            {
                                Ok(ts) => {
                                    msg_ts = Some(ts);
                                    last_update = tokio::time::Instant::now();
                                }
                                Err(e) => {
                                    error!("slack: failed to post streaming message: {e}");
                                    break;
                                }
                            }
                        }
                    } else if last_update.elapsed() >= Duration::from_millis(1000)
                        && let Some(ts) = &msg_ts
                    {
                        let _ = api::update_message(
                            &client,
                            &bot_token,
                            &channel_id,
                            ts,
                            &accumulated,
                        )
                        .await;
                        last_update = tokio::time::Instant::now();
                    }
                }

                // Get final result
                let result = callback_handle
                    .await
                    .unwrap_or_else(|e| Err(format!("task panic: {e}")));

                match result {
                    Ok(final_text) => {
                        let formatted = fmt::to_slack_mrkdwn(&final_text);
                        if let Some(ts) = &msg_ts {
                            let _ = api::update_message(
                                &client,
                                &bot_token,
                                &channel_id,
                                ts,
                                &formatted,
                            )
                            .await;
                        } else {
                            // No streaming happened — send final message directly
                            let _ =
                                api::post_message(&client, &bot_token, &channel_id, &formatted)
                                    .await;
                        }
                    }
                    Err(e) if e == "__blocked__" => {
                        // Silently drop — unauthorized user
                    }
                    Err(e) => {
                        let error_text = format!("Sorry, an error occurred: {e}");
                        if let Some(ts) = &msg_ts {
                            let _ = api::update_message(
                                &client,
                                &bot_token,
                                &channel_id,
                                ts,
                                &error_text,
                            )
                            .await;
                        } else {
                            let _ = api::post_message(
                                &client,
                                &bot_token,
                                &channel_id,
                                &error_text,
                            )
                            .await;
                        }
                    }
                }
            });

            HandleResult::Ok
        }
        _ => {
            tracing::trace!("slack: unhandled event type: {msg_type}");
            HandleResult::Ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_type_is_slack() {
        let on_msg: SlackOnMessageFn =
            Arc::new(|_ch, _uid, _user, _text, _delta_tx| {
                Box::pin(async { Ok("test".to_string()) })
            });
        let channel = SlackChannel::new(
            "xoxb-fake".to_string(),
            "xapp-fake".to_string(),
            on_msg,
        );
        assert_eq!(channel.channel_type(), "slack");
        assert_eq!(channel.display_name(), "Slack");
        assert_eq!(channel.status(), ChannelStatus::Disconnected);
    }
}
