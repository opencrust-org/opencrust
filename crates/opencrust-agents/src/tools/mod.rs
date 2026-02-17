pub mod bash_tool;
pub mod file_read_tool;
pub mod file_write_tool;
pub mod web_fetch_tool;

pub use bash_tool::BashTool;
pub use file_read_tool::FileReadTool;
pub use file_write_tool::FileWriteTool;
pub use web_fetch_tool::WebFetchTool;

use async_trait::async_trait;
use opencrust_common::Result;
use serde::{Deserialize, Serialize};

/// Trait for tools that agents can invoke (bash, browser, file operations, etc.).
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ToolOutput;

    #[test]
    fn success_helper_sets_non_error_state() {
        let output = ToolOutput::success("done");
        assert_eq!(output.content, "done");
        assert!(!output.is_error);
    }

    #[test]
    fn error_helper_sets_error_state() {
        let output = ToolOutput::error("failed");
        assert_eq!(output.content, "failed");
        assert!(output.is_error);
    }
}
