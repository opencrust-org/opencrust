use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolOutput};

const MAX_WRITE_BYTES: usize = 1024 * 1024; // 1MB

/// Write content to a file with path validation and size limits.
pub struct FileWriteTool {
    allowed_directories: Option<Vec<PathBuf>>,
}

impl FileWriteTool {
    pub fn new(allowed_directories: Option<Vec<PathBuf>>) -> Self {
        Self {
            allowed_directories,
        }
    }

    fn validate_path(&self, path: &std::path::Path) -> Result<()> {
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Error::Security("path traversal not allowed".into()));
        }

        if let Some(allowed) = &self.allowed_directories {
            // For write, the parent must exist and be in allowed dirs
            let parent = path
                .parent()
                .ok_or_else(|| Error::Agent("invalid path".into()))?;
            let canonical = parent
                .canonicalize()
                .map_err(|e| Error::Agent(format!("cannot resolve parent path: {e}")))?;
            if !allowed.iter().any(|dir| canonical.starts_with(dir)) {
                return Err(Error::Security("path outside allowed directories".into()));
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file at the given path. Creates the file if it doesn't exist, overwrites if it does."
    }

    fn hint(&self, input: &serde_json::Value) -> String {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        format!("\n🔧 file_write: {path}\n")
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to write to"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'path' parameter".into()))?;

        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'content' parameter".into()))?;

        if content.len() > MAX_WRITE_BYTES {
            return Ok(ToolOutput::error(format!(
                "content too large: {} bytes (limit: {} bytes)",
                content.len(),
                MAX_WRITE_BYTES
            )));
        }

        let path = PathBuf::from(path_str);
        self.validate_path(&path)?;

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::Agent(format!("failed to create directories: {e}")))?;
        }

        tokio::fs::write(&path, content)
            .await
            .map_err(|e| Error::Agent(format!("failed to write file: {e}")))?;

        Ok(ToolOutput::success(format!(
            "wrote {} bytes to {}",
            content.len(),
            path_str
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn writes_file_successfully() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");

        let tool = FileWriteTool::new(None);
        let ctx = ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
        };
        let output = tool
            .execute(
                &ctx,
                serde_json::json!({
                    "path": file_path.to_str().unwrap(),
                    "content": "hello world"
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error);
        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "hello world");
    }

    #[tokio::test]
    async fn returns_error_on_missing_params() {
        let tool = FileWriteTool::new(None);
        let ctx = ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
        };
        assert!(tool.execute(&ctx, serde_json::json!({})).await.is_err());
        assert!(
            tool.execute(&ctx, serde_json::json!({"path": "/tmp/test"}))
                .await
                .is_err()
        );
    }
}
