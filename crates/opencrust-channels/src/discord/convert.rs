use chrono::Utc;
use opencrust_common::{ChannelId, Message, MessageContent, MessageDirection, SessionId, UserId};
use serenity::all as serenity_model;

pub const DISCORD_MESSAGE_CHAR_LIMIT: usize = 2000;

/// Convert a serenity Discord message into an OpenCrust `Message`.
///
/// Maps the Discord message content to the appropriate `MessageContent` variant.
/// Attachments are checked first — if present, the first attachment determines the
/// content type (image vs file). Otherwise falls back to text content.
pub fn discord_message_to_opencrust(
    msg: &serenity_model::Message,
    channel_id_str: &str,
) -> Message {
    let user_id = UserId::from_string(msg.author.id.to_string());
    let channel_id = ChannelId::from_string(channel_id_str);
    let session_id = SessionId::from_string(format!("discord-{}", msg.channel_id));

    let content = if let Some(attachment) = msg.attachments.first() {
        // Check if the attachment is an image based on content type
        let is_image = attachment
            .content_type
            .as_deref()
            .is_some_and(|ct| ct.starts_with("image/"));

        if is_image {
            MessageContent::Image {
                url: attachment.url.clone(),
                caption: if msg.content.is_empty() {
                    None
                } else {
                    Some(msg.content.clone())
                },
            }
        } else {
            MessageContent::File {
                url: attachment.url.clone(),
                filename: attachment.filename.clone(),
            }
        }
    } else {
        MessageContent::Text(msg.content.clone())
    };

    Message {
        id: msg.id.to_string(),
        session_id,
        channel_id,
        user_id,
        direction: MessageDirection::Incoming,
        content,
        timestamp: Utc::now(),
        metadata: serde_json::json!({
            "discord_channel_id": msg.channel_id.to_string(),
            "discord_guild_id": msg.guild_id.map(|g| g.to_string()),
            "discord_message_id": msg.id.to_string(),
            "author_name": msg.author.name,
            "author_discriminator": msg.author.discriminator,
            "is_bot": msg.author.bot,
        }),
    }
}

/// Convert a reaction add event into an OpenCrust `Message` with `Reaction` content.
pub fn reaction_to_opencrust(reaction: &serenity_model::Reaction, channel_id_str: &str) -> Message {
    let user_id = reaction
        .user_id
        .map(|id| UserId::from_string(id.to_string()))
        .unwrap_or_default();

    let emoji = match &reaction.emoji {
        serenity_model::ReactionType::Unicode(s) => s.clone(),
        serenity_model::ReactionType::Custom { name, id, .. } => {
            name.clone().unwrap_or_else(|| format!("<:emoji:{}>", id))
        }
        _ => "unknown".to_string(),
    };

    Message {
        id: format!(
            "discord-reaction-{}-{}",
            reaction.message_id, reaction.channel_id
        ),
        session_id: SessionId::from_string(format!("discord-{}", reaction.channel_id)),
        channel_id: ChannelId::from_string(channel_id_str),
        user_id,
        direction: MessageDirection::Incoming,
        content: MessageContent::Reaction {
            emoji,
            target_message_id: reaction.message_id.to_string(),
        },
        timestamp: Utc::now(),
        metadata: serde_json::json!({
            "discord_channel_id": reaction.channel_id.to_string(),
            "discord_guild_id": reaction.guild_id.map(|g| g.to_string()),
        }),
    }
}

/// Convert an OpenCrust `MessageContent` into a plain text string suitable
/// for sending as a Discord message.
pub fn opencrust_content_to_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Image { url, caption } => {
            if let Some(cap) = caption {
                format!("{cap}\n{url}")
            } else {
                url.clone()
            }
        }
        MessageContent::Audio { url, duration_secs } => {
            if let Some(dur) = duration_secs {
                format!("🎵 Audio ({dur:.0}s): {url}")
            } else {
                format!("🎵 Audio: {url}")
            }
        }
        MessageContent::Video { url, caption } => {
            if let Some(cap) = caption {
                format!("{cap}\n🎬 {url}")
            } else {
                format!("🎬 {url}")
            }
        }
        MessageContent::File { url, filename } => {
            format!("📎 {filename}: {url}")
        }
        MessageContent::Location {
            latitude,
            longitude,
        } => {
            format!("📍 Location: {latitude}, {longitude}")
        }
        MessageContent::Reaction {
            emoji,
            target_message_id,
        } => {
            format!("{emoji} (on message {target_message_id})")
        }
        MessageContent::System(text) => format!("ℹ️ {text}"),
    }
}

/// Convert generic markdown to Discord-friendly markdown.
///
/// Discord supports most common markdown constructs, so this primarily normalizes
/// line endings and neutralizes mass mentions.
pub fn to_discord_markdown(input: &str) -> String {
    input
        .replace("\r\n", "\n")
        .replace("@everyone", "@\u{200B}everyone")
        .replace("@here", "@\u{200B}here")
}

/// Split text into Discord-safe chunks (<= 2000 chars each).
pub fn split_discord_chunks(input: &str) -> Vec<String> {
    if input.is_empty() {
        return vec!["\u{200B}".to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for ch in input.chars() {
        let ch_len = ch.len_utf8();
        if current_len + ch_len > DISCORD_MESSAGE_CHAR_LIMIT {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
        current.push(ch);
        current_len += ch_len;
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_content_converts_to_string() {
        let content = MessageContent::Text("Hello, world!".into());
        assert_eq!(opencrust_content_to_text(&content), "Hello, world!");
    }

    #[test]
    fn image_with_caption_converts_correctly() {
        let content = MessageContent::Image {
            url: "https://example.com/img.png".into(),
            caption: Some("Look at this!".into()),
        };
        let result = opencrust_content_to_text(&content);
        assert!(result.contains("Look at this!"));
        assert!(result.contains("https://example.com/img.png"));
    }

    #[test]
    fn file_content_includes_filename() {
        let content = MessageContent::File {
            url: "https://example.com/doc.pdf".into(),
            filename: "doc.pdf".into(),
        };
        let result = opencrust_content_to_text(&content);
        assert!(result.contains("doc.pdf"));
    }

    #[test]
    fn reaction_content_includes_emoji() {
        let content = MessageContent::Reaction {
            emoji: "👍".into(),
            target_message_id: "12345".into(),
        };
        let result = opencrust_content_to_text(&content);
        assert!(result.contains("👍"));
        assert!(result.contains("12345"));
    }

    #[test]
    fn system_message_converts() {
        let content = MessageContent::System("Bot restarted".into());
        let result = opencrust_content_to_text(&content);
        assert!(result.contains("Bot restarted"));
    }

    #[test]
    fn markdown_neutralizes_mass_mentions() {
        let out = to_discord_markdown("hello @everyone and @here");
        assert!(!out.contains("@everyone"));
        assert!(!out.contains("@here"));
    }

    #[test]
    fn chunking_splits_over_limit() {
        let long = "a".repeat(DISCORD_MESSAGE_CHAR_LIMIT + 10);
        let chunks = split_discord_chunks(&long);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), DISCORD_MESSAGE_CHAR_LIMIT);
        assert_eq!(chunks[1].len(), 10);
    }
}
