use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tracing::{info, warn};

use super::LineChannel;
use super::api;
use super::fmt;

/// Shared state passed to LINE webhook handlers.
pub type LineWebhookState = Arc<Vec<Arc<LineChannel>>>;

/// POST /line/webhook — receives webhook events from the LINE platform.
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

    // The destination field contains the bot's own user ID.
    let destination = body_value
        .get("destination")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    for event in events {
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if event_type != "message" {
            continue;
        }

        let msg = match event.get("message") {
            Some(m) => m,
            None => continue,
        };
        if msg.get("type").and_then(|v| v.as_str()).unwrap_or("") != "text" {
            continue;
        }

        let text = match msg.get("text").and_then(|v| v.as_str()) {
            Some(t) if !t.trim().is_empty() => t.to_string(),
            _ => continue,
        };

        let reply_token = event
            .get("replyToken")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let source = event.get("source").cloned().unwrap_or_default();
        let source_type = source
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        let is_group = source_type == "group" || source_type == "room";

        let user_id = source
            .get("userId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // For groups, use groupId/roomId as the display context; push still targets userId.
        let context_id = if is_group {
            source
                .get("groupId")
                .or_else(|| source.get("roomId"))
                .and_then(|v| v.as_str())
                .unwrap_or(&user_id)
                .to_string()
        } else {
            user_id.clone()
        };

        // Detect if the bot itself was mentioned in this message.
        let mut is_mentioned = false;
        if is_group
            && !destination.is_empty()
            && let Some(mentionees) = msg
                .get("mention")
                .and_then(|m| m.get("mentionees"))
                .and_then(|v| v.as_array())
        {
            is_mentioned = mentionees
                .iter()
                .any(|m| m.get("userId").and_then(|v| v.as_str()) == Some(destination));
        }

        // Apply group filter — now using reliable mention detection.
        if is_group && !channel.group_filter()(is_mentioned) {
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
            let result = ch
                .handle_incoming(&user_id, &context_id, &text, is_group)
                .await;
            match result {
                Ok(response) => {
                    let out = fmt::to_line_text(&response);
                    // Try reply API first (free), fallback to push.
                    if !reply_token.is_empty() {
                        match api::reply(ch.client(), ch.channel_access_token(), &reply_token, &out)
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
                        match api::push(ch.client(), ch.channel_access_token(), &user_id, &out)
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
                        )
                        .await;
                    } else if !user_id.is_empty() {
                        let _ =
                            api::push(ch.client(), ch.channel_access_token(), &user_id, err_text)
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

    use crate::line::{LineChannel, LineOnMessageFn};

    fn make_state(secret: &str) -> LineWebhookState {
        let on_msg: LineOnMessageFn =
            Arc::new(|_uid, _ctx, _text, _is_group, _| Box::pin(async { Ok("reply".to_string()) }));
        let ch = Arc::new(LineChannel::new(
            "tok".to_string(),
            secret.to_string(),
            on_msg,
        ));
        Arc::new(vec![ch])
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let tag = hmac::sign(&key, body);
        general_purpose::STANDARD.encode(tag.as_ref())
    }

    fn make_router(state: LineWebhookState) -> Router {
        Router::new()
            .route("/line/webhook", post(line_webhook))
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
                    .uri("/line/webhook")
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
                    .uri("/line/webhook")
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
                    .uri("/line/webhook")
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
                    .uri("/line/webhook")
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
                    .uri("/line/webhook")
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
    async fn group_mention_message_returns_200() {
        let secret = "test-secret";
        let body = serde_json::json!({
            "destination": "Ubot123",
            "events": [{
                "type": "message",
                "replyToken": "tok789",
                "source": {"type": "group", "groupId": "Cgroup1", "userId": "Uuser1"},
                "timestamp": 1234567890,
                "message": {
                    "id": "3",
                    "type": "text",
                    "text": "@Bot hi",
                    "mention": {
                        "mentionees": [
                            {"index": 0, "length": 4, "type": "user", "userId": "Ubot123"}
                        ]
                    }
                }
            }]
        })
        .to_string()
        .into_bytes();
        let sig = sign(secret, &body);

        let resp = make_router(make_state(secret))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/line/webhook")
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
