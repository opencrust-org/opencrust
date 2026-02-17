use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::time::Duration;

use super::{Tool, ToolOutput};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024; // 1MB

/// Fetch content from a URL with timeout, size limits, and domain blocking.
pub struct WebFetchTool {
    client: reqwest::Client,
    blocked_domains: Vec<String>,
}

impl WebFetchTool {
    pub fn new(blocked_domains: Option<Vec<String>>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .unwrap_or_default();

        Self {
            client,
            blocked_domains: blocked_domains.unwrap_or_default(),
        }
    }

    fn is_blocked(&self, url: &str) -> bool {
        self.blocked_domains
            .iter()
            .any(|domain| url.contains(domain))
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch the content of a web page at the given URL. Returns the response body as text."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let url = input
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'url' parameter".into()))?;

        if self.is_blocked(url) {
            return Ok(ToolOutput::error("domain is blocked".to_string()));
        }

        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("fetch failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Ok(ToolOutput::error(format!("HTTP {status}")));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Agent(format!("failed to read response body: {e}")))?;

        if bytes.len() > MAX_RESPONSE_BYTES {
            let truncated = String::from_utf8_lossy(&bytes[..MAX_RESPONSE_BYTES]);
            return Ok(ToolOutput::success(format!(
                "{}\n... (response truncated at {} bytes)",
                truncated, MAX_RESPONSE_BYTES
            )));
        }

        let text = String::from_utf8_lossy(&bytes);
        Ok(ToolOutput::success(text.into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_domains() {
        let tool = WebFetchTool::new(Some(vec!["evil.com".to_string()]));
        assert!(tool.is_blocked("https://evil.com/path"));
        assert!(!tool.is_blocked("https://good.com/path"));
    }

    #[test]
    fn returns_error_on_missing_url() {
        let tool = WebFetchTool::new(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(tool.execute(serde_json::json!({})));
        assert!(result.is_err());
    }
}
