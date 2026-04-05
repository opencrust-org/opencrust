pub mod api;
pub mod fmt;
pub mod webhook;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::{RwLock, mpsc};
use tracing::info;

use crate::traits::{ChannelLifecycle, ChannelSender, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// Group filter closure for WeChat channels.
/// Returns `true` if the message should be processed.
pub type WeChatGroupFilter = Arc<dyn Fn(bool) -> bool + Send + Sync>;

/// Callback invoked when the bot receives a text message from WeChat.
///
/// Arguments: `(openid, context_id, text, is_group, delta_tx)`.
/// `delta_tx` is always `None` — WeChat does not support message editing.
/// Return `Err("__blocked__")` to silently drop (unauthorized user).
pub type WeChatOnMessageFn = Arc<
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

/// WeChat access tokens are valid for 7200 seconds (2 hours).
/// Refresh slightly early to avoid expiry during a request.
const TOKEN_TTL: Duration = Duration::from_secs(7000);

/// How long to remember a MsgId for deduplication.
/// WeChat retries up to 3 times over ~15 seconds; 120 s gives a safe margin.
const MSG_ID_TTL: Duration = Duration::from_secs(120);

/// Cached access token with the instant it was fetched.
type TokenCache = Arc<RwLock<Option<(String, Instant)>>>;

pub struct WeChatChannel {
    client: Client,
    pub(crate) appid: String,
    pub(crate) secret: String,
    /// Verification token configured in the WeChat Official Account console.
    pub(crate) token: String,
    api_base_url: String,
    display: String,
    status: ChannelStatus,
    on_message: WeChatOnMessageFn,
    group_filter: WeChatGroupFilter,
    token_cache: TokenCache,
    /// In-memory deduplication cache for WeChat MsgIds (TTL = MSG_ID_TTL).
    msg_id_cache: Arc<RwLock<HashMap<String, Instant>>>,
}

impl WeChatChannel {
    pub fn new(
        appid: String,
        secret: String,
        token: String,
        on_message: WeChatOnMessageFn,
    ) -> Self {
        Self::with_group_filter(appid, secret, token, on_message, Arc::new(|_| true))
    }

    pub fn with_group_filter(
        appid: String,
        secret: String,
        token: String,
        on_message: WeChatOnMessageFn,
        group_filter: WeChatGroupFilter,
    ) -> Self {
        Self {
            client: Client::new(),
            appid,
            secret,
            token,
            api_base_url: api::WECHAT_API_BASE.to_string(),
            display: "WeChat".to_string(),
            status: ChannelStatus::Disconnected,
            on_message,
            group_filter,
            token_cache: Arc::new(RwLock::new(None)),
            msg_id_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Override the WeChat API base URL (e.g. to point at a mock server in tests).
    pub fn with_api_base_url(mut self, base_url: String) -> Self {
        self.api_base_url = base_url;
        self
    }

    pub fn appid(&self) -> &str {
        &self.appid
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    pub fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn group_filter(&self) -> &WeChatGroupFilter {
        &self.group_filter
    }

    /// Process an incoming message through the `on_message` callback.
    pub async fn handle_incoming(
        &self,
        openid: &str,
        context_id: &str,
        text: &str,
        is_group: bool,
    ) -> std::result::Result<String, String> {
        (self.on_message)(
            openid.to_string(),
            context_id.to_string(),
            text.to_string(),
            is_group,
            None,
        )
        .await
    }

    /// Check whether `msg_id` was already processed within `MSG_ID_TTL`.
    ///
    /// Returns `true` (duplicate — skip) or `false` (new — record and process).
    /// Expired entries are pruned on every call to bound memory usage.
    pub async fn check_and_mark_msg_id(&self, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }
        let now = Instant::now();
        let mut cache = self.msg_id_cache.write().await;
        cache.retain(|_, seen_at| now.duration_since(*seen_at) < MSG_ID_TTL);
        if cache.contains_key(msg_id) {
            return true;
        }
        cache.insert(msg_id.to_string(), now);
        false
    }
}

/// Lightweight send-only handle for the WeChat Customer Service API.
pub struct WeChatSender {
    client: Client,
    appid: String,
    secret: String,
    api_base_url: String,
    token_cache: TokenCache,
}

#[async_trait]
impl ChannelSender for WeChatSender {
    fn channel_type(&self) -> &str {
        "wechat"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        wechat_push_message(
            &self.client,
            &self.appid,
            &self.secret,
            &self.api_base_url,
            &self.token_cache,
            message,
        )
        .await
    }
}

#[async_trait]
impl ChannelLifecycle for WeChatChannel {
    fn display_name(&self) -> &str {
        &self.display
    }

    fn create_sender(&self) -> Box<dyn ChannelSender> {
        Box::new(WeChatSender {
            client: self.client.clone(),
            appid: self.appid.clone(),
            secret: self.secret.clone(),
            api_base_url: self.api_base_url.clone(),
            token_cache: Arc::clone(&self.token_cache),
        })
    }

    async fn connect(&mut self) -> Result<()> {
        // WeChat is webhook-driven — no persistent connection needed.
        // Register GET+POST /wechat/webhook in the gateway router.
        self.status = ChannelStatus::Connected;
        info!("wechat channel connected (webhook mode)");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Disconnected;
        info!("wechat channel disconnected");
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

#[async_trait]
impl ChannelSender for WeChatChannel {
    fn channel_type(&self) -> &str {
        "wechat"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        wechat_push_message(
            &self.client,
            &self.appid,
            &self.secret,
            &self.api_base_url,
            &self.token_cache,
            message,
        )
        .await
    }
}

/// Return a valid access token, using the cache when possible.
///
/// WeChat tokens are valid for 7200 s. We refresh after TOKEN_TTL (7000 s) to
/// avoid expiry mid-request. The `RwLock` allows concurrent reads while a
/// single writer refreshes the token.
async fn get_token_cached(
    client: &Client,
    appid: &str,
    secret: &str,
    api_base_url: &str,
    cache: &TokenCache,
) -> opencrust_common::Result<String> {
    {
        let guard = cache.read().await;
        if let Some((token, fetched_at)) = guard.as_ref()
            && fetched_at.elapsed() < TOKEN_TTL
        {
            return Ok(token.clone());
        }
    }
    let token = api::get_access_token(client, appid, secret, api_base_url)
        .await
        .map_err(|e| opencrust_common::Error::Channel(format!("wechat token fetch failed: {e}")))?;
    *cache.write().await = Some((token.clone(), Instant::now()));
    Ok(token)
}

/// Push a message via WeChat Customer Service API.
///
/// Uses a cached access token (refreshed after TOKEN_TTL) to avoid hitting
/// WeChat's 2000 token-requests/day rate limit.
///
/// For media messages (`Image`, `Audio`, `Video`) the metadata must contain a
/// `wechat_media_id` (pre-uploaded via the WeChat Media API). Video also
/// requires `wechat_thumb_media_id`.
async fn wechat_push_message(
    client: &Client,
    appid: &str,
    secret: &str,
    api_base_url: &str,
    token_cache: &TokenCache,
    message: &Message,
) -> Result<()> {
    let openid = message
        .metadata
        .get("wechat_openid")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            opencrust_common::Error::Channel("missing wechat_openid in metadata".into())
        })?;

    let access_token = get_token_cached(client, appid, secret, api_base_url, token_cache).await?;

    match &message.content {
        MessageContent::Text(t) => {
            let text = fmt::to_wechat_text(t);
            api::push(client, &access_token, openid, &text, api_base_url)
                .await
                .map_err(|e| {
                    opencrust_common::Error::Channel(format!("wechat push failed: {e}"))
                })?;
        }
        MessageContent::Image { .. } => {
            let media_id = message
                .metadata
                .get("wechat_media_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    opencrust_common::Error::Channel(
                        "missing wechat_media_id in metadata for image send".into(),
                    )
                })?;
            api::push_image(client, &access_token, openid, media_id, api_base_url)
                .await
                .map_err(|e| {
                    opencrust_common::Error::Channel(format!("wechat image push failed: {e}"))
                })?;
        }
        MessageContent::Audio { .. } => {
            let media_id = message
                .metadata
                .get("wechat_media_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    opencrust_common::Error::Channel(
                        "missing wechat_media_id in metadata for voice send".into(),
                    )
                })?;
            api::push_voice(client, &access_token, openid, media_id, api_base_url)
                .await
                .map_err(|e| {
                    opencrust_common::Error::Channel(format!("wechat voice push failed: {e}"))
                })?;
        }
        MessageContent::Video { caption, .. } => {
            let media_id = message
                .metadata
                .get("wechat_media_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    opencrust_common::Error::Channel(
                        "missing wechat_media_id in metadata for video send".into(),
                    )
                })?;
            let thumb_media_id = message
                .metadata
                .get("wechat_thumb_media_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    opencrust_common::Error::Channel(
                        "missing wechat_thumb_media_id in metadata for video send".into(),
                    )
                })?;
            api::push_video(
                client,
                &access_token,
                openid,
                media_id,
                thumb_media_id,
                caption.as_deref(),
                None,
                api_base_url,
            )
            .await
            .map_err(|e| {
                opencrust_common::Error::Channel(format!("wechat video push failed: {e}"))
            })?;
        }
        _ => {
            return Err(opencrust_common::Error::Channel(
                "unsupported message content type for wechat send".into(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_on_msg() -> WeChatOnMessageFn {
        Arc::new(|_uid, _ctx, _text, _is_group, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        })
    }

    #[test]
    fn channel_type_is_wechat() {
        let ch = WeChatChannel::new(
            "appid".to_string(),
            "secret".to_string(),
            "token".to_string(),
            make_on_msg(),
        );
        assert_eq!(ch.channel_type(), "wechat");
        assert_eq!(ch.display_name(), "WeChat");
        assert_eq!(ch.status(), ChannelStatus::Disconnected);
    }

    #[test]
    fn group_filter_default_allows_all() {
        let ch = WeChatChannel::new(
            "appid".to_string(),
            "secret".to_string(),
            "token".to_string(),
            make_on_msg(),
        );
        assert!(ch.group_filter()(false));
        assert!(ch.group_filter()(true));
    }

    #[test]
    fn group_filter_blocks_when_false() {
        let ch = WeChatChannel::with_group_filter(
            "appid".to_string(),
            "secret".to_string(),
            "token".to_string(),
            make_on_msg(),
            Arc::new(|_| false),
        );
        assert!(!ch.group_filter()(false));
        assert!(!ch.group_filter()(true));
    }

    #[tokio::test]
    async fn connect_sets_status_connected() {
        let mut ch = WeChatChannel::new(
            "appid".to_string(),
            "secret".to_string(),
            "token".to_string(),
            make_on_msg(),
        );
        ch.connect().await.unwrap();
        assert_eq!(ch.status(), ChannelStatus::Connected);
    }

    #[tokio::test]
    async fn disconnect_sets_status_disconnected() {
        let mut ch = WeChatChannel::new(
            "appid".to_string(),
            "secret".to_string(),
            "token".to_string(),
            make_on_msg(),
        );
        ch.connect().await.unwrap();
        ch.disconnect().await.unwrap();
        assert_eq!(ch.status(), ChannelStatus::Disconnected);
    }
}
