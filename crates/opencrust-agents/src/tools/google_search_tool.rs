use async_trait::async_trait;
use opencrust_common::{Error, Result};
use serde::Deserialize;
use std::time::Duration;

use super::{Tool, ToolContext, ToolOutput};

const SEARCH_TIMEOUT_SECS: u64 = 15;
const DEFAULT_COUNT: u64 = 5;
const MAX_COUNT: u64 = 10;
const GOOGLE_SEARCH_BASE_URL: &str = "https://www.googleapis.com/customsearch/v1";

/// Search the web using the Google Custom Search JSON API.
pub struct GoogleSearchTool {
    client: reqwest::Client,
    api_key: String,
    cx: String,
    base_url: String,
}

impl GoogleSearchTool {
    pub fn new(api_key: String, cx: String) -> Self {
        Self::with_base_url(api_key, cx, GOOGLE_SEARCH_BASE_URL.to_string())
    }

    pub fn with_base_url(api_key: String, cx: String, base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS))
            .build()
            .unwrap_or_default();

        Self {
            client,
            api_key,
            cx,
            base_url,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GoogleSearchResponse {
    items: Option<Vec<GoogleSearchResult>>,
}

#[derive(Debug, Deserialize)]
struct GoogleSearchResult {
    title: String,
    link: String,
    snippet: String,
}

#[async_trait]
impl Tool for GoogleSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for a query using Google and return top results with title, snippet, and URL."
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

        let response = self
            .client
            .get(&self.base_url)
            .query(&[
                ("key", &self.api_key),
                ("cx", &self.cx),
                ("q", &query.to_string()),
                ("num", &count.to_string()),
            ])
            .send()
            .await
            .map_err(|e| Error::Agent(format!("google search request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Ok(ToolOutput::error(format!(
                "Google Search API error: HTTP {status} — {error_text}"
            )));
        }

        let body: GoogleSearchResponse = response
            .json()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse google search response: {e}")))?;

        let results = match body.items {
            Some(items) if !items.is_empty() => items,
            _ => return Ok(ToolOutput::error("No search results found.")),
        };

        let mut output = String::new();
        for (i, result) in results.iter().enumerate() {
            output.push_str(&format!(
                "{}. **{}** — {}\n   {}\n\n",
                i + 1,
                result.title,
                result.link,
                result.snippet,
            ));
        }

        Ok(ToolOutput::success(output.trim_end()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_context() -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
        }
    }

    #[test]
    fn returns_error_on_missing_query() {
        let tool = GoogleSearchTool::new("test-key".into(), "test-cx".into());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(tool.execute(&test_context(), serde_json::json!({})));
        assert!(result.is_err());
    }

    #[test]
    fn formats_results_correctly() {
        let results = [GoogleSearchResult {
            title: "Rust Lang".into(),
            link: "https://www.rust-lang.org".into(),
            snippet: "A systems programming language.".into(),
        }];

        let mut output = String::new();
        for (i, result) in results.iter().enumerate() {
            output.push_str(&format!(
                "{}. **{}** — {}\n   {}\n\n",
                i + 1,
                result.title,
                result.link,
                result.snippet,
            ));
        }
        let output = output.trim_end();

        assert!(output.starts_with("1. **Rust Lang**"));
        assert!(output.contains("https://www.rust-lang.org"));
        assert!(output.contains("A systems programming language."));
    }

    #[tokio::test]
    async fn returns_formatted_results_from_api() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("q", "rust programming"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    {
                        "title": "Rust Programming Language",
                        "link": "https://www.rust-lang.org",
                        "snippet": "A language empowering everyone to build reliable software."
                    },
                    {
                        "title": "Learn Rust",
                        "link": "https://doc.rust-lang.org/book/",
                        "snippet": "The Rust Programming Language book."
                    }
                ]
            })))
            .mount(&server)
            .await;

        let tool =
            GoogleSearchTool::with_base_url("test-key".into(), "test-cx".into(), server.uri());
        let result = tool
            .execute(
                &test_context(),
                serde_json::json!({"query": "rust programming"}),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("1. **Rust Programming Language**"));
        assert!(result.content.contains("https://www.rust-lang.org"));
        assert!(result.content.contains("2. **Learn Rust**"));
        assert!(result.content.contains("https://doc.rust-lang.org/book/"));
    }

    #[tokio::test]
    async fn returns_error_output_on_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"error":{"message":"API key invalid"}}"#),
            )
            .mount(&server)
            .await;

        let tool =
            GoogleSearchTool::with_base_url("bad-key".into(), "test-cx".into(), server.uri());
        let result = tool
            .execute(&test_context(), serde_json::json!({"query": "anything"}))
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("403"));
    }

    #[tokio::test]
    async fn returns_error_output_on_empty_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": []
            })))
            .mount(&server)
            .await;

        let tool =
            GoogleSearchTool::with_base_url("test-key".into(), "test-cx".into(), server.uri());
        let result = tool
            .execute(&test_context(), serde_json::json!({"query": "xyzzy12345"}))
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("No search results found"));
    }

    #[tokio::test]
    async fn returns_error_output_on_missing_items_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "searchInformation": {"totalResults": "0"}
            })))
            .mount(&server)
            .await;

        let tool =
            GoogleSearchTool::with_base_url("test-key".into(), "test-cx".into(), server.uri());
        let result = tool
            .execute(
                &test_context(),
                serde_json::json!({"query": "nothing here"}),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("No search results found"));
    }

    #[tokio::test]
    async fn respects_count_parameter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("num", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    {"title": "A", "link": "https://a.com", "snippet": "alpha"},
                    {"title": "B", "link": "https://b.com", "snippet": "beta"},
                    {"title": "C", "link": "https://c.com", "snippet": "gamma"}
                ]
            })))
            .mount(&server)
            .await;

        let tool =
            GoogleSearchTool::with_base_url("test-key".into(), "test-cx".into(), server.uri());
        let result = tool
            .execute(
                &test_context(),
                serde_json::json!({"query": "test", "count": 3}),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("1. **A**"));
        assert!(result.content.contains("2. **B**"));
        assert!(result.content.contains("3. **C**"));
    }
}
