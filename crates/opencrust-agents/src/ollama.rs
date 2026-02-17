use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::{BoxStream, StreamExt, TryStreamExt};
use opencrust_common::{Error, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::providers::{
    ChatRole, ContentBlock, LlmProvider, LlmRequest, LlmResponse, MessagePart, Usage,
};

const DEFAULT_MODEL: &str = "llama3.1";
const DEFAULT_BASE_URL: &str = "http://localhost:11434";

#[derive(Clone)]
pub struct OllamaProvider {
    base_url: String,
    model: String,
    client: Client,
}

impl OllamaProvider {
    pub fn new(model: Option<String>, base_url: Option<String>) -> Self {
        Self {
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            client: Client::new(),
        }
    }

    fn build_request_body(&self, request: &LlmRequest, stream: bool) -> Value {
        let model = if request.model.is_empty() {
            self.model.clone()
        } else {
            request.model.clone()
        };

        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|msg| {
                let (content, images) = match &msg.content {
                    MessagePart::Text(text) => (text.clone(), Vec::new()),
                    MessagePart::Parts(parts) => {
                        let mut text_parts = Vec::new();
                        let mut images = Vec::new();

                        for part in parts {
                            match part {
                                ContentBlock::Text { text } => text_parts.push(text.clone()),
                                ContentBlock::Image { url } => {
                                    let b64 =
                                        if let Some(stripped) = url.strip_prefix("data:image/") {
                                            if let Some(idx) = stripped.find(";base64,") {
                                                stripped[idx + 8..].to_string()
                                            } else {
                                                url.clone()
                                            }
                                        } else {
                                            url.clone()
                                        };
                                    images.push(b64);
                                }
                                _ => {}
                            }
                        }

                        (text_parts.join("\n"), images)
                    }
                };

                let mut msg_obj = serde_json::json!({
                    "role": match msg.role {
                        ChatRole::System => "system",
                        ChatRole::User => "user",
                        ChatRole::Assistant => "assistant",
                        ChatRole::Tool => "tool",
                    },
                    "content": content,
                });

                if !images.is_empty() {
                    msg_obj["images"] = serde_json::json!(images);
                }

                msg_obj
            })
            .collect();

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": stream,
        });

        let mut options = serde_json::Map::new();
        if let Some(temp) = request.temperature {
            options.insert("temperature".to_string(), serde_json::json!(temp));
        }
        if let Some(max_tokens) = request.max_tokens {
            options.insert("num_predict".to_string(), serde_json::json!(max_tokens));
        }
        if !options.is_empty()
            && let Some(obj) = body.as_object_mut()
        {
            obj.insert("options".to_string(), Value::Object(options));
        }

        body
    }

    pub async fn stream_complete(
        &self,
        request: &LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmResponse>>> {
        let body = self.build_request_body(request, true);
        let url = format!("{}/api/chat", self.base_url);

        let res = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("ollama request failed: {e}")))?;

        if !res.status().is_success() {
            return Err(Error::Agent(format!(
                "ollama error status: {}",
                res.status()
            )));
        }

        let stream = res
            .bytes_stream()
            .map_err(|e| Error::Agent(format!("stream error: {e}")));
        let stream: BoxStream<'static, Result<Bytes>> = Box::pin(stream);

        let lines = futures::stream::unfold(
            (stream, Vec::new()),
            |(mut stream, mut buffer): (BoxStream<'static, Result<Bytes>>, Vec<u8>)| async move {
                loop {
                    if let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                        let line_bytes: Vec<u8> = buffer.drain(0..=pos).collect();
                        let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1])
                            .to_string();
                        if !line.is_empty() {
                            return Some((Ok(line), (stream, buffer)));
                        }
                        continue;
                    }

                    match stream.next().await {
                        Some(Ok(chunk)) => buffer.extend_from_slice(&chunk),
                        Some(Err(e)) => return Some((Err(e), (stream, buffer))),
                        None => {
                            if !buffer.is_empty() {
                                let line = String::from_utf8_lossy(&buffer).to_string();
                                if !line.is_empty() {
                                    return Some((Ok(line), (stream, Vec::new())));
                                }
                            }
                            return None;
                        }
                    }
                }
            },
        );

        let output = lines
            .map(|line_res: Result<String>| {
                let line = line_res?;
                let ollama_res: OllamaResponse = serde_json::from_str(&line)
                    .map_err(|e| Error::Agent(format!("failed to parse stream chunk: {e}")))?;

                let content = ollama_res
                    .message
                    .map(|msg| vec![ContentBlock::Text { text: msg.content }])
                    .unwrap_or_default();

                Ok(Some(LlmResponse {
                    content,
                    model: ollama_res.model,
                    usage: if ollama_res.done {
                        Some(Usage {
                            input_tokens: ollama_res.prompt_eval_count,
                            output_tokens: ollama_res.eval_count,
                        })
                    } else {
                        None
                    },
                    stop_reason: if ollama_res.done {
                        Some("stop".to_string())
                    } else {
                        None
                    },
                }))
            })
            .try_filter_map(|x| async move { Ok(x) });

        Ok(Box::pin(output))
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/tags", self.base_url);
        let res = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("failed to list models: {e}")))?;

        if !res.status().is_success() {
            return Err(Error::Agent(format!(
                "ollama error status: {}",
                res.status()
            )));
        }

        let models_res: OllamaModelsResponse = res
            .json()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse models response: {e}")))?;

        Ok(models_res.models.into_iter().map(|m| m.name).collect())
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn provider_id(&self) -> &str {
        "ollama"
    }

    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let body = self.build_request_body(request, false);
        let url = format!("{}/api/chat", self.base_url);

        let res = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("ollama request failed: {e}")))?;

        if !res.status().is_success() {
            return Err(Error::Agent(format!(
                "ollama error status: {}",
                res.status()
            )));
        }

        let ollama_res: OllamaResponse = res
            .json()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse ollama response: {e}")))?;

        let content = ollama_res
            .message
            .map(|msg| vec![ContentBlock::Text { text: msg.content }])
            .unwrap_or_default();

        Ok(LlmResponse {
            content,
            model: ollama_res.model,
            usage: Some(Usage {
                input_tokens: ollama_res.prompt_eval_count,
                output_tokens: ollama_res.eval_count,
            }),
            stop_reason: if ollama_res.done {
                Some("stop".to_string())
            } else {
                None
            },
        })
    }

    async fn health_check(&self) -> Result<bool> {
        match self.list_models().await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}

#[derive(Deserialize)]
struct OllamaResponse {
    model: String,
    message: Option<OllamaMessage>,
    done: bool,
    #[serde(default)]
    eval_count: u32,
    #[serde(default)]
    prompt_eval_count: u32,
}

#[derive(Deserialize)]
struct OllamaMessage {
    content: String,
}

#[derive(Deserialize)]
struct OllamaModelsResponse {
    models: Vec<OllamaModel>,
}

#[derive(Deserialize)]
struct OllamaModel {
    name: String,
}

#[cfg(test)]
mod tests {
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use futures::StreamExt;
    use serde_json::{Value, json};
    use tokio::sync::oneshot;

    use crate::providers::{
        ChatMessage, ChatRole, ContentBlock, LlmProvider, LlmRequest, MessagePart,
    };

    use super::OllamaProvider;

    #[test]
    fn request_serialization_includes_options() {
        let provider = OllamaProvider::new(None, None);
        let req = LlmRequest {
            model: "llama3".to_string(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Text("Hello".to_string()),
            }],
            system: None,
            max_tokens: Some(100),
            temperature: Some(0.7),
            tools: vec![],
        };

        let body = provider.build_request_body(&req, false);

        assert_eq!(body["model"], "llama3");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Hello");
        assert_eq!(body["options"]["temperature"], 0.7);
        assert_eq!(body["options"]["num_predict"], 100);
    }

    async fn run_mock_server() -> (String, oneshot::Sender<()>) {
        let (tx, rx) = oneshot::channel::<()>();

        let app = Router::new()
            .route(
                "/api/tags",
                get(|| async {
                    Json(json!({
                        "models": [
                            { "name": "llama3:latest" },
                            { "name": "mistral:latest" }
                        ]
                    }))
                }),
            )
            .route(
                "/api/chat",
                post(|Json(payload): Json<Value>| async move {
                    let stream = payload
                        .get("stream")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    if stream {
                        "{\"model\":\"llama3\",\"message\":{\"content\":\"Hello\"},\"done\":false}\n{\"model\":\"llama3\",\"message\":{\"content\":\" World\"},\"done\":true}".to_string()
                    } else {
                        serde_json::to_string(&json!({
                            "model": "llama3",
                            "message": { "content": "Hello World" },
                            "done": true,
                            "prompt_eval_count": 10,
                            "eval_count": 5
                        }))
                        .unwrap()
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}", addr);

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });

        (url, tx)
    }

    #[tokio::test]
    async fn list_models_works() {
        let (url, stop) = run_mock_server().await;
        let provider = OllamaProvider::new(None, Some(url));

        let models = provider.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.contains(&"llama3:latest".to_string()));

        let _ = stop.send(());
    }

    #[tokio::test]
    async fn complete_works() {
        let (url, stop) = run_mock_server().await;
        let provider = OllamaProvider::new(None, Some(url));

        let req = LlmRequest {
            model: "llama3".to_string(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Text("Hi".to_string()),
            }],
            system: None,
            max_tokens: None,
            temperature: None,
            tools: vec![],
        };

        let res = provider.complete(&req).await.unwrap();
        match &res.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello World"),
            _ => panic!("expected text content"),
        }

        let _ = stop.send(());
    }

    #[tokio::test]
    async fn stream_complete_works() {
        let (url, stop) = run_mock_server().await;
        let provider = OllamaProvider::new(None, Some(url));

        let req = LlmRequest {
            model: "llama3".to_string(),
            messages: vec![],
            system: None,
            max_tokens: None,
            temperature: None,
            tools: vec![],
        };

        let mut stream = provider.stream_complete(&req).await.unwrap();
        let mut full_text = String::new();
        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res.unwrap();
            if let ContentBlock::Text { text } = &chunk.content[0] {
                full_text.push_str(text);
            }
        }

        assert_eq!(full_text, "Hello World");
        let _ = stop.send(());
    }
}
