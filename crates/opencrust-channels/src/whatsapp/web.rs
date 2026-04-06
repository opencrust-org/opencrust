use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::traits::{ChannelLifecycle, ChannelSender, ChannelStatus};
use opencrust_common::{Message, MessageContent, Result};

use super::WhatsAppOnMessageFn;

/// Group filter closure for WhatsApp Web channels.
/// Argument: `is_mentioned` (always `false` - WhatsApp has no standard mention format).
/// Returns `true` if the message should be processed.
pub type WhatsAppWebGroupFilter = Arc<dyn Fn(bool) -> bool + Send + Sync>;

/// Shared sender handle that gets populated when the sidecar process starts.
type SharedStdinTx = Arc<tokio::sync::Mutex<Option<mpsc::Sender<String>>>>;

/// Lightweight send-only handle for WhatsApp Web.
pub struct WhatsAppWebSender {
    shared_stdin_tx: SharedStdinTx,
}

#[async_trait]
impl ChannelSender for WhatsAppWebSender {
    fn channel_type(&self) -> &str {
        "whatsapp-web"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        let guard = self.shared_stdin_tx.lock().await;
        let tx = guard.as_ref().ok_or_else(|| {
            opencrust_common::Error::Channel("whatsapp-web sidecar not connected".into())
        })?;
        whatsapp_web_send_message(tx, message).await
    }
}

/// Sidecar-driven WhatsApp Web channel using Baileys (QR code pairing).
pub struct WhatsAppWebChannel {
    child: Option<Child>,
    stdin_tx: Option<mpsc::Sender<String>>,
    /// Shared sender handle exposed via `create_sender()`.
    shared_stdin_tx: SharedStdinTx,
    status: ChannelStatus,
    display: String,
    on_message: WhatsAppOnMessageFn,
    group_filter: WhatsAppWebGroupFilter,
    auth_dir: PathBuf,
    sidecar_dir: PathBuf,
}

impl WhatsAppWebChannel {
    pub fn new(on_message: WhatsAppOnMessageFn) -> Self {
        Self::with_group_filter(on_message, Arc::new(|_| true))
    }

    pub fn with_group_filter(
        on_message: WhatsAppOnMessageFn,
        group_filter: WhatsAppWebGroupFilter,
    ) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let config_dir = home.join(".opencrust");

        Self {
            child: None,
            stdin_tx: None,
            shared_stdin_tx: Arc::new(tokio::sync::Mutex::new(None)),
            status: ChannelStatus::Disconnected,
            display: "WhatsApp Web".to_string(),
            on_message,
            group_filter,
            auth_dir: config_dir.join("whatsapp-web-auth"),
            sidecar_dir: config_dir.join("sidecar").join("whatsapp-web"),
        }
    }

    // Embedded sidecar files - written to disk on first connect if not found elsewhere.
    const EMBEDDED_INDEX_MJS: &'static str =
        include_str!("../../../../sidecar/whatsapp-web/index.mjs");
    const EMBEDDED_PACKAGE_JSON: &'static str =
        include_str!("../../../../sidecar/whatsapp-web/package.json");

    /// Resolve the sidecar directory. Checks `~/.opencrust/sidecar/whatsapp-web/`
    /// first, then falls back to the path relative to the binary, then extracts
    /// embedded files as a last resort.
    fn resolve_sidecar_dir(&self) -> Result<PathBuf> {
        if self.sidecar_dir.join("index.mjs").exists() {
            return Ok(self.sidecar_dir.clone());
        }

        // Fall back to path relative to the binary
        if let Ok(exe) = std::env::current_exe()
            && let Some(parent) = exe.parent()
        {
            let bundled = parent.join("sidecar").join("whatsapp-web");
            if bundled.join("index.mjs").exists() {
                return Ok(bundled);
            }
            // Also check repo layout (binary in target/debug or target/release)
            let repo_sidecar = parent
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("sidecar").join("whatsapp-web"));
            if let Some(ref repo) = repo_sidecar
                && repo.join("index.mjs").exists()
            {
                return Ok(repo.clone());
            }
        }

        // Last resort: extract embedded sidecar files
        info!(
            "whatsapp-web: extracting embedded sidecar to {}",
            self.sidecar_dir.display()
        );
        std::fs::create_dir_all(&self.sidecar_dir).map_err(|e| {
            opencrust_common::Error::Channel(format!(
                "failed to create sidecar dir {}: {e}",
                self.sidecar_dir.display()
            ))
        })?;
        std::fs::write(self.sidecar_dir.join("index.mjs"), Self::EMBEDDED_INDEX_MJS).map_err(
            |e| opencrust_common::Error::Channel(format!("failed to write index.mjs: {e}")),
        )?;
        std::fs::write(
            self.sidecar_dir.join("package.json"),
            Self::EMBEDDED_PACKAGE_JSON,
        )
        .map_err(|e| {
            opencrust_common::Error::Channel(format!("failed to write package.json: {e}"))
        })?;
        Ok(self.sidecar_dir.clone())
    }

    /// Ensure node_modules exist by running `npm install --omit=dev`.
    async fn ensure_npm_install(&self, sidecar_dir: &PathBuf) -> Result<()> {
        if sidecar_dir.join("node_modules").exists() {
            return Ok(());
        }

        info!("whatsapp-web: running npm install (first time setup)...");
        let status = Command::new("npm")
            .args(["install", "--omit=dev"])
            .current_dir(sidecar_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await
            .map_err(|e| {
                opencrust_common::Error::Channel(format!(
                    "failed to run npm install: {e} (is Node.js installed?)"
                ))
            })?;

        if !status.success() {
            return Err(opencrust_common::Error::Channel(
                "npm install failed for whatsapp-web sidecar".into(),
            ));
        }

        info!("whatsapp-web: npm install completed");
        Ok(())
    }
}

#[async_trait]
impl ChannelLifecycle for WhatsAppWebChannel {
    fn display_name(&self) -> &str {
        &self.display
    }

    fn create_sender(&self) -> Box<dyn ChannelSender> {
        Box::new(WhatsAppWebSender {
            shared_stdin_tx: Arc::clone(&self.shared_stdin_tx),
        })
    }

    async fn connect(&mut self) -> Result<()> {
        self.status = ChannelStatus::Connecting;

        let sidecar_dir = self.resolve_sidecar_dir()?;
        self.ensure_npm_install(&sidecar_dir).await?;

        // Create auth directory if needed
        if !self.auth_dir.exists() {
            std::fs::create_dir_all(&self.auth_dir).map_err(|e| {
                opencrust_common::Error::Channel(format!(
                    "failed to create auth dir {}: {e}",
                    self.auth_dir.display()
                ))
            })?;
        }

        let mut child = Command::new("node")
            .arg("index.mjs")
            .current_dir(&sidecar_dir)
            .env("WHATSAPP_AUTH_DIR", &self.auth_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // QR art and errors go to terminal
            .spawn()
            .map_err(|e| {
                opencrust_common::Error::Channel(format!(
                    "failed to spawn whatsapp-web sidecar: {e} (is Node.js installed?)"
                ))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| opencrust_common::Error::Channel("no stdout from sidecar".into()))?;

        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| opencrust_common::Error::Channel("no stdin to sidecar".into()))?;

        self.child = Some(child);

        // Writer task: forward commands from channel to sidecar stdin
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(64);
        self.stdin_tx = Some(stdin_tx.clone());

        // Populate shared sender handle for create_sender() consumers
        {
            let mut shared = self.shared_stdin_tx.lock().await;
            *shared = Some(stdin_tx.clone());
        }

        tokio::spawn(async move {
            let mut writer = child_stdin;
            while let Some(line) = stdin_rx.recv().await {
                let data = format!("{line}\n");
                if writer.write_all(data.as_bytes()).await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
        });

        // Reader task: parse events from sidecar stdout
        let on_message = Arc::clone(&self.on_message);
        let group_filter = Arc::clone(&self.group_filter);
        let reply_tx = stdin_tx;

        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let event: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match event_type {
                    "qr" => {
                        info!("whatsapp-web: QR code generated, waiting for scan...");
                    }
                    "ready" => {
                        info!("whatsapp-web: connected and ready");
                    }
                    "message" => {
                        let from = event
                            .get("from")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = event
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let text = event
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let is_group = event
                            .get("isGroup")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        // For group messages, reply to the group JID, not the participant
                        let reply_to = if is_group {
                            event
                                .get("groupJid")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&from)
                                .to_string()
                        } else {
                            from.clone()
                        };

                        if from.is_empty() || text.is_empty() {
                            continue;
                        }

                        // Apply group filter (no mention detection for WhatsApp)
                        if is_group && !group_filter(false) {
                            continue;
                        }

                        let reply_tx = reply_tx.clone();
                        let on_message = Arc::clone(&on_message);

                        tokio::spawn(async move {
                            // WhatsApp Web sidecar does not yet emit file events.
                            match (on_message)(from.clone(), name, text, is_group, None, None).await
                            {
                                Ok(response) => {
                                    let cmd = serde_json::json!({
                                        "type": "send",
                                        "to": reply_to,
                                        "text": response.text(),
                                    });
                                    let _ = reply_tx
                                        .send(serde_json::to_string(&cmd).unwrap_or_default())
                                        .await;
                                }
                                Err(e) => {
                                    if e != "__blocked__" {
                                        warn!("whatsapp-web: message handler error: {e}");
                                    }
                                }
                            }
                        });
                    }
                    "disconnected" => {
                        let reason = event
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        warn!("whatsapp-web: disconnected ({reason})");
                    }
                    "pong" => {}
                    _ => {}
                }
            }
            info!("whatsapp-web: sidecar stdout closed");
        });

        self.status = ChannelStatus::Connected;
        info!("whatsapp-web channel started");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Drop stdin sender - sidecar detects stdin close and exits gracefully
        self.stdin_tx.take();

        // Clear shared sender handle so pending senders get an error
        {
            let mut shared = self.shared_stdin_tx.lock().await;
            *shared = None;
        }

        if let Some(mut child) = self.child.take() {
            // Wait with timeout, then force kill
            match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                Ok(_) => {}
                Err(_) => {
                    warn!("whatsapp-web: sidecar did not exit in time, killing");
                    child.kill().await.ok();
                }
            }
        }

        self.status = ChannelStatus::Disconnected;
        info!("whatsapp-web channel disconnected");
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

#[async_trait]
impl ChannelSender for WhatsAppWebChannel {
    fn channel_type(&self) -> &str {
        "whatsapp-web"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        let tx = self.stdin_tx.as_ref().ok_or_else(|| {
            opencrust_common::Error::Channel("whatsapp-web sidecar not connected".into())
        })?;
        whatsapp_web_send_message(tx, message).await
    }
}

/// Shared send logic used by both `WhatsAppWebChannel` and `WhatsAppWebSender`.
async fn whatsapp_web_send_message(tx: &mpsc::Sender<String>, message: &Message) -> Result<()> {
    let to = message
        .metadata
        .get("whatsapp_from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            opencrust_common::Error::Channel("missing whatsapp_from in metadata".into())
        })?;

    let text = match &message.content {
        MessageContent::Text(t) => t.clone(),
        _ => {
            return Err(opencrust_common::Error::Channel(
                "only text messages are supported for whatsapp-web send".into(),
            ));
        }
    };

    let cmd = serde_json::json!({
        "type": "send",
        "to": to,
        "text": text,
    });

    tx.send(serde_json::to_string(&cmd).unwrap_or_default())
        .await
        .map_err(|e| opencrust_common::Error::Channel(format!("failed to send to sidecar: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_type_is_whatsapp_web() {
        let on_msg: WhatsAppOnMessageFn =
            Arc::new(|_from, _user, _text, _is_group, _file, _delta_tx| {
                Box::pin(async { Ok(crate::traits::ChannelResponse::Text("test".to_string())) })
            });
        let channel = WhatsAppWebChannel::new(on_msg);
        assert_eq!(channel.channel_type(), "whatsapp-web");
        assert_eq!(channel.display_name(), "WhatsApp Web");
        assert_eq!(channel.status(), ChannelStatus::Disconnected);
    }

    #[test]
    fn whatsapp_web_group_filter_blocks() {
        let filter: WhatsAppWebGroupFilter = Arc::new(|_mentioned| false);
        assert!(!filter(false));
    }

    #[test]
    fn sidecar_event_is_group_parsing() {
        let event: serde_json::Value = serde_json::json!({
            "type": "message",
            "from": "1234@s.whatsapp.net",
            "name": "Test",
            "text": "hello",
            "isGroup": true,
            "groupJid": "group@g.us",
        });
        let is_group = event
            .get("isGroup")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert!(is_group);
        let group_jid = event.get("groupJid").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(group_jid, "group@g.us");
    }
}
