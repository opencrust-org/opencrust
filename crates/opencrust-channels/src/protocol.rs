use opencrust_common::{Error, Message, Result};
use serde::{Deserialize, Serialize};

use crate::traits::ChannelStatus;

/// Wire protocol version for external connector processes.
pub const CONNECTOR_PROTOCOL_VERSION: u32 = 1;

/// Maximum accepted serialized frame size in bytes.
pub const MAX_CONNECTOR_FRAME_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorCapability {
    SendMessage,
    ReceiveMessages,
    HealthCheck,
    Attachments,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorHandshake {
    pub protocol_version: u32,
    pub connector_name: String,
    pub connector_version: String,
    pub channel_type: String,
    #[serde(default)]
    pub capabilities: Vec<ConnectorCapability>,
}

impl ConnectorHandshake {
    pub fn validate(&self) -> Result<()> {
        if self.protocol_version != CONNECTOR_PROTOCOL_VERSION {
            return Err(Error::Channel(format!(
                "unsupported protocol version {}, expected {}",
                self.protocol_version, CONNECTOR_PROTOCOL_VERSION
            )));
        }

        if self.connector_name.trim().is_empty() {
            return Err(Error::Channel("connector_name cannot be empty".into()));
        }

        if self.connector_version.trim().is_empty() {
            return Err(Error::Channel("connector_version cannot be empty".into()));
        }

        if self.channel_type.trim().is_empty() {
            return Err(Error::Channel("channel_type cannot be empty".into()));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectorFrame {
    Handshake {
        payload: ConnectorHandshake,
    },
    HandshakeAck {
        protocol_version: u32,
        accepted: bool,
        message: Option<String>,
    },
    SendMessage {
        request_id: String,
        message: Message,
    },
    MessageReceived {
        message: Message,
    },
    StatusUpdate {
        status: ChannelStatus,
    },
    HealthCheck {
        request_id: String,
    },
    HealthCheckResult {
        request_id: String,
        healthy: bool,
        details: Option<String>,
    },
    Error {
        request_id: Option<String>,
        code: String,
        message: String,
    },
}

impl ConnectorFrame {
    pub fn parse_json(line: &str) -> Result<Self> {
        if line.len() > MAX_CONNECTOR_FRAME_BYTES {
            return Err(Error::Channel(format!(
                "connector frame exceeds max size: {} > {}",
                line.len(),
                MAX_CONNECTOR_FRAME_BYTES
            )));
        }

        serde_json::from_str(line)
            .map_err(|e| Error::Channel(format!("invalid connector frame json: {e}")))
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CONNECTOR_PROTOCOL_VERSION, ConnectorCapability, ConnectorFrame, ConnectorHandshake,
        MAX_CONNECTOR_FRAME_BYTES,
    };
    use opencrust_common::{ChannelId, Message, MessageDirection, SessionId, UserId};

    #[test]
    fn handshake_validation_accepts_valid_payload() {
        let handshake = ConnectorHandshake {
            protocol_version: CONNECTOR_PROTOCOL_VERSION,
            connector_name: "telegram-sidecar".to_string(),
            connector_version: "0.1.0".to_string(),
            channel_type: "telegram".to_string(),
            capabilities: vec![
                ConnectorCapability::ReceiveMessages,
                ConnectorCapability::SendMessage,
            ],
        };

        assert!(handshake.validate().is_ok());
    }

    #[test]
    fn handshake_validation_rejects_wrong_protocol_version() {
        let handshake = ConnectorHandshake {
            protocol_version: 999,
            connector_name: "telegram-sidecar".to_string(),
            connector_version: "0.1.0".to_string(),
            channel_type: "telegram".to_string(),
            capabilities: vec![],
        };

        assert!(handshake.validate().is_err());
    }

    #[test]
    fn connector_frame_round_trip_json() {
        let message = Message::text(
            SessionId::from_string("session-1"),
            ChannelId::from_string("telegram"),
            UserId::from_string("user-1"),
            MessageDirection::Outgoing,
            "hello",
        );

        let frame = ConnectorFrame::SendMessage {
            request_id: "req-1".to_string(),
            message,
        };

        let json = frame.to_json().expect("serialization should succeed");
        let parsed = ConnectorFrame::parse_json(&json).expect("deserialization should succeed");

        assert!(matches!(
            parsed,
            ConnectorFrame::SendMessage {
                request_id,
                message: _
            } if request_id == "req-1"
        ));
    }

    #[test]
    fn parse_json_rejects_oversized_payload() {
        let oversized = "x".repeat(MAX_CONNECTOR_FRAME_BYTES + 1);
        let err = ConnectorFrame::parse_json(&oversized).expect_err("must reject oversized frame");
        assert!(err.to_string().contains("exceeds max size"));
    }
}
