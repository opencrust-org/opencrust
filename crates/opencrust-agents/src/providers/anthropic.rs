use super::{
    ChatRole, ContentBlock, LlmProvider, LlmRequest, LlmResponse, MessagePart, Usage,
};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use opencrust_common::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    client: Client,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: ANTHROPIC_API_URL.to_string(),
            client: Client::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }

    fn prepare_request_body(&self, request: &LlmRequest, stream: bool) -> Result<Value> {
        let mut system_prompt = request.system.clone().unwrap_or_default();
        let mut messages = Vec::new();

        for msg in &request.messages {
            match msg.role {
                ChatRole::System => {
                    // Append to system prompt if found in messages (though LlmRequest has explicit system field)
                    if !system_prompt.is_empty() {
                        system_prompt.push_str("\n");
                    }
                    match &msg.content {
                        MessagePart::Text(t) => system_prompt.push_str(t),
                        MessagePart::Parts(parts) => {
                            for part in parts {
                                if let ContentBlock::Text { text } = part {
                                    system_prompt.push_str(text);
                                }
                            }
                        }
                    }
                }
                ChatRole::User | ChatRole::Assistant => {
                    messages.push(self.convert_message(msg)?);
                }
                ChatRole::Tool => {
                    // Tool results must be user messages
                    messages.push(self.convert_tool_message(msg)?);
                }
            }
        }

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(1024),
            "stream": stream,
        });

        if !system_prompt.is_empty() {
            body["system"] = json!(system_prompt);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
        }

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request.tools.iter().map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema
                })
            }).collect();
            body["tools"] = json!(tools);
        }

        Ok(body)
    }

    fn convert_message(&self, msg: &super::ChatMessage) -> Result<Value> {
        let role = match msg.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            _ => "user", // Should not happen given logic above
        };

        let content = match &msg.content {
            MessagePart::Text(text) => json!(text),
            MessagePart::Parts(parts) => {
                let blocks: Result<Vec<Value>> = parts.iter().map(|p| self.convert_content_block(p)).collect();
                json!(blocks?)
            }
        };

        Ok(json!({
            "role": role,
            "content": content
        }))
    }

    fn convert_tool_message(&self, msg: &super::ChatMessage) -> Result<Value> {
        // Tool results are user messages with tool_result blocks
        let blocks = match &msg.content {
            MessagePart::Parts(parts) => {
                let mut converted = Vec::new();
                for p in parts {
                    if let ContentBlock::ToolResult { tool_use_id, content } = p {
                        converted.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": content
                        }));
                    }
                }
                converted
            }
            _ => vec![],
        };

        Ok(json!({
            "role": "user",
            "content": blocks
        }))
    }

    fn convert_content_block(&self, block: &ContentBlock) -> Result<Value> {
        match block {
            ContentBlock::Text { text } => Ok(json!({
                "type": "text",
                "text": text
            })),
            ContentBlock::Image { url } => {
                // Expect data URL: data:image/png;base64,...
                if url.starts_with("data:") {
                    let parts: Vec<&str> = url.splitn(2, ',').collect();
                    if parts.len() != 2 {
                        return Err(Error::Config("Invalid data URL for image".to_string()));
                    }
                    let meta = parts[0];
                    let data = parts[1];
                    // meta example: data:image/png;base64
                    let media_type = meta.trim_start_matches("data:").trim_end_matches(";base64");

                    Ok(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data
                        }
                    }))
                } else {
                    // For now, fail or maybe treat as unsupported.
                    // Anthropic doesn't support remote URLs directly.
                    Err(Error::Config(format!("Unsupported image URL format (must be base64 data URL): {}", url)))
                }
            }
            ContentBlock::ToolUse { id, name, input } => Ok(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            })),
            ContentBlock::ToolResult { .. } => {
                 Err(Error::Config("ToolResult cannot be used in Assistant/User message blocks directly".to_string()))
            }
        }
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn provider_id(&self) -> &str {
        "anthropic"
    }

    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let body = self.prepare_request_body(request, false)?;

        let res = self.client.post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Gateway(e.to_string()))?;

        if !res.status().is_success() {
            let error_text = res.text().await.unwrap_or_default();
            return Err(Error::Gateway(format!("Anthropic API error: {}", error_text)));
        }

        let resp_json: Value = res.json().await.map_err(|e| Error::Gateway(e.to_string()))?;

        // Parse response
        let content_json = resp_json["content"].as_array().ok_or(Error::Gateway("Missing content".to_string()))?;
        let mut content = Vec::new();
        for item in content_json {
            let type_str = item["type"].as_str().unwrap_or_default();
            match type_str {
                "text" => {
                    content.push(ContentBlock::Text {
                        text: item["text"].as_str().unwrap_or_default().to_string()
                    });
                }
                "tool_use" => {
                    content.push(ContentBlock::ToolUse {
                        id: item["id"].as_str().unwrap_or_default().to_string(),
                        name: item["name"].as_str().unwrap_or_default().to_string(),
                        input: item["input"].clone(),
                    });
                }
                _ => {}
            }
        }

        let model = resp_json["model"].as_str().unwrap_or_default().to_string();
        let stop_reason = resp_json["stop_reason"].as_str().map(|s| s.to_string());

        let usage = if let Some(u) = resp_json.get("usage") {
            Some(Usage {
                input_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
            })
        } else {
            None
        };

        Ok(LlmResponse {
            content,
            model,
            usage,
            stop_reason,
        })
    }

    async fn stream_complete(&self, request: &LlmRequest) -> Result<BoxStream<'static, Result<LlmResponse>>> {
        let body = self.prepare_request_body(request, true)?;

        let res = self.client.post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Gateway(e.to_string()))?;

        if !res.status().is_success() {
            let error_text = res.text().await.unwrap_or_default();
            return Err(Error::Gateway(format!("Anthropic API error: {}", error_text)));
        }

        let stream = res.bytes_stream();

        let response_stream = stream::unfold(
            (stream, Vec::new(), None as Option<(String, String, String)>), // stream, buffer, current_tool_use (id, name, input_json_acc)
            move |(mut stream, mut buffer, mut current_tool)| async move {
                loop {
                    // Try to process buffer line by line
                    if let Some(i) = buffer.iter().position(|&b| b == b'\n') {
                        let line_bytes: Vec<u8> = buffer.drain(..=i).collect();
                        let line = String::from_utf8_lossy(&line_bytes).trim().to_string();

                        if line.is_empty() { continue; }

                        if line.starts_with("event: ") {
                            // verify event type if needed
                            continue;
                        }

                        if line.starts_with("data: ") {
                            let data = &line[6..];
                            if let Ok(json) = serde_json::from_str::<Value>(data) {
                                let event_type = json["type"].as_str().unwrap_or_default();
                                match event_type {
                                    "content_block_start" => {
                                        if let Some(content_block) = json.get("content_block") {
                                            if content_block["type"] == "tool_use" {
                                                let id = content_block["id"].as_str().unwrap_or_default().to_string();
                                                let name = content_block["name"].as_str().unwrap_or_default().to_string();
                                                current_tool = Some((id, name, String::new()));
                                            }
                                        }
                                    }
                                    "content_block_delta" => {
                                        if let Some(delta) = json.get("delta") {
                                            if delta["type"] == "text_delta" {
                                                let text = delta["text"].as_str().unwrap_or_default().to_string();
                                                return Some((Ok(LlmResponse {
                                                    content: vec![ContentBlock::Text { text }],
                                                    model: String::new(),
                                                    usage: None,
                                                    stop_reason: None,
                                                }), (stream, buffer, current_tool)));
                                            } else if delta["type"] == "input_json_delta" {
                                                if let Some((_, _, ref mut input_acc)) = current_tool {
                                                    input_acc.push_str(delta["partial_json"].as_str().unwrap_or_default());
                                                }
                                            }
                                        }
                                    }
                                    "content_block_stop" => {
                                        if let Some((id, name, input_json)) = current_tool.take() {
                                            if let Ok(input_val) = serde_json::from_str::<Value>(&input_json) {
                                                return Some((Ok(LlmResponse {
                                                    content: vec![ContentBlock::ToolUse {
                                                        id,
                                                        name,
                                                        input: input_val,
                                                    }],
                                                    model: String::new(),
                                                    usage: None,
                                                    stop_reason: None,
                                                }), (stream, buffer, None)));
                                            }
                                        }
                                    }
                                    "message_delta" => {
                                        // Update usage/stop reason
                                        if let Some(usage) = json.get("usage") {
                                            let output_tokens = usage["output_tokens"].as_u64().unwrap_or(0) as u32;
                                             return Some((Ok(LlmResponse {
                                                content: vec![],
                                                model: String::new(),
                                                usage: Some(Usage { input_tokens: 0, output_tokens }),
                                                stop_reason: json.get("stop_reason").and_then(|s| s.as_str()).map(|s| s.to_string()),
                                            }), (stream, buffer, current_tool)));
                                        }
                                    }
                                    "message_start" => {
                                         if let Some(message) = json.get("message") {
                                            if let Some(usage) = message.get("usage") {
                                                let input_tokens = usage["input_tokens"].as_u64().unwrap_or(0) as u32;
                                                 return Some((Ok(LlmResponse {
                                                    content: vec![],
                                                    model: String::new(),
                                                    usage: Some(Usage { input_tokens, output_tokens: 0 }),
                                                    stop_reason: None,
                                                }), (stream, buffer, current_tool)));
                                            }
                                         }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        continue;
                    }

                    // Need more data
                    match stream.next().await {
                        Some(Ok(bytes)) => {
                            buffer.extend_from_slice(&bytes);
                        }
                        Some(Err(e)) => return Some((Err(Error::Gateway(e.to_string())), (stream, buffer, current_tool))),
                        None => return None, // End of stream
                    }
                }
            }
        );

        Ok(Box::pin(response_stream))
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        Ok(vec![
            "claude-sonnet-4-5-20250929".to_string(),
            "claude-opus-4-6".to_string(),
            "claude-3-5-sonnet-20240620".to_string(),
            "claude-3-opus-20240229".to_string(),
        ])
    }

    async fn health_check(&self) -> Result<bool> {
        // Send a minimal request to check validity
        let body = json!({
            "model": "claude-3-5-sonnet-20240620",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "ping"}]
        });

        let res = self.client.post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await;

        match res {
            Ok(r) => Ok(r.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}
