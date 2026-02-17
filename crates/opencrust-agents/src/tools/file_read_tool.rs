use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::path::PathBuf;

use super::{Tool, ToolOutput};

const MAX_READ_BYTES: u64 = 1024 * 1024; // 1MB

/// Read the contents of a file with path validation and size limits.
pub struct FileReadTool {
    allowed_directories: Option<Vec<PathBuf>>,
}

impl FileReadTool {
    pub fn new(allowed_directories: Option<Vec<PathBuf>>) -> Self {
        Self {
            allowed_directories,
        }
    }

    fn validate_path(&self, path: &std::path::Path) -> Result<()> {
        // Reject path traversal
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Error::Security("path traversal not allowed".into()));
        }

        if let Some(allowed) = &self.allowed_directories {
            let canonical = path
                .canonicalize()
                .map_err(|e| Error::Agent(format!("cannot resolve path: {e}")))?;
            if !allowed.iter().any(|dir| canonical.starts_with(dir)) {
                return Err(Error::Security("path outside allowed directories".into()));
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file at the given path."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to read"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'path' parameter".into()))?;

        let path = PathBuf::from(path_str);
        self.validate_path(&path)?;

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| Error::Agent(format!("cannot read file metadata: {e}")))?;

        if metadata.len() > MAX_READ_BYTES {
            return Ok(ToolOutput::error(format!(
                "file too large: {} bytes (limit: {} bytes)",
                metadata.len(),
                MAX_READ_BYTES
            )));
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| Error::Agent(format!("failed to read file: {e}")))?;

        Ok(ToolOutput::success(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn reads_existing_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "hello world").unwrap();

        let tool = FileReadTool::new(None);
        let output = tool
            .execute(serde_json::json!({"path": tmp.path().to_str().unwrap()}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "hello world");
    }

    #[tokio::test]
    async fn returns_error_for_missing_file() {
        let tool = FileReadTool::new(None);
        let result = tool
            .execute(serde_json::json!({"path": "/nonexistent/file.txt"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn returns_error_on_missing_param() {
        let tool = FileReadTool::new(None);
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }
}
