use reqwest::Client;
use serde::Serialize;
use tracing::warn;

const GRAPH_API_BASE: &str = "https://graph.facebook.com/v21.0";

#[derive(Serialize)]
struct WhatsAppTextBody {
    body: String,
}

#[derive(Serialize)]
struct WhatsAppMessage {
    messaging_product: String,
    recipient_type: String,
    to: String,
    #[serde(rename = "type")]
    msg_type: String,
    text: WhatsAppTextBody,
}

#[derive(Serialize)]
struct WhatsAppReadReceipt {
    messaging_product: String,
    status: String,
    message_id: String,
}

/// Send a text message to a WhatsApp user.
pub async fn send_text_message(
    client: &Client,
    token: &str,
    phone_number_id: &str,
    to: &str,
    text: &str,
) -> Result<(), String> {
    let msg = WhatsAppMessage {
        messaging_product: "whatsapp".to_string(),
        recipient_type: "individual".to_string(),
        to: to.to_string(),
        msg_type: "text".to_string(),
        text: WhatsAppTextBody {
            body: text.to_string(),
        },
    };

    let resp = client
        .post(format!("{GRAPH_API_BASE}/{phone_number_id}/messages"))
        .bearer_auth(token)
        .json(&msg)
        .send()
        .await
        .map_err(|e| format!("WhatsApp send_text_message failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        warn!("WhatsApp send_text_message error {status}: {body}");
        return Err(format!("WhatsApp API error {status}: {body}"));
    }

    Ok(())
}

/// Mark a message as read.
pub async fn mark_as_read(
    client: &Client,
    token: &str,
    phone_number_id: &str,
    message_id: &str,
) -> Result<(), String> {
    let receipt = WhatsAppReadReceipt {
        messaging_product: "whatsapp".to_string(),
        status: "read".to_string(),
        message_id: message_id.to_string(),
    };

    let resp = client
        .post(format!("{GRAPH_API_BASE}/{phone_number_id}/messages"))
        .bearer_auth(token)
        .json(&receipt)
        .send()
        .await
        .map_err(|e| format!("WhatsApp mark_as_read failed: {e}"))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        warn!("WhatsApp mark_as_read error: {body}");
    }

    Ok(())
}
