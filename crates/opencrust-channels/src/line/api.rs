use reqwest::Client;

const LINE_API_BASE: &str = "https://api.line.me/v2/bot";

/// Send a reply using a reply token (free, expires in 30 seconds, one use).
pub async fn reply(
    client: &Client,
    channel_access_token: &str,
    reply_token: &str,
    text: &str,
) -> Result<(), String> {
    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": [{"type": "text", "text": text}]
    });

    let resp = client
        .post(format!("{LINE_API_BASE}/message/reply"))
        .bearer_auth(channel_access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("line reply request failed: {e}"))?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        Err(format!("line reply failed ({status}): {body_text}"))
    }
}

/// Send a push message to a user ID (paid tier, works at any time).
pub async fn push(
    client: &Client,
    channel_access_token: &str,
    user_id: &str,
    text: &str,
) -> Result<(), String> {
    let body = serde_json::json!({
        "to": user_id,
        "messages": [{"type": "text", "text": text}]
    });

    let resp = client
        .post(format!("{LINE_API_BASE}/message/push"))
        .bearer_auth(channel_access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("line push request failed: {e}"))?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        Err(format!("line push failed ({status}): {body_text}"))
    }
}
