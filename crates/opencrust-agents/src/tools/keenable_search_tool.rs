use async_trait::async_trait;
use opencrust_common::{Error, Result};
use serde::Deserialize;
use std::time::Duration;

use super::{Tool, ToolContext, ToolOutput};

const SEARCH_TIMEOUT_SECS: u64 = 15;
const DEFAULT_COUNT: u64 = 5;
const MAX_COUNT: u64 = 10;
const KEENABLE_BASE_URL: &str = "https://api.keenable.ai";
const KEENABLE_APP_TITLE: &str = "OpenCrust";

/// Search the web using the Keenable Search API.
///
/// Works without an API key through the public endpoint (rate limited per IP).
/// With a key, uses the keyed endpoint, which has no hourly cap.
pub struct KeenableSearchTool {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
}

impl KeenableSearchTool {
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_base_url(api_key, KEENABLE_BASE_URL.to_string())
    }

    pub fn with_base_url(api_key: Option<String>, base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS))
            .build()
            .unwrap_or_default();

        Self {
            client,
            api_key: api_key.filter(|k| !k.trim().is_empty()),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct KeenableSearchResponse {
    results: Option<Vec<KeenableSearchResult>>,
}

#[derive(Debug, Deserialize)]
struct KeenableSearchResult {
    title: String,
    url: String,
    #[serde(default)]
    snippet: String,
}

#[async_trait]
impl Tool for KeenableSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for a query using Keenable and return top results with title, snippet, and URL."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "count": {
                    "type": "number",
                    "description": "Number of results to return (1-10, default 5)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'query' parameter".into()))?;

        let count = input
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_COUNT)
            .clamp(1, MAX_COUNT);

        let body = serde_json::json!({ "query": query, "max_results": count });
        let request = match &self.api_key {
            Some(key) => self
                .client
                .post(format!("{}/v1/search", self.base_url))
                .header("X-API-Key", key),
            None => self
                .client
                .post(format!("{}/v1/search/public", self.base_url))
                .header("X-Keenable-Title", KEENABLE_APP_TITLE),
        };

        let response = request
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("keenable search request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Ok(ToolOutput::error(format!(
                "Keenable Search API error: HTTP {status} — {error_text}"
            )));
        }

        let body: KeenableSearchResponse = response
            .json()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse keenable search response: {e}")))?;

        let results = match body.results {
            Some(items) if !items.is_empty() => items,
            _ => return Ok(ToolOutput::error("No search results found.")),
        };

        let mut output = String::new();
        for (i, result) in results.iter().take(count as usize).enumerate() {
            output.push_str(&format!(
                "{}. **{}** — {}\n   {}\n\n",
                i + 1,
                result.title,
                result.url,
                result.snippet,
            ));
        }

        Ok(ToolOutput::success(output.trim_end()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_context() -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
            allowed_tools: None,
        }
    }

    fn two_results() -> serde_json::Value {
        serde_json::json!({
            "query": "rust programming",
            "results": [
                {
                    "title": "Rust Programming Language",
                    "url": "https://www.rust-lang.org",
                    "snippet": "A language empowering everyone to build reliable software."
                },
                {
                    "title": "Learn Rust",
                    "url": "https://doc.rust-lang.org/book/"
                }
            ]
        })
    }

    #[test]
    fn returns_error_on_missing_query() {
        let tool = KeenableSearchTool::new(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(tool.execute(&test_context(), serde_json::json!({})));
        assert!(result.is_err());
    }

    #[test]
    fn blank_api_key_means_keyless() {
        let tool = KeenableSearchTool::new(Some("  ".into()));
        assert!(tool.api_key.is_none());
    }

    #[tokio::test]
    async fn keyless_uses_public_endpoint_with_title_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/search/public"))
            .and(header("X-Keenable-Title", "OpenCrust"))
            .and(body_json(
                serde_json::json!({"query": "rust programming", "max_results": 5}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(two_results()))
            .expect(1)
            .mount(&server)
            .await;

        let tool = KeenableSearchTool::with_base_url(None, server.uri());
        let result = tool
            .execute(
                &test_context(),
                serde_json::json!({"query": "rust programming"}),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(
            result
                .content
                .contains("1. **Rust Programming Language** — https://www.rust-lang.org")
        );
        assert!(result.content.contains("2. **Learn Rust**"));
    }

    #[tokio::test]
    async fn api_key_uses_keyed_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/search"))
            .and(header("X-API-Key", "test-key"))
            .and(body_json(
                serde_json::json!({"query": "test", "max_results": 3}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(two_results()))
            .expect(1)
            .mount(&server)
            .await;

        let tool = KeenableSearchTool::with_base_url(Some("test-key".into()), server.uri());
        let result = tool
            .execute(
                &test_context(),
                serde_json::json!({"query": "test", "count": 3}),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn returns_error_output_on_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/search/public"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&server)
            .await;

        let tool = KeenableSearchTool::with_base_url(None, server.uri());
        let result = tool
            .execute(&test_context(), serde_json::json!({"query": "test"}))
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("429"));
    }

    #[tokio::test]
    async fn returns_error_output_on_empty_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/search/public"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"query": "x", "results": []})),
            )
            .mount(&server)
            .await;

        let tool = KeenableSearchTool::with_base_url(None, server.uri());
        let result = tool
            .execute(&test_context(), serde_json::json!({"query": "x"}))
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("No search results found"));
    }

    /// Live call against the real API. Run with
    /// `cargo test -p opencrust-agents keenable -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_keyless_search() {
        let tool = KeenableSearchTool::new(None);
        let result = tool
            .execute(
                &test_context(),
                serde_json::json!({"query": "rust async runtime", "count": 3}),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("1. **"));
    }
}
