/// Maximum characters allowed in a single WeChat text message reply.
const WECHAT_TEXT_MAX: usize = 2048;

/// Prepare text for a WeChat text message. Truncates at the 2048-char limit.
pub fn to_wechat_text(text: &str) -> String {
    if text.chars().count() <= WECHAT_TEXT_MAX {
        text.to_string()
    } else {
        text.chars().take(WECHAT_TEXT_MAX).collect()
    }
}

/// Build a synchronous XML reply for the WeChat passive-reply interface.
///
/// WeChat expects a response within 5 seconds. `to_user` is the subscriber's
/// OpenID (`FromUserName` from the incoming event), `from_user` is the
/// Official Account ID (`ToUserName` from the incoming event).
pub fn build_reply_xml(to_user: &str, from_user: &str, text: &str) -> String {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    format!(
        "<xml>\
            <ToUserName><![CDATA[{to_user}]]></ToUserName>\
            <FromUserName><![CDATA[{from_user}]]></FromUserName>\
            <CreateTime>{timestamp}</CreateTime>\
            <MsgType><![CDATA[text]]></MsgType>\
            <Content><![CDATA[{text}]]></Content>\
        </xml>"
    )
}

/// Extract the text content of an XML tag, handling both CDATA and plain forms.
pub fn extract_xml_field<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    // Try CDATA form: <Tag><![CDATA[value]]></Tag>
    let cdata_open = format!("<{tag}><![CDATA[");
    let cdata_close = format!("]]></{tag}>");
    if let Some(start) = xml.find(&cdata_open) {
        let value_start = start + cdata_open.len();
        if let Some(end_offset) = xml[value_start..].find(&cdata_close) {
            return Some(&xml[value_start..value_start + end_offset]);
        }
    }

    // Fall back to plain text form: <Tag>value</Tag>
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end_offset = xml[start..].find(&close)?;
    Some(&xml[start..start + end_offset])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_unchanged() {
        assert_eq!(to_wechat_text("hello"), "hello");
    }

    #[test]
    fn long_text_truncated() {
        let long: String = "a".repeat(3000);
        let result = to_wechat_text(&long);
        assert_eq!(result.chars().count(), WECHAT_TEXT_MAX);
    }

    #[test]
    fn reply_xml_contains_fields() {
        let xml = build_reply_xml("user123", "gh_abc", "hello");
        assert!(xml.contains("<![CDATA[user123]]>"));
        assert!(xml.contains("<![CDATA[gh_abc]]>"));
        assert!(xml.contains("<![CDATA[hello]]>"));
        assert!(xml.contains("<MsgType><![CDATA[text]]></MsgType>"));
    }

    #[test]
    fn extract_cdata_field() {
        let xml = "<xml><FromUserName><![CDATA[oABC123]]></FromUserName></xml>";
        assert_eq!(extract_xml_field(xml, "FromUserName"), Some("oABC123"));
    }

    #[test]
    fn extract_plain_field() {
        let xml = "<xml><MsgType>text</MsgType></xml>";
        assert_eq!(extract_xml_field(xml, "MsgType"), Some("text"));
    }

    #[test]
    fn extract_missing_field_returns_none() {
        let xml = "<xml><MsgType>text</MsgType></xml>";
        assert_eq!(extract_xml_field(xml, "Content"), None);
    }
}
