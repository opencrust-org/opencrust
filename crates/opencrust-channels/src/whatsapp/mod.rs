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

use crate::traits::{ChannelLifecycle, ChannelResponse, ChannelSender, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

/// A file attached to a WhatsApp message, with bytes already downloaded.
///
/// For WhatsApp Business the channel handler downloads the media before
/// invoking `WhatsAppOnMessageFn`. For WhatsApp Web the sidecar does not
/// yet emit file events, so this will always be `None` on that path.
#[derive(Debug, Clone)]
pub struct WhatsAppFile {
    /// Original filename as reported by WhatsApp (may be empty for some types).
    pub filename: String,
    /// Raw file bytes.
    pub data: Vec<u8>,
    /// MIME type string (e.g. `"application/pdf"`).
    pub mime_type: Option<String>,
}

/// Callback invoked when the bot receives a message from WhatsApp.
///
/// Arguments: `(from_number, user_name, text, is_group, file, delta_tx)`.
/// `file` is `Some` when the user sent a document/image along with the message.
/// `delta_tx` is always `None` for WhatsApp (no streaming support).
/// Return `Err("__blocked__")` to silently drop the message (unauthorized user).
pub type WhatsAppOnMessageFn = Arc<
    dyn Fn(
            String,
            String,
            String,
            bool,
            Option<WhatsAppFile>,
            Option<mpsc::Sender<String>>,
        )
            -> Pin<Box<dyn Future<Output = std::result::Result<ChannelResponse, String>> + Send>>
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

    /// Process an incoming message from the webhook.
    pub async fn handle_incoming(
        &self,
        from: &str,
        user_name: &str,
        text: &str,
        file: Option<WhatsAppFile>,
    ) -> std::result::Result<ChannelResponse, String> {
        (self.on_message)(
            from.to_string(),
            user_name.to_string(),
            text.to_string(),
            false, // WhatsApp Business is DM-only
            file,
            None, // No streaming for WhatsApp
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

    let text = match &message.content {
        MessageContent::Text(t) => t.clone(),
        _ => {
            return Err(opencrust_common::Error::Channel(
                "only text messages are supported for whatsapp send".into(),
            ));
        }
    };

    api::send_text_message(client, access_token, phone_number_id, to, &text)
        .await
        .map_err(|e| opencrust_common::Error::Channel(format!("whatsapp send failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_type_is_whatsapp() {
        let on_msg: WhatsAppOnMessageFn =
            Arc::new(|_from, _user, _text, _is_group, _file, _delta_tx| {
                Box::pin(async { Ok(ChannelResponse::Text("test".to_string())) })
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

    // --- WhatsAppFile / file-ingest tests ---

    #[test]
    fn whatsapp_file_fields_accessible() {
        let file = WhatsAppFile {
            filename: "invoice.pdf".to_string(),
            data: vec![1, 2, 3],
            mime_type: Some("application/pdf".to_string()),
        };
        assert_eq!(file.filename, "invoice.pdf");
        assert_eq!(file.data.len(), 3);
        assert_eq!(file.mime_type.as_deref(), Some("application/pdf"));
    }

    #[test]
    fn whatsapp_file_mime_type_optional() {
        let file = WhatsAppFile {
            filename: "data.bin".to_string(),
            data: vec![],
            mime_type: None,
        };
        assert!(file.mime_type.is_none());
    }

    #[tokio::test]
    async fn on_message_callback_receives_whatsapp_file() {
        let on_msg: WhatsAppOnMessageFn =
            Arc::new(|_from, _user, _text, _is_group, file, _delta_tx| {
                Box::pin(async move {
                    let name = file
                        .map(|f| f.filename)
                        .unwrap_or_else(|| "none".to_string());
                    Ok(ChannelResponse::Text(name))
                })
            });

        let wa_file = WhatsAppFile {
            filename: "contract.pdf".to_string(),
            data: vec![0u8; 32],
            mime_type: Some("application/pdf".to_string()),
        };

        let result = on_msg(
            "+66812345678".to_string(),
            "Alice".to_string(),
            "/ingest".to_string(),
            false,
            Some(wa_file),
            None,
        )
        .await;

        assert!(matches!(result, Ok(ChannelResponse::Text(t)) if t == "contract.pdf"));
    }

    #[tokio::test]
    async fn on_message_callback_with_no_file() {
        let on_msg: WhatsAppOnMessageFn =
            Arc::new(|_from, _user, _text, _is_group, file, _delta_tx| {
                Box::pin(async move {
                    let name = file
                        .map(|f| f.filename)
                        .unwrap_or_else(|| "none".to_string());
                    Ok(ChannelResponse::Text(name))
                })
            });

        let result = on_msg(
            "+66812345678".to_string(),
            "Alice".to_string(),
            "hello".to_string(),
            false,
            None,
            None,
        )
        .await;

        assert!(matches!(result, Ok(ChannelResponse::Text(t)) if t == "none"));
    }

    #[test]
    fn channel_response_text_extracted() {
        let r = ChannelResponse::Text("reply".to_string());
        assert_eq!(r.text(), "reply");
    }
}
