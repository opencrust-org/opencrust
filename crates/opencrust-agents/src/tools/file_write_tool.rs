use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::path::{Path, PathBuf};

use super::{Tool, ToolContext, ToolOutput};

const MAX_WRITE_BYTES: usize = 1024 * 1024; // 1MB

/// Write content to a file with path validation and size limits.
pub struct FileWriteTool {
    allowed_directories: Option<Vec<PathBuf>>,
    protected_config_dir: PathBuf,
}

impl FileWriteTool {
    pub fn new(allowed_directories: Option<Vec<PathBuf>>) -> Self {
        Self::new_with_config_dir(
            allowed_directories,
            opencrust_config::ConfigLoader::default_config_dir(),
        )
    }

    fn new_with_config_dir(
        allowed_directories: Option<Vec<PathBuf>>,
        protected_config_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            allowed_directories,
            protected_config_dir: protected_config_dir.into(),
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

    fn should_backup_before_write(&self, path: &std::path::Path) -> bool {
        is_protected_agent_state_path(path, &self.protected_config_dir)
    }
}

/// File-write-managed agent state that should be backed up before overwrite.
///
/// This covers only explicitly protected files in the config directory:
/// - `dna.md`
/// - `mcp.json`
fn is_protected_agent_state_path(path: &Path, config_dir: &Path) -> bool {
    let path = normalize_with_existing_parent(path);
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    if path.parent() != Some(config_dir.as_path()) {
        return false;
    }

    path.file_name()
        .is_some_and(|name| name == "dna.md" || name == "mcp.json")
}

fn normalize_with_existing_parent(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let Some(file_name) = path.file_name() else {
        return path.to_path_buf();
    };

    parent
        .canonicalize()
        .map(|parent| parent.join(file_name))
        .unwrap_or_else(|_| path.to_path_buf())
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file at the given path. Creates the file if it doesn't exist, overwrites if it does."
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

        if self.should_backup_before_write(&path) {
            opencrust_config::try_backup_file(&path);
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
            allowed_tools: None,
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
            allowed_tools: None,
        };
        assert!(tool.execute(&ctx, serde_json::json!({})).await.is_err());
        assert!(
            tool.execute(&ctx, serde_json::json!({"path": "/tmp/test"}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn backs_up_existing_protected_config_files_before_overwrite() {
        let dir = TempDir::new().unwrap();
        for file_name in ["dna.md", "mcp.json"] {
            let file_path = dir.path().join(file_name);
            std::fs::write(&file_path, format!("original {file_name}")).unwrap();

            let tool = FileWriteTool::new_with_config_dir(None, dir.path());
            let ctx = ToolContext {
                session_id: "test".into(),
                user_id: None,
                heartbeat_depth: 0,
                allowed_tools: None,
            };
            let output = tool
                .execute(
                    &ctx,
                    serde_json::json!({
                        "path": file_path.to_str().unwrap(),
                        "content": format!("updated {file_name}")
                    }),
                )
                .await
                .unwrap();

            assert!(!output.is_error);
            assert!(
                std::fs::read_to_string(&file_path)
                    .unwrap()
                    .contains(&format!("updated {file_name}"))
            );
            assert!(
                std::fs::read_to_string(file_path.with_file_name(format!("{file_name}.bak.1")))
                    .unwrap()
                    .contains(&format!("original {file_name}"))
            );
        }
    }

    #[test]
    fn protected_agent_state_paths_cover_explicit_file_write_state_only() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("skills/example"))
            .expect("failed to create temp dir");

        assert!(is_protected_agent_state_path(
            &dir.path().join("dna.md"),
            dir.path()
        ));
        assert!(is_protected_agent_state_path(
            &dir.path().join("mcp.json"),
            dir.path()
        ));
        assert!(!is_protected_agent_state_path(
            &dir.path().join("notes.md"),
            dir.path()
        ));

        assert!(!is_protected_agent_state_path(
            &dir.path().join("config.yml"),
            dir.path()
        ));
        assert!(!is_protected_agent_state_path(
            &dir.path().join("dna.md.bak.1"),
            dir.path()
        ));
        assert!(!is_protected_agent_state_path(
            &dir.path().join("skills/example/SKILL.md"),
            dir.path()
        ));
    }
}
