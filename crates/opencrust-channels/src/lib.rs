pub mod protocol;
pub mod registry;
pub mod traits;

pub use protocol::{
    CONNECTOR_PROTOCOL_VERSION, ConnectorCapability, ConnectorFrame, ConnectorHandshake,
    MAX_CONNECTOR_FRAME_BYTES,
};
pub use registry::ChannelRegistry;
pub use traits::{Channel, ChannelEvent, ChannelStatus};
