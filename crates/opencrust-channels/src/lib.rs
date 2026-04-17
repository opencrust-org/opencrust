pub mod protocol;
pub mod registry;

/// Maximum file size accepted when downloading attachments from any channel (10 MiB).
///
/// Applied as both a `Content-Length` pre-check (where the header is available)
/// and a post-download `bytes.len()` check as a safety net.  This matches the
/// limit already in use by the Slack channel (`SLACK_MAX_FILE_BYTES`).
pub const MAX_DOWNLOAD_BYTES: usize = 10 * 1024 * 1024;
#[cfg(feature = "telegram")]
pub mod telegram;
#[cfg(feature = "telegram")]
pub mod telegram_fmt;
pub mod traits;

#[cfg(feature = "discord")]
pub mod discord;
#[cfg(all(target_os = "macos", feature = "imessage"))]
pub mod imessage;
#[cfg(feature = "line")]
pub mod line;
#[cfg(feature = "mqtt")]
pub mod mqtt;
#[cfg(feature = "slack")]
pub mod slack;
#[cfg(feature = "wechat")]
pub mod wechat;
#[cfg(feature = "whatsapp")]
pub mod whatsapp;

#[cfg(all(target_os = "macos", feature = "imessage"))]
pub use imessage::{IMessageChannel, IMessageGroupFilter, IMessageOnMessageFn};
#[cfg(feature = "line")]
pub use line::webhook::{LineWebhookState, line_webhook};
#[cfg(feature = "line")]
pub use line::{LineChannel, LineFile, LineGroupFilter, LineOnMessageFn};
#[cfg(feature = "mqtt")]
pub use mqtt::{MqttChannel, MqttOnMessageFn};
pub use protocol::{
    CONNECTOR_PROTOCOL_VERSION, ConnectorCapability, ConnectorFrame, ConnectorHandshake,
    MAX_CONNECTOR_FRAME_BYTES,
};
pub use registry::ChannelRegistry;
#[cfg(feature = "slack")]
pub use slack::{SlackChannel, SlackFile, SlackGroupFilter, SlackOnMessageFn};
#[cfg(feature = "telegram")]
pub use telegram::{GroupFilter, MediaAttachment, OnMessageFn, TelegramChannel};
pub use traits::{
    Channel, ChannelEvent, ChannelLifecycle, ChannelResponse, ChannelSender, ChannelStatus,
};
#[cfg(feature = "wechat")]
pub use wechat::webhook::{WeChatWebhookState, wechat_webhook, wechat_webhook_verify};
#[cfg(feature = "wechat")]
pub use wechat::{WeChatChannel, WeChatFile, WeChatGroupFilter, WeChatOnMessageFn};
#[cfg(feature = "whatsapp-web")]
pub use whatsapp::web::{WhatsAppWebChannel, WhatsAppWebGroupFilter};
#[cfg(feature = "whatsapp")]
pub use whatsapp::{WhatsAppChannel, WhatsAppFile, WhatsAppOnMessageFn};
