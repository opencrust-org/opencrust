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

use crate::traits::{ChannelLifecycle, ChannelResponse, ChannelSender, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// Group filter closure for LINE channels.
/// Argument: `is_mentioned` (currently always `false` — LINE has no native mention API).
/// Returns `true` if the group message should be processed.
pub type LineGroupFilter = Arc<dyn Fn(bool) -> bool + Send + Sync>;

/// A file attached to a LINE message, with bytes already downloaded from the data API.
#[derive(Debug, Clone)]
pub struct LineFile {
    /// Original filename as reported by LINE (present for `file` type; generated for images).
    pub filename: String,
    /// Raw file bytes.
    pub data: Vec<u8>,
    /// MIME type string if detectable (e.g. `"application/pdf"`).
    pub mime_type: Option<String>,
}

/// Fire-and-forget callback invoked for every group text message (before reply filtering).
/// Arguments: `(group_id, user_id, text)`.
/// Used to embed and store messages for RAG without blocking the webhook response.
pub type GroupObserveFn = Arc<
    dyn Fn(String, String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Callback invoked when the bot receives a message from LINE.
///
/// Arguments: `(user_id, context_id, text, is_group, file, delta_tx)`.
/// `file` is `Some` when the user sent a document or image alongside (or instead of) text.
/// `delta_tx` is always `None` — LINE does not support streaming message edits.
/// Return `Err("__blocked__")` to silently drop (unauthorized user).
pub type LineOnMessageFn = Arc<
    dyn Fn(
            String,
            String,
            String,
            bool,
            Option<LineFile>,
            Option<mpsc::Sender<String>>,
        )
            -> Pin<Box<dyn Future<Output = std::result::Result<ChannelResponse, String>> + Send>>
        + Send
        + Sync,
>;

pub struct LineChannel {
    client: Client,
    channel_access_token: String,
    channel_secret: String,
    api_base_url: String,
    /// Base URL for the LINE data API (file/image/audio downloads).
    /// Defaults to `https://api-data.line.me/v2/bot`.
    data_api_base_url: String,
    name: String,
    display: String,
    /// LINE user ID of this bot, resolved from `GET /v2/bot/info` on connect.
    /// Used to detect `@mention` in group messages.
    bot_user_id: Option<String>,
    status: ChannelStatus,
    on_message: LineOnMessageFn,
    group_filter: LineGroupFilter,
    /// Optional RAG observer: called for every group text message before reply filtering.
    group_observe_fn: Option<GroupObserveFn>,
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
            data_api_base_url: api::LINE_DATA_API_BASE.to_string(),
            name: "line".to_string(),
            display: String::new(),
            bot_user_id: None,
            status: ChannelStatus::Disconnected,
            on_message,
            group_filter,
            group_observe_fn: None,
        }
    }

    /// Attach a RAG observer that embeds every group message for later retrieval.
    pub fn with_group_observe(mut self, observe_fn: GroupObserveFn) -> Self {
        self.group_observe_fn = Some(observe_fn);
        self
    }

    pub fn group_observe_fn(&self) -> Option<&GroupObserveFn> {
        self.group_observe_fn.as_ref()
    }

    /// Override the config key name for this channel instance.
    pub fn with_name(mut self, name: String) -> Self {
        self.name = name;
        self
    }

    /// Override the LINE messaging API base URL (e.g. to point at a mock server in tests).
    /// Also sets the data API base URL to the same value for test convenience.
    pub fn with_api_base_url(mut self, base_url: String) -> Self {
        self.data_api_base_url = base_url.clone();
        self.api_base_url = base_url;
        self
    }

    pub fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    pub fn data_api_base_url(&self) -> &str {
        &self.data_api_base_url
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

    pub fn bot_user_id(&self) -> Option<&str> {
        self.bot_user_id.as_deref()
    }

    /// Verify the `X-Line-Signature` header.
    ///
    /// LINE signs the raw request body with HMAC-SHA256 using the channel secret
    /// and base64-encodes the result. Uses `hmac::verify` for constant-time comparison
    /// to avoid timing side-channels.
    pub fn verify_signature(&self, body: &[u8], signature: &str) -> bool {
        let Ok(sig_bytes) = general_purpose::STANDARD.decode(signature) else {
            return false;
        };
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.channel_secret.as_bytes());
        hmac::verify(&key, body, &sig_bytes).is_ok()
    }

    /// Process an incoming message through the `on_message` callback.
    pub async fn handle_incoming(
        &self,
        user_id: &str,
        context_id: &str,
        text: &str,
        is_group: bool,
        file: Option<LineFile>,
    ) -> std::result::Result<ChannelResponse, String> {
        (self.on_message)(
            user_id.to_string(),
            context_id.to_string(),
            text.to_string(),
            is_group,
            file,
            None,
        )
        .await
    }
}

/// Lightweight send-only handle for the LINE Push API.
pub struct LineSender {
    client: Client,
    channel_access_token: String,
    api_base_url: String,
    name: String,
}

#[async_trait]
impl ChannelSender for LineSender {
    fn channel_type(&self) -> &str {
        "line"
    }

    fn channel_name(&self) -> &str {
        &self.name
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        line_push_message(
            &self.client,
            &self.channel_access_token,
            &self.api_base_url,
            message,
        )
        .await
    }
}

#[async_trait]
impl ChannelLifecycle for LineChannel {
    fn display_name(&self) -> &str {
        &self.display
    }

    fn create_sender(&self) -> Box<dyn ChannelSender> {
        Box::new(LineSender {
            client: self.client.clone(),
            channel_access_token: self.channel_access_token.clone(),
            api_base_url: self.api_base_url.clone(),
            name: self.name.clone(),
        })
    }

    async fn connect(&mut self) -> Result<()> {
        // LINE is webhook-driven — no persistent connection needed.
        // Register POST /line/webhook in the gateway router.
        match api::get_bot_info(&self.client, &self.channel_access_token, &self.api_base_url).await
        {
            Ok(info) => {
                info!(
                    "line: bot resolved — name: {}, userId: {}",
                    info.display_name, info.user_id
                );
                self.display = info.display_name;
                self.bot_user_id = Some(info.user_id);
            }
            Err(e) => {
                tracing::warn!("line: could not resolve bot info: {e}");
            }
        }
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

    fn channel_name(&self) -> &str {
        &self.name
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        line_push_message(
            &self.client,
            &self.channel_access_token,
            &self.api_base_url,
            message,
        )
        .await
    }
}

/// Push a message via LINE Push API.
///
/// Requires `line_user_id` in `message.metadata`. Used by the scheduler
/// and any code path that cannot use a reply token.
async fn line_push_message(
    client: &Client,
    channel_access_token: &str,
    api_base_url: &str,
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

    api::push(client, channel_access_token, user_id, &text, api_base_url)
        .await
        .map_err(|e| opencrust_common::Error::Channel(format!("line push failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_on_msg() -> LineOnMessageFn {
        Arc::new(|_uid, _ctx, _text, _is_group, _file, _delta_tx| {
            Box::pin(async { Ok(ChannelResponse::Text("test".to_string())) })
        })
    }

    #[test]
    fn channel_type_is_line() {
        let ch = LineChannel::new("tok".to_string(), "sec".to_string(), make_on_msg());
        assert_eq!(ch.channel_type(), "line");
        assert_eq!(ch.display_name(), "");
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

    // --- LineFile / file-ingest tests ---

    #[test]
    fn line_file_fields_accessible() {
        let f = LineFile {
            filename: "report.pdf".to_string(),
            data: vec![1, 2, 3],
            mime_type: Some("application/pdf".to_string()),
        };
        assert_eq!(f.filename, "report.pdf");
        assert_eq!(f.data.len(), 3);
        assert_eq!(f.mime_type.as_deref(), Some("application/pdf"));
    }

    #[test]
    fn line_file_mime_type_optional() {
        let f = LineFile {
            filename: "data.bin".to_string(),
            data: vec![],
            mime_type: None,
        };
        assert!(f.mime_type.is_none());
    }

    #[tokio::test]
    async fn on_message_callback_receives_line_file() {
        let on_msg: LineOnMessageFn = Arc::new(|_uid, _ctx, _text, _is_group, file, _delta_tx| {
            Box::pin(async move {
                let name = file
                    .map(|f| f.filename)
                    .unwrap_or_else(|| "none".to_string());
                Ok(ChannelResponse::Text(name))
            })
        });

        let line_file = LineFile {
            filename: "invoice.pdf".to_string(),
            data: vec![0u8; 16],
            mime_type: Some("application/pdf".to_string()),
        };

        let result = on_msg(
            "U123".to_string(),
            "U123".to_string(),
            String::new(),
            false,
            Some(line_file),
            None,
        )
        .await;

        assert!(matches!(result, Ok(ChannelResponse::Text(t)) if t == "invoice.pdf"));
    }

    #[tokio::test]
    async fn on_message_callback_with_no_file() {
        let on_msg: LineOnMessageFn = Arc::new(|_uid, _ctx, _text, _is_group, file, _delta_tx| {
            Box::pin(async move {
                let name = file
                    .map(|f| f.filename)
                    .unwrap_or_else(|| "none".to_string());
                Ok(ChannelResponse::Text(name))
            })
        });

        let result = on_msg(
            "U123".to_string(),
            "U123".to_string(),
            "hello".to_string(),
            false,
            None,
            None,
        )
        .await;

        assert!(matches!(result, Ok(ChannelResponse::Text(t)) if t == "none"));
    }

    #[test]
    fn channel_response_text_extracted_for_line() {
        let r = ChannelResponse::Text("reply".to_string());
        assert_eq!(r.text(), "reply");
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
