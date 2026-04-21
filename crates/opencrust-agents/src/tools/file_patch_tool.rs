use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolOutput};

const MAX_FILE_BYTES: u64 = 1024 * 1024; // 1 MB

/// Apply a targeted find-and-replace edit to a file without rewriting the whole thing.
pub struct FilePatchTool {
    allowed_directories: Option<Vec<PathBuf>>,
}

impl FilePatchTool {
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
impl Tool for FilePatchTool {
    fn name(&self) -> &str {
        "file_patch"
    }

    fn description(&self) -> &str {
        "Apply a targeted find-and-replace edit to a file. \
         Safer than file_write for editing existing files — only the changed region is touched. \
         Fails if old_string is not found or matches more than once \
         (use replace_all: true to replace every occurrence)."
    }

    fn system_hint(&self) -> Option<&str> {
        Some(
            "Prefer file_patch over file_write when editing existing files — it is safer and \
             produces minimal diffs. Provide enough context in old_string to make the match \
             unique. Use replace_all: true only when you intentionally want every occurrence \
             replaced.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find. Must match exactly once unless replace_all is true."
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence of old_string (default: false)"
                }
            },
            "required": ["path", "old_string", "new_string"]
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

        let old_string = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'old_string' parameter".into()))?;

        let new_string = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'new_string' parameter".into()))?;

        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = PathBuf::from(path_str);
        self.validate_path(&path)?;

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| Error::Agent(format!("cannot read file metadata: {e}")))?;

        if metadata.len() > MAX_FILE_BYTES {
            return Ok(ToolOutput::error(format!(
                "file too large: {} bytes (limit: {} bytes)",
                metadata.len(),
                MAX_FILE_BYTES
            )));
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| Error::Agent(format!("failed to read file: {e}")))?;

        let count = content.matches(old_string).count();

        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "old_string not found in {path_str}"
            )));
        }

        if count > 1 && !replace_all {
            return Ok(ToolOutput::error(format!(
                "old_string matches {count} times in {path_str} — \
                 add more context to make it unique, or use replace_all: true"
            )));
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        tokio::fs::write(&path, &new_content)
            .await
            .map_err(|e| Error::Agent(format!("failed to write file: {e}")))?;

        Ok(ToolOutput::success(format!(
            "patched {path_str}: replaced {count} occurrence(s)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
            allowed_tools: None,
        }
    }

    #[tokio::test]
    async fn patches_single_occurrence() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "hello world").unwrap();

        let tool = FilePatchTool::new(None);
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "path": tmp.path().to_str().unwrap(),
                    "old_string": "world",
                    "new_string": "rust"
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error, "{}", output.content);
        let result = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(result, "hello rust");
    }

    #[tokio::test]
    async fn errors_when_old_string_not_found() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "hello world").unwrap();

        let tool = FilePatchTool::new(None);
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "path": tmp.path().to_str().unwrap(),
                    "old_string": "goodbye",
                    "new_string": "rust"
                }),
            )
            .await
            .unwrap();

        assert!(output.is_error);
        assert!(output.content.contains("not found"));
    }

    #[tokio::test]
    async fn errors_on_ambiguous_match_without_replace_all() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "foo foo foo").unwrap();

        let tool = FilePatchTool::new(None);
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "path": tmp.path().to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "bar"
                }),
            )
            .await
            .unwrap();

        assert!(output.is_error);
        assert!(output.content.contains("3 times"));
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "foo foo foo").unwrap();

        let tool = FilePatchTool::new(None);
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "path": tmp.path().to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "bar",
                    "replace_all": true
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error, "{}", output.content);
        let result = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(result, "bar bar bar");
    }

    #[tokio::test]
    async fn returns_error_on_missing_params() {
        let tool = FilePatchTool::new(None);
        assert!(tool.execute(&ctx(), serde_json::json!({})).await.is_err());
    }
}
