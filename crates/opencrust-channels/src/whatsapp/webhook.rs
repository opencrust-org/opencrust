use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use tracing::{info, warn};

use super::api;
use super::{WhatsAppChannel, WhatsAppFile};

/// Shared state passed to WhatsApp webhook handlers.
pub type WhatsAppState = Arc<Vec<Arc<WhatsAppChannel>>>;

#[derive(Deserialize)]
pub struct VerifyParams {
    #[serde(rename = "hub.mode")]
    pub mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub challenge: Option<String>,
}

/// GET handler for WhatsApp webhook verification.
pub async fn whatsapp_verify(
    State(channels): State<WhatsAppState>,
    Query(params): Query<VerifyParams>,
) -> impl IntoResponse {
    let mode = params.mode.as_deref().unwrap_or("");
    let token = params.verify_token.as_deref().unwrap_or("");
    let challenge = params.challenge.as_deref().unwrap_or("");

    if mode != "subscribe" {
        return (StatusCode::FORBIDDEN, "invalid mode".to_string());
    }

    // Check token against any configured channel
    let valid = channels.iter().any(|ch| ch.verify_token() == token);

    if valid {
        info!("whatsapp: webhook verified");
        (StatusCode::OK, challenge.to_string())
    } else {
        warn!("whatsapp: webhook verification failed — token mismatch");
        (StatusCode::FORBIDDEN, "invalid verify token".to_string())
    }
}

/// POST handler for incoming WhatsApp messages.
pub async fn whatsapp_webhook(
    State(channels): State<WhatsAppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // WhatsApp sends: { "entry": [{ "changes": [{ "value": { "messages": [...] } }] }] }
    let entries = match body.get("entry").and_then(|v| v.as_array()) {
        Some(e) => e,
        None => return StatusCode::OK,
    };

    for entry in entries {
        let changes = match entry.get("changes").and_then(|v| v.as_array()) {
            Some(c) => c,
            None => continue,
        };

        for change in changes {
            let value = match change.get("value") {
                Some(v) => v,
                None => continue,
            };

            // Get the phone_number_id this message was sent to
            let metadata_phone_id = value
                .get("metadata")
                .and_then(|m| m.get("phone_number_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let messages = match value.get("messages").and_then(|v| v.as_array()) {
                Some(m) => m,
                None => continue,
            };

            // Get contacts for display names
            let contacts = value.get("contacts").and_then(|v| v.as_array());

            for msg in messages {
                let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

                // Only handle text and document messages.
                if msg_type != "text" && msg_type != "document" {
                    continue;
                }

                let from = msg
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let message_id = msg
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Extract text body (empty string for document-only messages).
                let text = msg
                    .get("text")
                    .and_then(|v| v.get("body"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Extract document metadata when present.
                let doc_info = if msg_type == "document" {
                    msg.get("document").map(|d| {
                        let media_id = d
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let filename = d
                            .get("filename")
                            .and_then(|v| v.as_str())
                            .unwrap_or("document")
                            .to_string();
                        let mime_type = d
                            .get("mime_type")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        (media_id, filename, mime_type)
                    })
                } else {
                    None
                };

                // Skip if there is neither text nor a document.
                if text.trim().is_empty() && doc_info.is_none() {
                    continue;
                }

                // Try to get display name from contacts
                let user_name = contacts
                    .and_then(|c| {
                        c.iter().find_map(|contact| {
                            let wa_id = contact.get("wa_id").and_then(|v| v.as_str())?;
                            if wa_id == from {
                                contact
                                    .get("profile")
                                    .and_then(|p| p.get("name"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap_or_else(|| from.clone());

                info!(
                    "whatsapp: message from {} ({}): {} chars",
                    user_name,
                    from,
                    text.len()
                );

                // Find the matching channel by phone_number_id
                let channel = channels
                    .iter()
                    .find(|ch| ch.phone_number_id() == metadata_phone_id)
                    .or_else(|| channels.first());

                let Some(channel) = channel else {
                    warn!(
                        "whatsapp: no channel configured for phone_number_id {metadata_phone_id}"
                    );
                    continue;
                };

                // Mark as read
                let client = channel.client();
                let token = channel.access_token();
                let phone_id = channel.phone_number_id().to_string();

                let read_client = client.clone();
                let read_token = token.to_string();
                let read_phone_id = phone_id.clone();
                let read_msg_id = message_id.clone();
                tokio::spawn(async move {
                    let _ =
                        api::mark_as_read(&read_client, &read_token, &read_phone_id, &read_msg_id)
                            .await;
                });

                // Process message (download document first if present)
                let channel = Arc::clone(channel);
                let from_clone = from.clone();
                tokio::spawn(async move {
                    // Download document bytes before invoking the callback.
                    let whatsapp_file = if let Some((media_id, filename, mime_type)) = doc_info {
                        if media_id.is_empty() {
                            warn!("whatsapp: document has no media id, skipping download");
                            None
                        } else {
                            match api::download_media(
                                channel.client(),
                                channel.access_token(),
                                &media_id,
                            )
                            .await
                            {
                                Ok(data) => Some(WhatsAppFile {
                                    filename,
                                    data,
                                    mime_type,
                                }),
                                Err(e) => {
                                    warn!("whatsapp: failed to download document: {e}");
                                    let _ = api::send_text_message(
                                        channel.client(),
                                        channel.access_token(),
                                        channel.phone_number_id(),
                                        &from_clone,
                                        &format!("Failed to download file: {e}"),
                                    )
                                    .await;
                                    return;
                                }
                            }
                        }
                    } else {
                        None
                    };

                    match channel
                        .handle_incoming(&from_clone, &user_name, &text, whatsapp_file)
                        .await
                    {
                        Ok(response) => {
                            if let Err(e) = api::send_text_message(
                                channel.client(),
                                channel.access_token(),
                                channel.phone_number_id(),
                                &from_clone,
                                response.text(),
                            )
                            .await
                            {
                                warn!("whatsapp: failed to send reply: {e}");
                            }
                        }
                        Err(e) if e == "__blocked__" => {
                            // Silently drop — unauthorized user
                        }
                        Err(e) => {
                            warn!("whatsapp: error processing message: {e}");
                            let _ = api::send_text_message(
                                channel.client(),
                                channel.access_token(),
                                channel.phone_number_id(),
                                &from_clone,
                                "Sorry, an error occurred processing your message.",
                            )
                            .await;
                        }
                    }
                });
            }
        }
    }

    StatusCode::OK
}
