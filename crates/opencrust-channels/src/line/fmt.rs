/// Maximum characters allowed in a single LINE text message.
const LINE_TEXT_MAX: usize = 5000;

/// Prepare text for a LINE text message. Truncates at the 5000-char limit.
pub fn to_line_text(text: &str) -> String {
    if text.chars().count() <= LINE_TEXT_MAX {
        text.to_string()
    } else {
        text.chars().take(LINE_TEXT_MAX).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_unchanged() {
        assert_eq!(to_line_text("hello"), "hello");
    }

    #[test]
    fn long_text_truncated() {
        let long: String = "a".repeat(6000);
        let result = to_line_text(&long);
        assert_eq!(result.chars().count(), LINE_TEXT_MAX);
    }
}
