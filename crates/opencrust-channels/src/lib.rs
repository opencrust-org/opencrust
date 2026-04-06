pub mod protocol;
pub mod registry;
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
pub use line::{LineChannel, LineGroupFilter, LineOnMessageFn};
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
pub use wechat::{WeChatChannel, WeChatGroupFilter, WeChatOnMessageFn};
#[cfg(feature = "whatsapp-web")]
pub use whatsapp::web::{WhatsAppWebChannel, WhatsAppWebGroupFilter};
#[cfg(feature = "whatsapp")]
pub use whatsapp::{WhatsAppChannel, WhatsAppFile, WhatsAppOnMessageFn};
