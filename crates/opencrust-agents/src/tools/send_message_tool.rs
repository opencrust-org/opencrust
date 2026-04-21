use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

use super::{Tool, ToolContext, ToolOutput};

/// Outbound message dispatched by the tool to the gateway.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    /// Channel key from config (e.g. "telegram", "discord", "slack").
    pub channel_id: String,
    /// Platform-native recipient identifier (chat_id, user_id, channel_id, etc.).
    pub recipient_id: String,
    /// Message text to deliver.
    pub text: String,
}

/// Send a proactive message to a connected channel from agent code.
///
/// The tool holds a `mpsc::Sender` that is wired at bootstrap time via
/// `SendMessageHandle::wire()`. Until wired the tool returns an error so it
/// fails safely without panicking.
pub struct SendMessageTool {
    sender: Arc<OnceLock<mpsc::Sender<OutboundMessage>>>,
}

impl SendMessageTool {
    /// Create the tool and return the accompanying handle used for wiring.
    pub fn new() -> (Self, SendMessageHandle) {
        let cell = Arc::new(OnceLock::new());
        let tool = Self {
            sender: Arc::clone(&cell),
        };
        let handle = SendMessageHandle { cell };
        (tool, handle)
    }
}

impl Default for SendMessageTool {
    fn default() -> Self {
        Self::new().0
    }
}

/// Returned by `SendMessageTool::new()`.
/// Call `wire()` once the gateway's outbound receiver is set up.
pub struct SendMessageHandle {
    cell: Arc<OnceLock<mpsc::Sender<OutboundMessage>>>,
}

impl SendMessageHandle {
    /// Wire the tool to the outbound sender. Safe to call only once; subsequent
    /// calls are silently ignored (the `OnceLock` guarantees idempotency).
    pub fn wire(&self, sender: mpsc::Sender<OutboundMessage>) {
        let _ = self.cell.set(sender);
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a proactive message to any connected channel. \
         Use this to deliver notifications, alerts, or scheduled-job results \
         to a specific user or chat on a given platform."
    }

    fn system_hint(&self) -> Option<&str> {
        Some(
            "Use send_message to deliver a message to a channel other than the one the user \
             is currently on, or to notify a specific recipient without waiting for their reply. \
             channel_id must match a key in the `channels:` section of config.yml \
             (e.g. \"telegram\", \"discord\", \"slack\"). \
             recipient_id is the platform-native chat or user ID.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "channel_id": {
                    "type": "string",
                    "description": "Channel key from config (e.g. \"telegram\", \"discord\", \"slack\")"
                },
                "recipient_id": {
                    "type": "string",
                    "description": "Platform-native recipient identifier (chat_id, user_id, channel name, etc.)"
                },
                "text": {
                    "type": "string",
                    "description": "Message text to send"
                }
            },
            "required": ["channel_id", "recipient_id", "text"]
        })
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let channel_id = input
            .get("channel_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'channel_id' parameter".into()))?
            .to_string();

        let recipient_id = input
            .get("recipient_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'recipient_id' parameter".into()))?
            .to_string();

        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'text' parameter".into()))?
            .to_string();

        if text.trim().is_empty() {
            return Ok(ToolOutput::error("text cannot be empty"));
        }

        let sender = self.sender.get().ok_or_else(|| {
            Error::Agent(
                "send_message is not wired — call SendMessageHandle::wire() in bootstrap".into(),
            )
        })?;

        sender
            .send(OutboundMessage {
                channel_id: channel_id.clone(),
                recipient_id: recipient_id.clone(),
                text,
            })
            .await
            .map_err(|_| Error::Agent("outbound message channel closed".into()))?;

        Ok(ToolOutput::success(format!(
            "message queued for delivery to {channel_id}/{recipient_id}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
            allowed_tools: None,
        }
    }

    #[tokio::test]
    async fn returns_error_when_not_wired() {
        let (tool, _handle) = SendMessageTool::new();
        let result = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "channel_id": "telegram",
                    "recipient_id": "123",
                    "text": "hello"
                }),
            )
            .await;
        // Unwired tool returns Err (not wired)
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delivers_message_when_wired() {
        let (tool, handle) = SendMessageTool::new();
        let (tx, mut rx) = mpsc::channel(8);
        handle.wire(tx);

        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "channel_id": "telegram",
                    "recipient_id": "999",
                    "text": "hello from agent"
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error, "{}", output.content);

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.channel_id, "telegram");
        assert_eq!(msg.recipient_id, "999");
        assert_eq!(msg.text, "hello from agent");
    }

    #[tokio::test]
    async fn wire_twice_is_idempotent() {
        let (_tool, handle) = SendMessageTool::new();
        let (tx1, _rx1) = mpsc::channel(1);
        let (tx2, _rx2) = mpsc::channel(1);
        handle.wire(tx1);
        handle.wire(tx2); // second call must not panic
    }

    #[tokio::test]
    async fn missing_params_return_err() {
        let (tool, _) = SendMessageTool::new();
        assert!(tool.execute(&ctx(), serde_json::json!({})).await.is_err());
    }
}
