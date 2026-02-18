use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use opencrust_common::{Error, Result};
use rmcp::ServiceExt;
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{info, warn};

use super::tool_bridge::McpTool;
use crate::tools::Tool;

/// Cached info about a tool discovered from an MCP server.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

/// A live connection to one MCP server.
struct McpConnection {
    server_name: String,
    service: RunningService<RoleClient, ()>,
    tools: Vec<McpToolInfo>,
}

/// Manages the lifecycle of MCP server connections.
pub struct McpManager {
    connections: Arc<RwLock<HashMap<String, McpConnection>>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Connect to an MCP server by spawning a child process.
    pub async fn connect(
        &self,
        name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        timeout_secs: u64,
    ) -> Result<()> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }

        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| Error::Mcp(format!("failed to spawn MCP server '{name}': {e}")))?;

        let service = tokio::time::timeout(Duration::from_secs(timeout_secs), ().serve(transport))
            .await
            .map_err(|_| {
                Error::Mcp(format!(
                    "MCP server '{name}' handshake timed out after {timeout_secs}s"
                ))
            })?
            .map_err(|e| Error::Mcp(format!("MCP server '{name}' handshake failed: {e}")))?;

        // Discover tools
        let mcp_tools = service
            .list_all_tools()
            .await
            .map_err(|e| Error::Mcp(format!("failed to list tools from '{name}': {e}")))?;

        let tools: Vec<McpToolInfo> = mcp_tools
            .into_iter()
            .map(|t| McpToolInfo {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()),
                input_schema: serde_json::to_value(&*t.input_schema).unwrap_or_default(),
            })
            .collect();

        info!(
            "MCP server '{name}' connected: {} tool(s) discovered",
            tools.len()
        );
        for tool in &tools {
            info!("  -> {name}.{}", tool.name);
        }

        let conn = McpConnection {
            server_name: name.to_string(),
            service,
            tools,
        };

        self.connections
            .write()
            .await
            .insert(name.to_string(), conn);
        Ok(())
    }

    /// Disconnect a specific MCP server.
    pub async fn disconnect(&self, name: &str) {
        if let Some(conn) = self.connections.write().await.remove(name) {
            info!("disconnecting MCP server '{name}'");
            if let Err(e) = conn.service.cancel().await {
                warn!("error cancelling MCP server '{name}': {e}");
            }
        }
    }

    /// Disconnect all MCP servers.
    pub async fn disconnect_all(&self) {
        let conns: HashMap<String, McpConnection> =
            std::mem::take(&mut *self.connections.write().await);
        for (name, conn) in conns {
            info!("disconnecting MCP server '{name}'");
            if let Err(e) = conn.service.cancel().await {
                warn!("error cancelling MCP server '{name}': {e}");
            }
        }
    }

    /// Create `Tool` trait objects for all tools from a specific server.
    /// The tools share a reference to the server's peer handle.
    pub async fn take_tools(&self, name: &str, timeout: Duration) -> Vec<Box<dyn Tool>> {
        let conns = self.connections.read().await;
        let Some(conn) = conns.get(name) else {
            return Vec::new();
        };

        let peer: Arc<Peer<RoleClient>> = Arc::new(conn.service.peer().clone());

        conn.tools
            .iter()
            .map(|t| {
                Box::new(McpTool::new(
                    &conn.server_name,
                    t.name.clone(),
                    t.description.clone(),
                    t.input_schema.clone(),
                    Arc::clone(&peer),
                    timeout,
                )) as Box<dyn Tool>
            })
            .collect()
    }

    /// List all connected servers with their tool counts.
    pub async fn list_servers(&self) -> Vec<(String, usize, bool)> {
        let conns = self.connections.read().await;
        conns
            .iter()
            .map(|(name, conn)| (name.clone(), conn.tools.len(), !conn.service.is_closed()))
            .collect()
    }

    /// Get tool info for a specific server.
    pub async fn tool_info(&self, name: &str) -> Vec<McpToolInfo> {
        let conns = self.connections.read().await;
        conns.get(name).map(|c| c.tools.clone()).unwrap_or_default()
    }
}
