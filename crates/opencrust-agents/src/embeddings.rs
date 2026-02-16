use async_trait::async_trait;
use opencrust_common::{Error, Result};
use serde::{Deserialize, Serialize};

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn model(&self) -> &str;
    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    async fn health_check(&self) -> Result<bool>;
}

/// Cohere embeddings provider.
pub struct CohereEmbeddingProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl CohereEmbeddingProvider {
    pub fn new(
        api_key: impl Into<String>,
        model: Option<String>,
        base_url: Option<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            model: model.unwrap_or_else(|| "embed-english-v3.0".to_string()),
            base_url: base_url.unwrap_or_else(|| "https://api.cohere.com".to_string()),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/embed", self.base_url.trim_end_matches('/'))
    }

    fn build_request_body(&self, texts: &[String], input_type: &str) -> CohereEmbedRequest {
        CohereEmbedRequest {
            model: self.model.clone(),
            texts: texts.to_vec(),
            input_type: input_type.to_string(),
            embedding_types: vec!["float".to_string()],
            truncate: "END".to_string(),
        }
    }

    async fn embed_with_input_type(
        &self,
        texts: &[String],
        input_type: &str,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let response = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .json(&self.build_request_body(texts, input_type))
            .send()
            .await
            .map_err(|e| Error::Agent(format!("cohere request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "cohere embed request failed: status={}, body={}",
                status, body
            )));
        }

        let payload: CohereEmbedResponse = response
            .json()
            .await
            .map_err(|e| Error::Agent(format!("failed to decode cohere response: {e}")))?;

        payload.into_float_embeddings()
    }
}

#[async_trait]
impl EmbeddingProvider for CohereEmbeddingProvider {
    fn provider_id(&self) -> &str {
        "cohere"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_with_input_type(texts, "search_document").await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let texts = vec![text.to_string()];
        let mut embeddings = self.embed_with_input_type(&texts, "search_query").await?;
        embeddings
            .pop()
            .ok_or_else(|| Error::Agent("cohere returned no embeddings for query".into()))
    }

    async fn health_check(&self) -> Result<bool> {
        Ok(self.embed_query("health check").await.is_ok())
    }
}

#[derive(Debug, Clone, Serialize)]
struct CohereEmbedRequest {
    model: String,
    texts: Vec<String>,
    input_type: String,
    embedding_types: Vec<String>,
    truncate: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CohereEmbedResponse {
    embeddings: Option<CohereEmbeddings>,
}

#[derive(Debug, Clone, Deserialize)]
struct CohereEmbeddings {
    float: Option<Vec<Vec<f32>>>,
}

impl CohereEmbedResponse {
    fn into_float_embeddings(self) -> Result<Vec<Vec<f32>>> {
        self.embeddings
            .and_then(|e| e.float)
            .ok_or_else(|| Error::Agent("cohere response missing float embeddings".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::{CohereEmbedResponse, CohereEmbeddingProvider};

    #[test]
    fn builds_expected_request_shape() {
        let provider =
            CohereEmbeddingProvider::new("test-key", Some("embed-english-v3.0".into()), None);
        let body = provider.build_request_body(
            &["hello".to_string(), "world".to_string()],
            "search_document",
        );
        assert_eq!(body.model, "embed-english-v3.0");
        assert_eq!(body.input_type, "search_document");
        assert_eq!(body.embedding_types, vec!["float".to_string()]);
        assert_eq!(body.texts.len(), 2);
    }

    #[test]
    fn parses_float_embeddings_payload() {
        let payload: CohereEmbedResponse = serde_json::from_str(
            r#"{
                "embeddings": {
                    "float": [[0.1, 0.2, 0.3], [0.9, 0.1, 0.0]]
                }
            }"#,
        )
        .expect("json should parse");

        let vectors = payload
            .into_float_embeddings()
            .expect("should contain float embeddings");
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors[0], vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn endpoint_is_normalized_without_double_slashes() {
        let provider = CohereEmbeddingProvider::new(
            "test-key",
            Some("embed-english-v3.0".into()),
            Some("https://api.cohere.com/".into()),
        );
        assert_eq!(provider.endpoint(), "https://api.cohere.com/v1/embed");
    }
}
