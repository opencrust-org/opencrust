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

/// Cached info about a resource from an MCP server.
#[derive(Debug, Clone)]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// Cached info about a prompt from an MCP server.
#[derive(Debug, Clone)]
pub struct McpPromptInfo {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Vec<McpPromptArgument>,
}

/// A prompt argument definition.
#[derive(Debug, Clone)]
pub struct McpPromptArgument {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

/// Connection parameters for reconnection.
#[derive(Clone)]
enum ConnectionParams {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        timeout_secs: u64,
    },
    Http {
        url: String,
        timeout_secs: u64,
    },
}

/// A live connection to one MCP server.
struct McpConnection {
    server_name: String,
    service: RunningService<RoleClient, ()>,
    tools: Vec<McpToolInfo>,
    instructions: Option<String>,
    params: ConnectionParams,
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

        // Capture server instructions from handshake
        let instructions = service
            .peer()
            .peer_info()
            .and_then(|info| info.instructions.clone());

        info!(
            "MCP server '{name}' connected: {} tool(s) discovered",
            tools.len()
        );
        for tool in &tools {
            info!("  -> {name}.{}", tool.name);
        }
        if instructions.is_some() {
            info!("MCP server '{name}' provided instructions");
        }

        let conn = McpConnection {
            server_name: name.to_string(),
            service,
            tools,
            instructions,
            params: ConnectionParams::Stdio {
                command: command.to_string(),
                args: args.to_vec(),
                env: env.clone(),
                timeout_secs,
            },
        };

        self.connections
            .write()
            .await
            .insert(name.to_string(), conn);
        Ok(())
    }

    /// Connect to an MCP server via HTTP (Streamable HTTP transport).
    pub async fn connect_http(&self, name: &str, url: &str, timeout_secs: u64) -> Result<()> {
        use rmcp::transport::StreamableHttpClientTransport;

        let transport = StreamableHttpClientTransport::from_uri(url);

        let service = tokio::time::timeout(Duration::from_secs(timeout_secs), ().serve(transport))
            .await
            .map_err(|_| {
                Error::Mcp(format!(
                    "MCP server '{name}' HTTP handshake timed out after {timeout_secs}s"
                ))
            })?
            .map_err(|e| Error::Mcp(format!("MCP server '{name}' HTTP handshake failed: {e}")))?;

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

        let instructions = service
            .peer()
            .peer_info()
            .and_then(|info| info.instructions.clone());

        info!(
            "MCP server '{name}' connected via HTTP: {} tool(s) discovered",
            tools.len()
        );
        if instructions.is_some() {
            info!("MCP server '{name}' provided instructions");
        }

        let conn = McpConnection {
            server_name: name.to_string(),
            service,
            tools,
            instructions,
            params: ConnectionParams::Http {
                url: url.to_string(),
                timeout_secs,
            },
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

    /// List resources from a specific MCP server.
    pub async fn list_resources(&self, name: &str) -> Result<Vec<McpResourceInfo>> {
        let conns = self.connections.read().await;
        let conn = conns
            .get(name)
            .ok_or_else(|| Error::Mcp(format!("MCP server '{name}' not connected")))?;

        let resources = conn
            .service
            .list_all_resources()
            .await
            .map_err(|e| Error::Mcp(format!("failed to list resources from '{name}': {e}")))?;

        Ok(resources
            .into_iter()
            .map(|r| McpResourceInfo {
                uri: r.uri.to_string(),
                name: r.name.to_string(),
                description: r.description.as_deref().map(|d| d.to_string()),
                mime_type: r.mime_type.as_deref().map(|m| m.to_string()),
            })
            .collect())
    }

    /// Read a specific resource from an MCP server.
    pub async fn read_resource(&self, name: &str, uri: &str) -> Result<String> {
        let conns = self.connections.read().await;
        let conn = conns
            .get(name)
            .ok_or_else(|| Error::Mcp(format!("MCP server '{name}' not connected")))?;

        let params = rmcp::model::ReadResourceRequestParams::new(uri.to_string());

        let result = conn.service.read_resource(params).await.map_err(|e| {
            Error::Mcp(format!(
                "failed to read resource '{uri}' from '{name}': {e}"
            ))
        })?;

        let text_parts: Vec<String> = result
            .contents
            .into_iter()
            .filter_map(|c| match c {
                rmcp::model::ResourceContents::TextResourceContents { text, .. } => Some(text),
                _ => None,
            })
            .collect();

        Ok(text_parts.join("\n"))
    }

    /// List prompts from a specific MCP server.
    pub async fn list_prompts(&self, name: &str) -> Result<Vec<McpPromptInfo>> {
        let conns = self.connections.read().await;
        let conn = conns
            .get(name)
            .ok_or_else(|| Error::Mcp(format!("MCP server '{name}' not connected")))?;

        let prompts = conn
            .service
            .list_all_prompts()
            .await
            .map_err(|e| Error::Mcp(format!("failed to list prompts from '{name}': {e}")))?;

        Ok(prompts
            .into_iter()
            .map(|p| McpPromptInfo {
                name: p.name.to_string(),
                description: p.description.map(|d| d.to_string()),
                arguments: p
                    .arguments
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| McpPromptArgument {
                        name: a.name.to_string(),
                        description: a.description.map(|d| d.to_string()),
                        required: a.required.unwrap_or(false),
                    })
                    .collect(),
            })
            .collect())
    }

    /// Get a specific prompt with arguments from an MCP server.
    pub async fn get_prompt(
        &self,
        name: &str,
        prompt_name: &str,
        args: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<Vec<String>> {
        let conns = self.connections.read().await;
        let conn = conns
            .get(name)
            .ok_or_else(|| Error::Mcp(format!("MCP server '{name}' not connected")))?;

        let mut params = rmcp::model::GetPromptRequestParams::new(prompt_name.to_string());
        if let Some(a) = args {
            params = params.with_arguments(a);
        }

        let result = conn.service.get_prompt(params).await.map_err(|e| {
            Error::Mcp(format!(
                "failed to get prompt '{prompt_name}' from '{name}': {e}"
            ))
        })?;

        let messages: Vec<String> = result
            .messages
            .into_iter()
            .map(|m| {
                let role = match m.role {
                    rmcp::model::Role::User => "user",
                    rmcp::model::Role::Assistant => "assistant",
                };
                let text = match m.content {
                    rmcp::model::ContentBlock::Text(t) => t.text,
                    _ => "(non-text content)".to_string(),
                };
                format!("[{role}] {text}")
            })
            .collect();

        Ok(messages)
    }

    /// Return instructions from all connected servers that provided them.
    pub async fn get_all_instructions(&self) -> Vec<(String, String)> {
        let conns = self.connections.read().await;
        conns
            .iter()
            .filter_map(|(name, conn)| {
                conn.instructions
                    .as_ref()
                    .map(|inst| (name.clone(), inst.clone()))
            })
            .collect()
    }

    /// List resources from all connected MCP servers.
    pub async fn list_all_resources(&self) -> Vec<(String, Vec<McpResourceInfo>)> {
        let server_names: Vec<String> = {
            let conns = self.connections.read().await;
            conns.keys().cloned().collect()
        };
        let mut results = Vec::new();
        for name in server_names {
            match self.list_resources(&name).await {
                Ok(resources) if !resources.is_empty() => {
                    results.push((name, resources));
                }
                _ => {}
            }
        }
        results
    }

    /// Spawn a background health monitor that pings servers and reconnects on failure.
    pub fn spawn_health_monitor(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                manager.check_and_reconnect().await;
            }
        });
    }

    /// Check all connections and attempt to reconnect any that are closed.
    async fn check_and_reconnect(&self) {
        let to_reconnect: Vec<(String, ConnectionParams)> = {
            let conns = self.connections.read().await;
            conns
                .iter()
                .filter(|(_, conn)| conn.service.is_closed())
                .map(|(name, conn)| (name.clone(), conn.params.clone()))
                .collect()
        };

        for (name, params) in to_reconnect {
            info!("MCP server '{name}' connection lost, attempting reconnect...");
            // Remove stale connection
            self.connections.write().await.remove(&name);

            let result = match params {
                ConnectionParams::Stdio {
                    ref command,
                    ref args,
                    ref env,
                    timeout_secs,
                } => self.connect(&name, command, args, env, timeout_secs).await,
                ConnectionParams::Http {
                    ref url,
                    timeout_secs,
                } => self.connect_http(&name, url, timeout_secs).await,
            };

            match result {
                Ok(()) => info!("MCP server '{name}' reconnected successfully"),
                Err(e) => warn!("MCP server '{name}' reconnect failed: {e}"),
            }
        }
    }
}
