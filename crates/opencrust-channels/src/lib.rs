pub mod protocol;
pub mod registry;
#[cfg(feature = "telegram")]
pub mod telegram;
#[cfg(feature = "telegram")]
pub mod telegram_fmt;
pub mod traits;

#[cfg(feature = "discord")]
pub mod discord;
#[cfg(feature = "slack")]
pub mod slack;
#[cfg(feature = "whatsapp")]
pub mod whatsapp;

pub use protocol::{
    CONNECTOR_PROTOCOL_VERSION, ConnectorCapability, ConnectorFrame, ConnectorHandshake,
    MAX_CONNECTOR_FRAME_BYTES,
};
pub use registry::ChannelRegistry;
#[cfg(feature = "slack")]
pub use slack::{SlackChannel, SlackOnMessageFn};
#[cfg(feature = "telegram")]
pub use telegram::{OnMessageFn, TelegramChannel};
pub use traits::{Channel, ChannelEvent, ChannelStatus};
#[cfg(feature = "whatsapp")]
pub use whatsapp::{WhatsAppChannel, WhatsAppOnMessageFn};
