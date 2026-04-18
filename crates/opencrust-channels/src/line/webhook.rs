use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tracing::{info, warn};

use crate::traits::ChannelResponse;

use super::api;
use super::fmt;
use super::{LineChannel, LineFile};

/// Shared state passed to LINE webhook handlers.
pub type LineWebhookState = Arc<Vec<Arc<LineChannel>>>;

/// POST /webhooks/line — receives webhook events from the LINE platform.
///
/// Verifies the `X-Line-Signature` header (HMAC-SHA256 with channel secret),
/// then dispatches each text message event to the configured channel.
pub async fn line_webhook(
    State(channels): State<LineWebhookState>,
    req: Request,
) -> impl IntoResponse {
    let signature = req
        .headers()
        .get("x-line-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body: Bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    // Find the channel whose secret validates this request.
    let channel = match channels
        .iter()
        .find(|ch| ch.verify_signature(&body, &signature))
    {
        Some(ch) => Arc::clone(ch),
        None => {
            warn!("line: no channel matched signature — request rejected");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let body_value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("line: failed to parse webhook body: {e}");
            return StatusCode::BAD_REQUEST;
        }
    };

    let events = match body_value.get("events").and_then(|v| v.as_array()) {
        Some(e) => e,
        None => return StatusCode::OK,
    };

    for event in events {
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if event_type != "message" {
            continue;
        }

        let msg = match event.get("message") {
            Some(m) => m,
            None => continue,
        };

        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Extract text and an optional file depending on message type.
        let (text, file_info) = match msg_type {
            "text" => {
                let t = msg
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                (t, None::<(String, String, Option<String>)>)
            }
            "file" => {
                let msg_id = msg
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let filename = msg
                    .get("fileName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("file")
                    .to_string();
                (String::new(), Some((msg_id, filename, None)))
            }
            "image" => {
                let msg_id = msg
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let filename = format!("image_{msg_id}.jpg");
                (
                    String::new(),
                    Some((msg_id, filename, Some("image/jpeg".to_string()))),
                )
            }
            _ => continue,
        };

        // Skip if there is nothing to process.
        if text.trim().is_empty() && file_info.is_none() {
            continue;
        }

        let reply_token = event
            .get("replyToken")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let source = event.get("source");
        let source_type = source
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        let is_group = source_type == "group" || source_type == "room";

        let user_id = source
            .and_then(|v| v.get("userId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // For groups, use groupId/roomId as the display context; push still targets userId.
        let context_id = if is_group {
            source
                .and_then(|v| v.get("groupId").or_else(|| v.get("roomId")))
                .and_then(|v| v.as_str())
                .unwrap_or(&user_id)
                .to_string()
        } else {
            user_id.clone()
        };

        // Detect @mention: LINE includes mention data in message.mention.mentionees.
        // Each mentionee has a `userId` field; match against the bot's own userId.
        let is_mentioned = if is_group {
            let bot_uid = channel.bot_user_id().unwrap_or("");
            msg.get("mention")
                .and_then(|m| m.get("mentionees"))
                .and_then(|v| v.as_array())
                .map(|mentionees| {
                    mentionees
                        .iter()
                        .any(|m| m.get("userId").and_then(|v| v.as_str()) == Some(bot_uid))
                })
                .unwrap_or(false)
        } else {
            false
        };

        // Embed every group text message for RAG (fire-and-forget, skips bot's own messages).
        let is_bot_message = channel.bot_user_id() == Some(user_id.as_str());
        if is_group
            && !text.is_empty()
            && !is_bot_message
            && let Some(observe_fn) = channel.group_observe_fn().cloned()
        {
            let gid = context_id.clone();
            let uid = user_id.clone();
            let msg = text.clone();
            tokio::spawn(observe_fn(gid, uid, msg));
        }

        // File messages bypass the mention filter when group RAG is enabled,
        // so the bot can prompt for !ingest regardless of mention.
        let has_file = file_info.is_some();
        let rag_enabled = channel.group_observe_fn().is_some();
        if is_group && !(has_file && rag_enabled) && !channel.group_filter()(is_mentioned) {
            continue;
        }

        info!(
            "line: {} from uid={} ctx={}: {} chars",
            if is_group { "group" } else { "dm" },
            user_id,
            context_id,
            text.len(),
        );

        let ch = Arc::clone(&channel);
        tokio::spawn(async move {
            // Download file content if present.
            let line_file = if let Some((msg_id, filename, mime_type)) = file_info {
                match api::download_content(
                    ch.client(),
                    ch.channel_access_token(),
                    &msg_id,
                    ch.data_api_base_url(),
                )
                .await
                {
                    Ok(data) => Some(LineFile {
                        filename,
                        data,
                        mime_type,
                    }),
                    Err(e) => {
                        warn!("line: failed to download file content: {e}");
                        let err_text = "Sorry, I could not download the file.";
                        if !reply_token.is_empty() {
                            let _ = api::reply(
                                ch.client(),
                                ch.channel_access_token(),
                                &reply_token,
                                err_text,
                                ch.api_base_url(),
                            )
                            .await;
                        } else if !user_id.is_empty() {
                            let _ = api::push(
                                ch.client(),
                                ch.channel_access_token(),
                                &user_id,
                                err_text,
                                ch.api_base_url(),
                            )
                            .await;
                        }
                        return;
                    }
                }
            } else {
                None
            };

            let result = ch
                .handle_incoming(&user_id, &context_id, &text, is_group, line_file)
                .await;
            match result {
                Ok(response) => {
                    // LINE audio messages require a publicly accessible HTTPS URL.
                    // Until CDN upload support is added, Voice responses fall back to text.
                    if matches!(response, ChannelResponse::Voice { .. }) {
                        info!(
                            "line: audio reply not yet supported (LINE requires CDN URL); sending text"
                        );
                    }
                    let out = fmt::to_line_text(response.text());
                    // Try reply API first (free), fallback to push.
                    if !reply_token.is_empty() {
                        match api::reply(
                            ch.client(),
                            ch.channel_access_token(),
                            &reply_token,
                            &out,
                            ch.api_base_url(),
                        )
                        .await
                        {
                            Ok(()) => {
                                info!("line: replied via reply API to uid={user_id}");
                                return;
                            }
                            Err(e) => warn!("line: reply failed, falling back to push: {e}"),
                        }
                    }
                    if !user_id.is_empty() {
                        match api::push(
                            ch.client(),
                            ch.channel_access_token(),
                            &user_id,
                            &out,
                            ch.api_base_url(),
                        )
                        .await
                        {
                            Ok(()) => info!("line: sent via push API to uid={user_id}"),
                            Err(e) => warn!("line: push also failed: {e}"),
                        }
                    }
                }
                Err(e) if e == "__blocked__" => {}
                Err(e) => {
                    warn!("line: error processing message: {e}");
                    let err_text = "Sorry, an error occurred.";
                    if !reply_token.is_empty() {
                        let _ = api::reply(
                            ch.client(),
                            ch.channel_access_token(),
                            &reply_token,
                            err_text,
                            ch.api_base_url(),
                        )
                        .await;
                    } else if !user_id.is_empty() {
                        let _ = api::push(
                            ch.client(),
                            ch.channel_access_token(),
                            &user_id,
                            err_text,
                            ch.api_base_url(),
                        )
                        .await;
                    }
                }
            }
        });
    }

    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use axum::{Router, body::Body, routing::post};
    use base64::{Engine, engine::general_purpose};
    use ring::hmac;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::line::{LineChannel, LineOnMessageFn};
    use crate::traits::ChannelResponse;

    fn make_state(secret: &str) -> LineWebhookState {
        let on_msg: LineOnMessageFn = Arc::new(|_uid, _ctx, _text, _is_group, _file, _| {
            Box::pin(async { Ok(ChannelResponse::Text("reply".to_string())) })
        });
        let ch = Arc::new(LineChannel::new(
            "tok".to_string(),
            secret.to_string(),
            on_msg,
        ));
        Arc::new(vec![ch])
    }

    fn make_state_with_base_url(secret: &str, base_url: String) -> LineWebhookState {
        let on_msg: LineOnMessageFn = Arc::new(|_uid, _ctx, _text, _is_group, _file, _| {
            Box::pin(async { Ok(ChannelResponse::Text("reply".to_string())) })
        });
        let ch = Arc::new(
            LineChannel::new("tok".to_string(), secret.to_string(), on_msg)
                .with_api_base_url(base_url),
        );
        Arc::new(vec![ch])
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let tag = hmac::sign(&key, body);
        general_purpose::STANDARD.encode(tag.as_ref())
    }

    fn make_router(state: LineWebhookState) -> Router {
        Router::new()
            .route("/webhooks/line", post(line_webhook))
            .with_state(state)
    }

    fn text_event(reply_token: &str, user_id: &str, text: &str) -> Vec<u8> {
        serde_json::json!({
            "destination": "Uxxxxx",
            "events": [{
                "type": "message",
                "replyToken": reply_token,
                "source": {"type": "user", "userId": user_id},
                "timestamp": 1234567890,
                "message": {"id": "1", "type": "text", "text": text}
            }]
        })
        .to_string()
        .into_bytes()
    }

    #[tokio::test]
    async fn valid_signature_returns_200() {
        let secret = "test-secret";
        let body = text_event("tok123", "Uabc", "hello");
        let sig = sign(secret, &body);

        let resp = make_router(make_state(secret))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_signature_returns_401() {
        let body = text_event("tok123", "Uabc", "hello");

        let resp = make_router(make_state("test-secret"))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", "badsig")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_signature_returns_401() {
        let body = text_event("tok123", "Uabc", "hello");

        let resp = make_router(make_state("test-secret"))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_message_event_returns_200() {
        let secret = "test-secret";
        let body = serde_json::json!({
            "destination": "Uxxxxx",
            "events": [{"type": "follow", "source": {"type": "user", "userId": "Uabc"}, "timestamp": 1234567890}]
        })
        .to_string()
        .into_bytes();
        let sig = sign(secret, &body);

        let resp = make_router(make_state(secret))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn group_message_returns_200() {
        let secret = "test-secret";
        let body = serde_json::json!({
            "destination": "Uxxxxx",
            "events": [{
                "type": "message",
                "replyToken": "tok456",
                "source": {"type": "group", "groupId": "Cabc123", "userId": "Uabc"},
                "timestamp": 1234567890,
                "message": {"id": "2", "type": "text", "text": "hi group"}
            }]
        })
        .to_string()
        .into_bytes();
        let sig = sign(secret, &body);

        let resp = make_router(make_state(secret))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── Reply / push fallback tests ──────────────────────────────────────────

    /// When the reply API succeeds, push must NOT be called.
    #[tokio::test]
    async fn reply_succeeds_push_not_called() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/message/push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&mock_server)
            .await;

        let secret = "test-secret";
        let body = text_event("tok-reply", "Uabc", "hello");
        let sig = sign(secret, &body);

        let resp = make_router(make_state_with_base_url(secret, mock_server.uri()))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        // Allow the spawned task to complete before mock expectations are verified on drop.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    /// When the reply API fails, the handler must fall back to push.
    #[tokio::test]
    async fn reply_fails_falls_back_to_push() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/message/reply"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/message/push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let secret = "test-secret";
        let body = text_event("tok-fail", "Uabc", "hello");
        let sig = sign(secret, &body);

        let resp = make_router(make_state_with_base_url(secret, mock_server.uri()))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    /// When the event has no reply token, push must be called directly (no reply attempt).
    #[tokio::test]
    async fn no_reply_token_uses_push_directly() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/message/push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let secret = "test-secret";
        // Empty reply token → no reply API call expected
        let body = text_event("", "Uabc", "hello");
        let sig = sign(secret, &body);

        let resp = make_router(make_state_with_base_url(secret, mock_server.uri()))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    // ── File / image message tests ───────────────────────────────────────────

    fn file_event(reply_token: &str, user_id: &str, msg_id: &str, filename: &str) -> Vec<u8> {
        serde_json::json!({
            "destination": "Uxxxxx",
            "events": [{
                "type": "message",
                "replyToken": reply_token,
                "source": {"type": "user", "userId": user_id},
                "timestamp": 1234567890,
                "message": {
                    "id": msg_id,
                    "type": "file",
                    "fileName": filename,
                    "fileSize": 1024
                }
            }]
        })
        .to_string()
        .into_bytes()
    }

    fn image_event(reply_token: &str, user_id: &str, msg_id: &str) -> Vec<u8> {
        serde_json::json!({
            "destination": "Uxxxxx",
            "events": [{
                "type": "message",
                "replyToken": reply_token,
                "source": {"type": "user", "userId": user_id},
                "timestamp": 1234567890,
                "message": {
                    "id": msg_id,
                    "type": "image"
                }
            }]
        })
        .to_string()
        .into_bytes()
    }

    fn make_state_with_file_callback(
        secret: &str,
        base_url: String,
    ) -> (LineWebhookState, tokio::sync::mpsc::Receiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let on_msg: LineOnMessageFn =
            Arc::new(move |_uid, _ctx, _text, _is_group, file, _delta_tx| {
                let tx = tx.clone();
                Box::pin(async move {
                    // Echo the filename back so the test can assert on it.
                    let name = file
                        .map(|f| f.filename)
                        .unwrap_or_else(|| "none".to_string());
                    let _ = tx.send(name.clone()).await;
                    Ok(ChannelResponse::Text(name))
                })
            });
        let ch = Arc::new(
            LineChannel::new("tok".to_string(), secret.to_string(), on_msg)
                .with_api_base_url(base_url),
        );
        (Arc::new(vec![ch]), rx)
    }

    /// A "file" message event triggers a download from the data API and passes
    /// the filename to the callback.
    #[tokio::test]
    async fn file_message_downloads_content_and_passes_to_callback() {
        let mock_server = MockServer::start().await;

        // Mock the data API content download.
        Mock::given(method("GET"))
            .and(path("/message/msg-001/content"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"PDF content here".to_vec())
                    .insert_header("content-type", "application/pdf"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        // Mock the reply API (callback echoes filename back).
        Mock::given(method("POST"))
            .and(path("/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let secret = "test-secret";
        let body = file_event("tok-file", "Uabc", "msg-001", "document.pdf");
        let sig = sign(secret, &body);

        let (state, mut rx) = make_state_with_file_callback(secret, mock_server.uri());

        let resp = make_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let received_filename = rx.try_recv().expect("callback should have been called");
        assert_eq!(received_filename, "document.pdf");
    }

    /// An "image" message event triggers a download and generates a synthetic filename.
    #[tokio::test]
    async fn image_message_downloads_content_and_generates_filename() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/message/img-999/content"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![0xFF, 0xD8, 0xFF]) // JPEG magic bytes
                    .insert_header("content-type", "image/jpeg"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let secret = "test-secret";
        let body = image_event("tok-img", "Uabc", "img-999");
        let sig = sign(secret, &body);

        let (state, mut rx) = make_state_with_file_callback(secret, mock_server.uri());

        let resp = make_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let received_filename = rx.try_recv().expect("callback should have been called");
        assert_eq!(received_filename, "image_img-999.jpg");
    }

    /// When the data API returns an error, the handler sends an error reply and
    /// does NOT invoke the callback.
    #[tokio::test]
    async fn file_download_failure_sends_error_reply() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/message/msg-bad/content"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Error reply should still be sent.
        Mock::given(method("POST"))
            .and(path("/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let secret = "test-secret";
        let body = file_event("tok-err", "Uabc", "msg-bad", "broken.pdf");
        let sig = sign(secret, &body);

        let (state, mut rx) = make_state_with_file_callback(secret, mock_server.uri());

        let resp = make_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Callback must NOT have been called.
        assert!(
            rx.try_recv().is_err(),
            "callback should not be called on download failure"
        );
    }

    /// Unsupported message types (e.g. "sticker") are silently ignored.
    #[tokio::test]
    async fn unsupported_message_type_is_ignored() {
        let secret = "test-secret";
        let body = serde_json::json!({
            "destination": "Uxxxxx",
            "events": [{
                "type": "message",
                "replyToken": "tok-sticker",
                "source": {"type": "user", "userId": "Uabc"},
                "timestamp": 1234567890,
                "message": {"id": "s1", "type": "sticker", "packageId": "1", "stickerId": "1"}
            }]
        })
        .to_string()
        .into_bytes();
        let sig = sign(secret, &body);

        let resp = make_router(make_state(secret))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/line")
                    .header("x-line-signature", sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }
}
