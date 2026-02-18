use reqwest::Client;
use serde::Deserialize;
use tracing::warn;

const SLACK_API_BASE: &str = "https://slack.com/api";

#[derive(Deserialize)]
struct SlackApiResponse {
    ok: bool,
    error: Option<String>,
    url: Option<String>,
    ts: Option<String>,
}

/// Call `apps.connections.open` to get a WebSocket URL for Socket Mode.
pub async fn open_connection(client: &Client, app_token: &str) -> Result<String, String> {
    let resp = client
        .post(format!("{SLACK_API_BASE}/apps.connections.open"))
        .bearer_auth(app_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
        .map_err(|e| format!("apps.connections.open request failed: {e}"))?;

    let body: SlackApiResponse = resp
        .json()
        .await
        .map_err(|e| format!("apps.connections.open parse failed: {e}"))?;

    if !body.ok {
        let err = body.error.unwrap_or_else(|| "unknown".to_string());
        return Err(format!("apps.connections.open error: {err}"));
    }

    body.url
        .ok_or_else(|| "apps.connections.open: no url in response".to_string())
}

/// Post a new message to a Slack channel. Returns the message `ts` (timestamp ID).
pub async fn post_message(
    client: &Client,
    bot_token: &str,
    channel: &str,
    text: &str,
) -> Result<String, String> {
    let resp = client
        .post(format!("{SLACK_API_BASE}/chat.postMessage"))
        .bearer_auth(bot_token)
        .json(&serde_json::json!({
            "channel": channel,
            "text": text,
        }))
        .send()
        .await
        .map_err(|e| format!("chat.postMessage request failed: {e}"))?;

    let body: SlackApiResponse = resp
        .json()
        .await
        .map_err(|e| format!("chat.postMessage parse failed: {e}"))?;

    if !body.ok {
        let err = body.error.unwrap_or_else(|| "unknown".to_string());
        return Err(format!("chat.postMessage error: {err}"));
    }

    body.ts
        .ok_or_else(|| "chat.postMessage: no ts in response".to_string())
}

/// Update an existing Slack message (used for streaming edits).
pub async fn update_message(
    client: &Client,
    bot_token: &str,
    channel: &str,
    ts: &str,
    text: &str,
) -> Result<(), String> {
    let resp = client
        .post(format!("{SLACK_API_BASE}/chat.update"))
        .bearer_auth(bot_token)
        .json(&serde_json::json!({
            "channel": channel,
            "ts": ts,
            "text": text,
        }))
        .send()
        .await
        .map_err(|e| format!("chat.update request failed: {e}"))?;

    let body: SlackApiResponse = resp
        .json()
        .await
        .map_err(|e| format!("chat.update parse failed: {e}"))?;

    if !body.ok {
        let err = body.error.unwrap_or_else(|| "unknown".to_string());
        warn!("chat.update error: {err}");
    }

    Ok(())
}
