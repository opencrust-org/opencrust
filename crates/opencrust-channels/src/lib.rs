pub mod protocol;
pub mod registry;
#[cfg(feature = "telegram")]
pub mod telegram;
pub mod traits;

pub use protocol::{
    CONNECTOR_PROTOCOL_VERSION, ConnectorCapability, ConnectorFrame, ConnectorHandshake,
    MAX_CONNECTOR_FRAME_BYTES,
};
pub use registry::ChannelRegistry;
#[cfg(feature = "telegram")]
pub use telegram::{OnMessageFn, TelegramChannel};
pub use traits::{Channel, ChannelEvent, ChannelStatus};
