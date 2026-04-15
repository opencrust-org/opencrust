use reqwest::Client;
use serde::Deserialize;
use tracing::warn;

const SLACK_API_BASE: &str = "https://slack.com/api";

/// Maximum file size accepted for document ingestion (10 MiB).
pub const SLACK_MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Deserialize)]
struct SlackApiResponse {
    ok: bool,
    error: Option<String>,
    url: Option<String>,
    ts: Option<String>,
}

/// Call `auth.test` to resolve the bot's own user ID and display name.
///
/// Returns `(user_id, bot_name)` on success. Used during `connect()` to
/// auto-populate `bot_user_id` so `@mention` detection works without manual
/// config.  Also handles the case where the bot token is rotated and the
/// user ID changes.
pub async fn auth_test(client: &Client, bot_token: &str) -> Result<(String, String), String> {
    #[derive(Deserialize)]
    struct AuthTestResp {
        ok: bool,
        error: Option<String>,
        user_id: Option<String>,
        user: Option<String>,
    }

    let resp = client
        .post(format!("{SLACK_API_BASE}/auth.test"))
        .bearer_auth(bot_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
        .map_err(|e| format!("auth.test request failed: {e}"))?;

    let body: AuthTestResp = resp
        .json()
        .await
        .map_err(|e| format!("auth.test parse failed: {e}"))?;

    if !body.ok {
        let err = body.error.unwrap_or_else(|| "unknown".to_string());
        return Err(format!("auth.test error: {err}"));
    }

    let user_id = body
        .user_id
        .ok_or_else(|| "auth.test: no user_id in response".to_string())?;
    let name = body.user.unwrap_or_else(|| user_id.clone());
    Ok((user_id, name))
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

/// Download a private Slack file using the bot token for authorization.
///
/// Slack files require `Authorization: Bearer <bot_token>` — they cannot be
/// fetched without credentials. Returns the raw file bytes.
/// Rejects files larger than [`SLACK_MAX_FILE_BYTES`] based on `Content-Length`.
pub async fn download_file(client: &Client, bot_token: &str, url: &str) -> Result<Vec<u8>, String> {
    let resp = client
        .get(url)
        .bearer_auth(bot_token)
        .send()
        .await
        .map_err(|e| format!("slack file download failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "slack file download error {}: {url}",
            resp.status()
        ));
    }

    if let Some(len) = resp.content_length()
        && len > SLACK_MAX_FILE_BYTES as u64
    {
        return Err(format!(
            "file too large: {len} bytes exceeds {SLACK_MAX_FILE_BYTES} byte limit"
        ));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("slack file read failed: {e}"))?;

    if bytes.len() > SLACK_MAX_FILE_BYTES {
        return Err(format!(
            "file too large: {} bytes exceeds {SLACK_MAX_FILE_BYTES} byte limit",
            bytes.len()
        ));
    }

    Ok(bytes.to_vec())
}

/// Look up a Slack user's display name via `users.info`.
///
/// Returns the display name if set, otherwise the real name, falling back to
/// the user ID if the call fails.
pub async fn get_user_name(client: &Client, bot_token: &str, user_id: &str) -> String {
    #[derive(Deserialize)]
    struct Profile {
        display_name: Option<String>,
        real_name: Option<String>,
    }

    #[derive(Deserialize)]
    struct User {
        profile: Option<Profile>,
        name: Option<String>,
    }

    #[derive(Deserialize)]
    struct Resp {
        ok: bool,
        user: Option<User>,
    }

    let resp = client
        .get(format!("{SLACK_API_BASE}/users.info"))
        .bearer_auth(bot_token)
        .query(&[("user", user_id)])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            warn!("users.info request failed: {e}");
            return user_id.to_string();
        }
    };

    let body: Resp = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("users.info parse failed: {e}");
            return user_id.to_string();
        }
    };

    if !body.ok {
        return user_id.to_string();
    }

    body.user
        .and_then(|u| {
            let profile = u.profile?;
            let display = profile.display_name.filter(|s| !s.is_empty());
            let real = profile.real_name.filter(|s| !s.is_empty());
            display.or(real).or(u.name)
        })
        .unwrap_or_else(|| user_id.to_string())
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
