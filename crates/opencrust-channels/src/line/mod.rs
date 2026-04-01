pub mod api;
pub mod fmt;
pub mod webhook;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose};
use reqwest::Client;
use ring::hmac;
use tokio::sync::mpsc;
use tracing::info;

use crate::traits::{ChannelLifecycle, ChannelSender, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// Group filter closure for LINE channels.
/// Argument: `is_mentioned` (currently always `false` — LINE has no native mention API).
/// Returns `true` if the group message should be processed.
pub type LineGroupFilter = Arc<dyn Fn(bool) -> bool + Send + Sync>;

/// Callback invoked when the bot receives a text message from LINE.
///
/// Arguments: `(user_id, context_id, text, is_group, delta_tx)`.
/// `delta_tx` is always `None` — LINE does not support message editing.
/// Return `Err("__blocked__")` to silently drop (unauthorized user).
pub type LineOnMessageFn = Arc<
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

pub struct LineChannel {
    client: Client,
    channel_access_token: String,
    channel_secret: String,
    api_base_url: String,
    display: String,
    status: ChannelStatus,
    on_message: LineOnMessageFn,
    group_filter: LineGroupFilter,
}

impl LineChannel {
    pub fn new(
        channel_access_token: String,
        channel_secret: String,
        on_message: LineOnMessageFn,
    ) -> Self {
        Self::with_group_filter(
            channel_access_token,
            channel_secret,
            on_message,
            Arc::new(|_| true),
        )
    }

    pub fn with_group_filter(
        channel_access_token: String,
        channel_secret: String,
        on_message: LineOnMessageFn,
        group_filter: LineGroupFilter,
    ) -> Self {
        Self {
            client: Client::new(),
            channel_access_token,
            channel_secret,
            api_base_url: api::LINE_API_BASE.to_string(),
            display: "LINE".to_string(),
            status: ChannelStatus::Disconnected,
            on_message,
            group_filter,
        }
    }

    /// Override the LINE API base URL. Used in tests to point at a mock server.
    #[cfg(test)]
    pub fn with_api_base_url(mut self, base_url: String) -> Self {
        self.api_base_url = base_url;
        self
    }

    pub fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    pub fn channel_access_token(&self) -> &str {
        &self.channel_access_token
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn group_filter(&self) -> &LineGroupFilter {
        &self.group_filter
    }

    /// Verify the `X-Line-Signature` header.
    ///
    /// LINE signs the raw request body with HMAC-SHA256 using the channel secret
    /// and base64-encodes the result.
    pub fn verify_signature(&self, body: &[u8], signature: &str) -> bool {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.channel_secret.as_bytes());
        let tag = hmac::sign(&key, body);
        let expected = general_purpose::STANDARD.encode(tag.as_ref());
        expected == signature
    }

    /// Process an incoming message through the `on_message` callback.
    pub async fn handle_incoming(
        &self,
        user_id: &str,
        context_id: &str,
        text: &str,
        is_group: bool,
    ) -> std::result::Result<String, String> {
        (self.on_message)(
            user_id.to_string(),
            context_id.to_string(),
            text.to_string(),
            is_group,
            None,
        )
        .await
    }
}

/// Lightweight send-only handle for the LINE Push API.
pub struct LineSender {
    client: Client,
    channel_access_token: String,
}

#[async_trait]
impl ChannelSender for LineSender {
    fn channel_type(&self) -> &str {
        "line"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        line_push_message(&self.client, &self.channel_access_token, message).await
    }
}

#[async_trait]
impl ChannelLifecycle for LineChannel {
    fn display_name(&self) -> &str {
        &self.display
    }

    fn create_sender(&self) -> Box<dyn ChannelSender> {
        Box::new(LineSender {
            client: Client::new(),
            channel_access_token: self.channel_access_token.clone(),
        })
    }

    async fn connect(&mut self) -> Result<()> {
        // LINE is webhook-driven — no persistent connection needed.
        // Register POST /line/webhook in the gateway router.
        self.status = ChannelStatus::Connected;
        info!("line channel connected (webhook mode)");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Disconnected;
        info!("line channel disconnected");
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

#[async_trait]
impl ChannelSender for LineChannel {
    fn channel_type(&self) -> &str {
        "line"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        line_push_message(&self.client, &self.channel_access_token, message).await
    }
}

/// Push a message via LINE Push API.
///
/// Requires `line_user_id` in `message.metadata`. Used by the scheduler
/// and any code path that cannot use a reply token.
async fn line_push_message(
    client: &Client,
    channel_access_token: &str,
    message: &Message,
) -> Result<()> {
    let user_id = message
        .metadata
        .get("line_user_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            opencrust_common::Error::Channel("missing line_user_id in metadata".into())
        })?;

    let text = match &message.content {
        MessageContent::Text(t) => fmt::to_line_text(t),
        _ => {
            return Err(opencrust_common::Error::Channel(
                "only text messages are supported for line send".into(),
            ));
        }
    };

    api::push(
        client,
        channel_access_token,
        user_id,
        &text,
        api::LINE_API_BASE,
    )
    .await
    .map_err(|e| opencrust_common::Error::Channel(format!("line push failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_on_msg() -> LineOnMessageFn {
        Arc::new(|_uid, _ctx, _text, _is_group, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        })
    }

    #[test]
    fn channel_type_is_line() {
        let ch = LineChannel::new("tok".to_string(), "sec".to_string(), make_on_msg());
        assert_eq!(ch.channel_type(), "line");
        assert_eq!(ch.display_name(), "LINE");
        assert_eq!(ch.status(), ChannelStatus::Disconnected);
    }

    #[test]
    fn verify_signature_correct() {
        let secret = "my-channel-secret";
        let body = b"hello world";

        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let tag = hmac::sign(&key, body);
        let sig = general_purpose::STANDARD.encode(tag.as_ref());

        let ch = LineChannel::new("tok".to_string(), secret.to_string(), make_on_msg());
        assert!(ch.verify_signature(body, &sig));
        assert!(!ch.verify_signature(body, "invalidsig"));
        assert!(!ch.verify_signature(b"other body", &sig));
    }

    #[test]
    fn group_filter_default_allows_all() {
        let ch = LineChannel::new("tok".to_string(), "sec".to_string(), make_on_msg());
        assert!(ch.group_filter()(false));
        assert!(ch.group_filter()(true));
    }

    #[test]
    fn group_filter_blocks_when_false() {
        let ch = LineChannel::with_group_filter(
            "tok".to_string(),
            "sec".to_string(),
            make_on_msg(),
            Arc::new(|_| false),
        );
        assert!(!ch.group_filter()(false));
        assert!(!ch.group_filter()(true));
    }
}
