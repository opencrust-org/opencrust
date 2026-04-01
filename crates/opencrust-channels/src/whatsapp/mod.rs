pub mod api;
#[cfg(feature = "whatsapp-web")]
pub mod web;
pub mod webhook;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::mpsc;
use tracing::info;

use crate::traits::{ChannelLifecycle, ChannelSender, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// Callback invoked when the bot receives a text message from WhatsApp.
///
/// Arguments: `(from_number, user_name, text, is_group, delta_tx)`.
/// `delta_tx` is always `None` for WhatsApp (no streaming support).
/// Return `Err("__blocked__")` to silently drop the message (unauthorized user).
pub type WhatsAppOnMessageFn = Arc<
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

pub struct WhatsAppChannel {
    client: Client,
    access_token: String,
    phone_number_id: String,
    verify_token: String,
    display: String,
    status: ChannelStatus,
    on_message: WhatsAppOnMessageFn,
}

impl WhatsAppChannel {
    pub fn new(
        access_token: String,
        phone_number_id: String,
        verify_token: String,
        on_message: WhatsAppOnMessageFn,
    ) -> Self {
        Self {
            client: Client::new(),
            access_token,
            phone_number_id,
            verify_token,
            display: "WhatsApp".to_string(),
            status: ChannelStatus::Disconnected,
            on_message,
        }
    }

    /// Access token for the WhatsApp Cloud API.
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Phone number ID for this WhatsApp Business account.
    pub fn phone_number_id(&self) -> &str {
        &self.phone_number_id
    }

    /// Verify token used for webhook verification.
    pub fn verify_token(&self) -> &str {
        &self.verify_token
    }

    /// HTTP client shared across requests.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Process an incoming message from the webhook. Returns the response text.
    pub async fn handle_incoming(
        &self,
        from: &str,
        user_name: &str,
        text: &str,
    ) -> std::result::Result<String, String> {
        (self.on_message)(
            from.to_string(),
            user_name.to_string(),
            text.to_string(),
            false, // WhatsApp Business is DM-only
            None,  // No streaming for WhatsApp
        )
        .await
    }
}

/// Lightweight send-only handle for WhatsApp Business API.
pub struct WhatsAppSender {
    client: Client,
    access_token: String,
    phone_number_id: String,
}

#[async_trait]
impl ChannelSender for WhatsAppSender {
    fn channel_type(&self) -> &str {
        "whatsapp"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        whatsapp_send_message(
            &self.client,
            &self.access_token,
            &self.phone_number_id,
            message,
        )
        .await
    }
}

#[async_trait]
impl ChannelLifecycle for WhatsAppChannel {
    fn display_name(&self) -> &str {
        &self.display
    }

    fn create_sender(&self) -> Box<dyn ChannelSender> {
        Box::new(WhatsAppSender {
            client: Client::new(),
            access_token: self.access_token.clone(),
            phone_number_id: self.phone_number_id.clone(),
        })
    }

    async fn connect(&mut self) -> Result<()> {
        // WhatsApp is webhook-driven - no persistent connection needed.
        self.status = ChannelStatus::Connected;
        info!("whatsapp channel connected (webhook mode)");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Disconnected;
        info!("whatsapp channel disconnected");
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

#[async_trait]
impl ChannelSender for WhatsAppChannel {
    fn channel_type(&self) -> &str {
        "whatsapp"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        whatsapp_send_message(
            &self.client,
            &self.access_token,
            &self.phone_number_id,
            message,
        )
        .await
    }
}

/// Shared send logic used by both `WhatsAppChannel` and `WhatsAppSender`.
async fn whatsapp_send_message(
    client: &Client,
    access_token: &str,
    phone_number_id: &str,
    message: &Message,
) -> Result<()> {
    let to = message
        .metadata
        .get("whatsapp_from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            opencrust_common::Error::Channel("missing whatsapp_from in metadata".into())
        })?;

    let raw = match &message.content {
        MessageContent::Text(t) => t.as_str(),
        _ => {
            return Err(opencrust_common::Error::Channel(
                "only text messages are supported for whatsapp send".into(),
            ));
        }
    };

    let (hints, body) = crate::hints::split_hints(raw);
    if let Some(h) = hints {
        api::send_text_message(client, access_token, phone_number_id, to, &h)
            .await
            .map_err(|e| opencrust_common::Error::Channel(format!("whatsapp send failed: {e}")))?;
    }
    if body.trim().is_empty() {
        return Ok(());
    }

    api::send_text_message(client, access_token, phone_number_id, to, &body)
        .await
        .map_err(|e| opencrust_common::Error::Channel(format!("whatsapp send failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_type_is_whatsapp() {
        let on_msg: WhatsAppOnMessageFn = Arc::new(|_from, _user, _text, _is_group, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let channel = WhatsAppChannel::new(
            "fake-token".to_string(),
            "123456".to_string(),
            "verify-me".to_string(),
            on_msg,
        );
        assert_eq!(channel.channel_type(), "whatsapp");
        assert_eq!(channel.display_name(), "WhatsApp");
        assert_eq!(channel.status(), ChannelStatus::Disconnected);
    }
}
