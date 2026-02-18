use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opencrust_common::Result;
use rmcp::model::{CallToolRequestParams, RawContent};
use rmcp::service::{Peer, RoleClient};
use serde_json::Value;

use crate::tools::{Tool, ToolOutput};

/// Bridges a single MCP server tool into the opencrust `Tool` trait.
pub struct McpTool {
    /// Namespaced name: "server_name.tool_name"
    namespaced_name: String,
    /// Original tool name as registered on the MCP server
    original_name: String,
    /// Tool description from the MCP server
    tool_description: String,
    /// JSON Schema for tool input
    schema: Value,
    /// Shared handle to the MCP server peer
    peer: Arc<Peer<RoleClient>>,
    /// Timeout for tool execution
    timeout: Duration,
}

impl McpTool {
    pub fn new(
        server_name: &str,
        original_name: String,
        description: Option<String>,
        input_schema: Value,
        peer: Arc<Peer<RoleClient>>,
        timeout: Duration,
    ) -> Self {
        Self {
            namespaced_name: format!("{server_name}.{original_name}"),
            tool_description: description
                .unwrap_or_else(|| format!("MCP tool {original_name} from {server_name}")),
            original_name,
            schema: input_schema,
            peer,
            timeout,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.namespaced_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, input: Value) -> Result<ToolOutput> {
        let arguments = match input {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("input".to_string(), other);
                Some(map)
            }
        };

        let params = CallToolRequestParams {
            name: Cow::Owned(self.original_name.clone()),
            arguments,
            meta: None,
            task: None,
        };

        let result = tokio::time::timeout(self.timeout, self.peer.call_tool(params))
            .await
            .map_err(|_| {
                opencrust_common::Error::Mcp(format!(
                    "tool {} timed out after {:?}",
                    self.namespaced_name, self.timeout
                ))
            })?
            .map_err(|e| opencrust_common::Error::Mcp(format!("call_tool failed: {e}")))?;

        // Convert MCP Content items to a single text output
        let mut text_parts = Vec::new();
        for content in &result.content {
            match &content.raw {
                RawContent::Text(text_content) => {
                    text_parts.push(text_content.text.to_string());
                }
                _ => {
                    // For non-text content, include a placeholder
                    text_parts.push("[non-text content]".to_string());
                }
            }
        }

        let output_text = text_parts.join("\n");
        let is_error = result.is_error.unwrap_or(false);

        if is_error {
            Ok(ToolOutput::error(output_text))
        } else {
            Ok(ToolOutput::success(output_text))
        }
    }
}
