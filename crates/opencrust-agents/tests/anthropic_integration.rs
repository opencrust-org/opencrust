use axum::{
    extract::Json,
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
    Router,
};
use futures::stream::{self, Stream, StreamExt};
use opencrust_agents::providers::{
    AnthropicProvider, ChatMessage, ChatRole, ContentBlock, LlmProvider, LlmRequest,
    MessagePart,
};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn test_anthropic_complete() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let app = Router::new().route("/v1/messages", post(mock_complete_handler));
        axum::serve(listener, app).await.unwrap();
    });

    // Wait for server to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    let base_url = format!("http://127.0.0.1:{}/v1/messages", port);
    let provider = AnthropicProvider::new("test-key".to_string()).with_base_url(base_url);

    let request = LlmRequest {
        model: "claude-test".to_string(),
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Text("Hello".to_string()),
        }],
        system: None,
        max_tokens: Some(100),
        temperature: None,
        tools: vec![],
    };

    let response = provider.complete(&request).await.expect("Failed to complete");

    assert_eq!(response.model, "claude-test");
    assert_eq!(response.content.len(), 1);
    if let ContentBlock::Text { text } = &response.content[0] {
        assert_eq!(text, "Hello from mock");
    } else {
        panic!("Expected text response");
    }
}

async fn mock_complete_handler(Json(payload): Json<Value>) -> Json<Value> {
    // Basic validation
    assert_eq!(payload["model"], "claude-test");
    assert_eq!(payload["messages"][0]["role"], "user");
    assert_eq!(payload["messages"][0]["content"], "Hello");

    Json(json!({
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "model": "claude-test",
        "content": [
            {
                "type": "text",
                "text": "Hello from mock"
            }
        ],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5
        }
    }))
}

#[tokio::test]
async fn test_anthropic_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let app = Router::new().route("/v1/messages", post(mock_stream_handler));
        axum::serve(listener, app).await.unwrap();
    });

    // Wait for server
    tokio::time::sleep(Duration::from_millis(100)).await;

    let base_url = format!("http://127.0.0.1:{}/v1/messages", port);
    let provider = AnthropicProvider::new("test-key".to_string()).with_base_url(base_url);

    let request = LlmRequest {
        model: "claude-stream".to_string(),
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Text("Stream me".to_string()),
        }],
        system: None,
        max_tokens: Some(100),
        temperature: None,
        tools: vec![],
    };

    let mut stream = provider.stream_complete(&request).await.expect("Failed to start stream");

    let mut collected_text = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("Stream error");
        for block in chunk.content {
            if let ContentBlock::Text { text } = block {
                collected_text.push_str(&text);
            }
        }
    }

    assert_eq!(collected_text, "Streamed response");
}

async fn mock_stream_handler(Json(payload): Json<Value>) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    assert_eq!(payload["stream"], true);

    let events = vec![
        Event::default().event("message_start").data(json!({
            "type": "message_start",
            "message": {
                "id": "msg_stream",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-stream",
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 5, "output_tokens": 1}
            }
        }).to_string()),
        Event::default().event("content_block_start").data(json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }).to_string()),
        Event::default().event("content_block_delta").data(json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Streamed "}
        }).to_string()),
        Event::default().event("content_block_delta").data(json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "response"}
        }).to_string()),
        Event::default().event("content_block_stop").data(json!({
            "type": "content_block_stop",
            "index": 0
        }).to_string()),
        Event::default().event("message_delta").data(json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 15}
        }).to_string()),
        Event::default().event("message_stop").data(json!({
            "type": "message_stop"
        }).to_string()),
    ];

    let stream = stream::iter(events.into_iter().map(Ok));
    Sse::new(stream).keep_alive(KeepAlive::default())
}
