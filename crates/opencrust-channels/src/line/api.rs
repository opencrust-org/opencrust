use reqwest::Client;

pub const LINE_API_BASE: &str = "https://api.line.me/v2/bot";
/// Separate hostname used for downloading message content (images, files, etc.).
pub const LINE_DATA_API_BASE: &str = "https://api-data.line.me/v2/bot";

/// Download the binary content of a LINE message (image, file, audio, video).
///
/// Uses the data API: `GET {data_base_url}/message/{message_id}/content`.
/// Returns the raw bytes.
pub async fn download_content(
    client: &Client,
    channel_access_token: &str,
    message_id: &str,
    data_base_url: &str,
) -> Result<Vec<u8>, String> {
    let resp = client
        .get(format!("{data_base_url}/message/{message_id}/content"))
        .bearer_auth(channel_access_token)
        .send()
        .await
        .map_err(|e| format!("line download_content request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("line download_content error {status}: {body}"));
    }

    if let Some(len) = resp.content_length()
        && len > crate::MAX_DOWNLOAD_BYTES as u64
    {
        return Err(format!(
            "line file too large: {len} bytes exceeds {} byte limit",
            crate::MAX_DOWNLOAD_BYTES
        ));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("line download_content read failed: {e}"))?;

    if bytes.len() > crate::MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "line file too large: {} bytes exceeds {} byte limit",
            bytes.len(),
            crate::MAX_DOWNLOAD_BYTES
        ));
    }

    Ok(bytes.to_vec())
}

/// Send a reply using a reply token (free, expires in 30 seconds, one use).
pub async fn reply(
    client: &Client,
    channel_access_token: &str,
    reply_token: &str,
    text: &str,
    base_url: &str,
) -> Result<(), String> {
    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": [{"type": "text", "text": text}]
    });

    let resp = client
        .post(format!("{base_url}/message/reply"))
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

/// Bot profile returned by `GET /v2/bot/info`.
pub struct BotInfo {
    /// Display name shown to users (e.g. `"MyBot"`).
    pub display_name: String,
    /// LINE user ID of the bot (e.g. `"Uxxxxxxxxx"`), used for mention detection.
    pub user_id: String,
}

/// Fetch the bot's profile from the LINE Bot API.
///
/// Calls `GET {base_url}/info` and returns `displayName` and `userId`.
pub async fn get_bot_info(
    client: &Client,
    channel_access_token: &str,
    base_url: &str,
) -> Result<BotInfo, String> {
    let resp = client
        .get(format!("{base_url}/info"))
        .bearer_auth(channel_access_token)
        .send()
        .await
        .map_err(|e| format!("line get_bot_info request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("line get_bot_info error {status}: {body}"));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("line get_bot_info parse failed: {e}"))?;

    let display_name = json
        .get("displayName")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "line get_bot_info: displayName missing".to_string())?
        .to_string();

    let user_id = json
        .get("userId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "line get_bot_info: userId missing".to_string())?
        .to_string();

    Ok(BotInfo {
        display_name,
        user_id,
    })
}

/// Send a push message to a user ID (paid tier, works at any time).
pub async fn push(
    client: &Client,
    channel_access_token: &str,
    user_id: &str,
    text: &str,
    base_url: &str,
) -> Result<(), String> {
    let body = serde_json::json!({
        "to": user_id,
        "messages": [{"type": "text", "text": text}]
    });

    let resp = client
        .post(format!("{base_url}/message/push"))
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

#[cfg(test)]
mod tests {
    #[test]
    fn download_size_limit_constant_is_10_mib() {
        assert_eq!(crate::MAX_DOWNLOAD_BYTES, 10 * 1024 * 1024);
    }
}
