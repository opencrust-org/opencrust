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
    use super::*;

    #[test]
    fn test_tool_output_success() {
        let output = ToolOutput::success("Operation successful");
        assert_eq!(output.content, "Operation successful");
        assert!(!output.is_error);
    }

    #[test]
    fn test_tool_output_error() {
        let output = ToolOutput::error("Operation failed");
        assert_eq!(output.content, "Operation failed");
        assert!(output.is_error);
    }

    #[test]
    fn test_tool_output_from_string() {
        let content = String::from("Owned string content");
        let output = ToolOutput::success(content.clone());
        assert_eq!(output.content, content);
        assert!(!output.is_error);
    }
}
