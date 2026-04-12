/// Result of inspecting a raw MQTT payload byte slice.
#[derive(Debug, PartialEq)]
pub enum DetectedMessage {
    /// Plain text payload — one session per channel (Mode A).
    Simple { text: String },
    /// JSON payload with `user_id` + `text` — one session per user (Mode B).
    Multi(MultiUserPayload),
}

/// Structured payload for multi-user mode.
#[derive(Debug, PartialEq)]
pub struct MultiUserPayload {
    /// Device / user identifier extracted from the JSON `user_id` field.
    pub user_id: String,
    /// Message text extracted from the JSON `text` field.
    pub text: String,
    /// Optional session override from the JSON `session_id` field.
    /// When present, the channel uses this value directly instead of deriving
    /// `mqtt-{channel_name}-{user_id}`.
    pub session_id: Option<String>,
}

/// Inspect raw MQTT payload bytes and decide whether this is a Mode A (simple)
/// or Mode B (multi-user) message.
///
/// Decision rules:
/// 1. Not valid UTF-8 → Simple (safe fallback, log binary size).
/// 2. Not valid JSON → Simple.
/// 3. JSON is not an object → Simple.
/// 4. Object missing `"text"` (string) or `"user_id"` (string) → Simple.
/// 5. Object has both `"text"` and `"user_id"` → Multi.
pub fn detect(payload: &[u8]) -> DetectedMessage {
    let Ok(text) = std::str::from_utf8(payload) else {
        return DetectedMessage::Simple {
            text: format!("<binary payload, {} bytes>", payload.len()),
        };
    };

    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return DetectedMessage::Simple {
            text: text.to_string(),
        };
    };

    let Some(obj) = value.as_object() else {
        return DetectedMessage::Simple {
            text: text.to_string(),
        };
    };

    let Some(msg_text) = obj.get("text").and_then(|v| v.as_str()) else {
        return DetectedMessage::Simple {
            text: text.to_string(),
        };
    };

    let Some(user_id) = obj.get("user_id").and_then(|v| v.as_str()) else {
        return DetectedMessage::Simple {
            text: text.to_string(),
        };
    };

    DetectedMessage::Multi(MultiUserPayload {
        user_id: user_id.to_string(),
        text: msg_text.to_string(),
        session_id: obj
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_simple() {
        let result = detect(b"hello world");
        assert_eq!(
            result,
            DetectedMessage::Simple {
                text: "hello world".into()
            }
        );
    }

    #[test]
    fn invalid_utf8_is_simple() {
        let result = detect(b"\xFF\xFE");
        match result {
            DetectedMessage::Simple { text } => assert!(text.contains("binary payload")),
            _ => panic!("expected Simple"),
        }
    }

    #[test]
    fn json_array_is_simple() {
        let result = detect(b"[1,2,3]");
        assert_eq!(
            result,
            DetectedMessage::Simple {
                text: "[1,2,3]".into()
            }
        );
    }

    #[test]
    fn json_missing_user_id_is_simple() {
        let result = detect(br#"{"text":"hello"}"#);
        match result {
            DetectedMessage::Simple { .. } => {}
            _ => panic!("expected Simple when user_id is missing"),
        }
    }

    #[test]
    fn json_missing_text_is_simple() {
        let result = detect(br#"{"user_id":"s01"}"#);
        match result {
            DetectedMessage::Simple { .. } => {}
            _ => panic!("expected Simple when text is missing"),
        }
    }

    #[test]
    fn json_with_text_and_user_id_is_multi() {
        let result = detect(br#"{"user_id":"s01","text":"hi"}"#);
        assert_eq!(
            result,
            DetectedMessage::Multi(MultiUserPayload {
                user_id: "s01".into(),
                text: "hi".into(),
                session_id: None,
            })
        );
    }

    #[test]
    fn json_with_session_id_override() {
        let result = detect(br#"{"user_id":"s01","text":"hi","session_id":"custom-42"}"#);
        assert_eq!(
            result,
            DetectedMessage::Multi(MultiUserPayload {
                user_id: "s01".into(),
                text: "hi".into(),
                session_id: Some("custom-42".into()),
            })
        );
    }

    #[test]
    fn non_string_text_field_is_simple() {
        let result = detect(br#"{"user_id":"s01","text":42}"#);
        match result {
            DetectedMessage::Simple { .. } => {}
            _ => panic!("expected Simple when text is not a string"),
        }
    }
}
