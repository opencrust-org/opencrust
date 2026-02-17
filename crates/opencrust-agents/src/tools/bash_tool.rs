use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::time::Duration;
use tokio::process::Command;

use super::{Tool, ToolOutput};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// Execute bash commands with configurable timeout and output limits.
pub struct BashTool {
    timeout: Duration,
}

impl BashTool {
    pub fn new(timeout_secs: Option<u64>) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)),
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command and return its output. Use this for running shell commands, scripts, and system operations."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'command' parameter".into()))?;

        let result = tokio::time::timeout(
            self.timeout,
            Command::new("bash").arg("-c").arg(command).output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                let mut combined = String::new();
                if !stdout.is_empty() {
                    combined.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str("STDERR:\n");
                    combined.push_str(&stderr);
                }

                // Truncate if too large
                if combined.len() > MAX_OUTPUT_BYTES {
                    combined.truncate(MAX_OUTPUT_BYTES);
                    combined.push_str("\n... (output truncated)");
                }

                if combined.is_empty() {
                    combined = format!("(exit code: {})", output.status.code().unwrap_or(-1));
                }

                if output.status.success() {
                    Ok(ToolOutput::success(combined))
                } else {
                    Ok(ToolOutput::error(format!(
                        "exit code {}: {}",
                        output.status.code().unwrap_or(-1),
                        combined
                    )))
                }
            }
            Ok(Err(e)) => Ok(ToolOutput::error(format!("failed to execute command: {e}"))),
            Err(_) => Ok(ToolOutput::error(format!(
                "command timed out after {}s",
                self.timeout.as_secs()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn executes_simple_command() {
        let tool = BashTool::new(None);
        let output = tool
            .execute(serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("hello"));
    }

    #[tokio::test]
    async fn reports_error_on_failing_command() {
        let tool = BashTool::new(None);
        let output = tool
            .execute(serde_json::json!({"command": "false"}))
            .await
            .unwrap();
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn returns_error_on_missing_command() {
        let tool = BashTool::new(None);
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn captures_stderr() {
        let tool = BashTool::new(None);
        let output = tool
            .execute(serde_json::json!({"command": "echo err >&2"}))
            .await
            .unwrap();
        assert!(output.content.contains("err"));
    }
}
