pub mod bash_tool;
pub mod create_skill_tool;
pub mod doc_search_tool;
pub mod file_read_tool;
pub mod file_write_tool;
pub mod google_search_tool;
pub mod handoff_tool;
pub mod schedule;
pub mod web_fetch_tool;
pub mod web_search_tool;

pub use bash_tool::BashTool;
pub use create_skill_tool::CreateSkillTool;
pub use doc_search_tool::DocSearchTool;
pub use file_read_tool::FileReadTool;
pub use file_write_tool::FileWriteTool;
pub use google_search_tool::GoogleSearchTool;
pub use handoff_tool::{HandoffHandle, HandoffTool};
pub use schedule::{CancelHeartbeat, ListHeartbeats, ScheduleHeartbeat};
pub use web_fetch_tool::WebFetchTool;
pub use web_search_tool::WebSearchTool;

use async_trait::async_trait;
use opencrust_common::Result;
use serde::{Deserialize, Serialize};

/// Context passed to tools during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContext {
    pub session_id: String,
    pub user_id: Option<String>,
    /// Heartbeat nesting depth. 0 = normal user request, 1+ = heartbeat execution.
    /// Scheduling is allowed up to depth 3 to enable chaining.
    #[serde(default)]
    pub heartbeat_depth: u8,
    /// When set, only tools in this list may be executed.
    /// Empty list means no tools are allowed; `None` means all tools are allowed.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
}

/// Trait for tools that agents can invoke (bash, browser, file operations, etc.).
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, context: &ToolContext, input: serde_json::Value) -> Result<ToolOutput>;
    /// Optional guidance for the system prompt about when to use this tool.
    fn system_hint(&self) -> Option<&str> {
        None
    }
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
