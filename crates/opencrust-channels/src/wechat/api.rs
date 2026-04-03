use reqwest::Client;
use serde::Deserialize;

pub const WECHAT_API_BASE: &str = "https://api.weixin.qq.com/cgi-bin";

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    errcode: Option<i64>,
    errmsg: Option<String>,
}

/// Fetch a fresh access token using appid + secret.
///
/// WeChat access tokens expire after 7200 seconds. Callers are responsible
/// for caching if they need to minimise API calls.
pub async fn get_access_token(
    client: &Client,
    appid: &str,
    secret: &str,
    base_url: &str,
) -> Result<String, String> {
    let resp = client
        .get(format!("{base_url}/token"))
        .query(&[
            ("grant_type", "client_credential"),
            ("appid", appid),
            ("secret", secret),
        ])
        .send()
        .await
        .map_err(|e| format!("wechat token request failed: {e}"))?;

    let status = resp.status();
    let body: AccessTokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse wechat token response: {e}"))?;

    if let Some(code) = body.errcode
        && code != 0
    {
        return Err(format!(
            "wechat token error {code}: {}",
            body.errmsg.unwrap_or_default()
        ));
    }

    body.access_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("wechat token response missing access_token (HTTP {status})"))
}

/// Build a Customer Service API request body and send it.
async fn send_custom(
    client: &Client,
    access_token: &str,
    base_url: &str,
    body: serde_json::Value,
) -> Result<(), String> {
    let resp = client
        .post(format!("{base_url}/message/custom/send"))
        .query(&[("access_token", access_token)])
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("wechat custom send request failed: {e}"))?;

    if resp.status().is_success() {
        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse wechat custom send response: {e}"))?;
        let errcode = result.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0);
        if errcode != 0 {
            let errmsg = result
                .get("errmsg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(format!("wechat custom send api error {errcode}: {errmsg}"));
        }
        Ok(())
    } else {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        Err(format!("wechat custom send failed ({status}): {body_text}"))
    }
}

/// Send a text message via the WeChat Customer Service (kefu) API.
///
/// Requires a valid `access_token`. This is the async push path — use it
/// when a synchronous reply is not possible (e.g. outside the 5-second window).
pub async fn push(
    client: &Client,
    access_token: &str,
    openid: &str,
    text: &str,
    base_url: &str,
) -> Result<(), String> {
    let body = serde_json::json!({
        "touser": openid,
        "msgtype": "text",
        "text": { "content": text }
    });
    send_custom(client, access_token, base_url, body).await
}

/// Send an image message via the WeChat Customer Service API.
///
/// `media_id` must be a pre-uploaded temporary or permanent media ID.
pub async fn push_image(
    client: &Client,
    access_token: &str,
    openid: &str,
    media_id: &str,
    base_url: &str,
) -> Result<(), String> {
    let body = serde_json::json!({
        "touser": openid,
        "msgtype": "image",
        "image": { "media_id": media_id }
    });
    send_custom(client, access_token, base_url, body).await
}

/// Send a voice message via the WeChat Customer Service API.
///
/// `media_id` must be a pre-uploaded temporary media ID (AMR or MP3, ≤ 2 MB, ≤ 60 s).
pub async fn push_voice(
    client: &Client,
    access_token: &str,
    openid: &str,
    media_id: &str,
    base_url: &str,
) -> Result<(), String> {
    let body = serde_json::json!({
        "touser": openid,
        "msgtype": "voice",
        "voice": { "media_id": media_id }
    });
    send_custom(client, access_token, base_url, body).await
}

/// Send a video message via the WeChat Customer Service API.
///
/// Both `media_id` (the video) and `thumb_media_id` (the thumbnail) must be
/// pre-uploaded temporary media IDs.
#[allow(clippy::too_many_arguments)]
pub async fn push_video(
    client: &Client,
    access_token: &str,
    openid: &str,
    media_id: &str,
    thumb_media_id: &str,
    title: Option<&str>,
    description: Option<&str>,
    base_url: &str,
) -> Result<(), String> {
    let body = serde_json::json!({
        "touser": openid,
        "msgtype": "video",
        "video": {
            "media_id": media_id,
            "thumb_media_id": thumb_media_id,
            "title": title.unwrap_or(""),
            "description": description.unwrap_or("")
        }
    });
    send_custom(client, access_token, base_url, body).await
}

/// Build the URL for downloading a temporary media file.
///
/// Caller must append a valid `access_token` to the returned URL before use,
/// or call `get_access_token` separately.
pub fn media_download_url(base_url: &str, media_id: &str, access_token: &str) -> String {
    format!("{base_url}/media/get?access_token={access_token}&media_id={media_id}")
}
