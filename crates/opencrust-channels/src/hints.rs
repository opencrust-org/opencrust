/// Format tool-hint lines in a channel-agnostic way.
///
/// Lines that start with `🔧` are treated as hints.  They are wrapped as
/// `[🔧 ...]` and placed above the rest of the message, separated by a blank
/// line.  This format is readable on every platform regardless of markdown
/// support.
///
/// If the text contains no hint lines it is returned unchanged.
pub fn format_hints(text: &str) -> String {
    let mut hints = Vec::new();
    let mut rest = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('🔧') {
            // Strip the bare emoji prefix and re-wrap as [🔧 ...]
            let inner = trimmed.trim_start_matches('🔧').trim();
            hints.push(format!("[🔧 {inner}]"));
        } else if !trimmed.is_empty() || !rest.is_empty() {
            rest.push(line.to_string());
        }
    }

    if hints.is_empty() {
        return text.to_string();
    }

    let mut out = hints.join("\n");
    let body = rest.join("\n");
    let body_trimmed = body.trim();
    if !body_trimmed.is_empty() {
        out.push_str("\n\n");
        out.push_str(body_trimmed);
    }
    out
}

/// Extract formatted hint lines from text, returning `(hints, body)`.
/// `hints` is `None` if no hint lines were found.
pub fn split_hints(text: &str) -> (Option<String>, String) {
    let mut hint_lines = Vec::new();
    let mut rest = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('🔧') {
            let inner = trimmed.trim_start_matches('🔧').trim();
            hint_lines.push(format!("[🔧 {inner}]"));
        } else if trimmed.starts_with("[🔧 ") && trimmed.ends_with(']') {
            hint_lines.push(trimmed.to_string());
        } else if !trimmed.is_empty() || !rest.is_empty() {
            rest.push(line.to_string());
        }
    }

    let hints = if hint_lines.is_empty() {
        None
    } else {
        Some(hint_lines.join("\n"))
    };
    let body = rest.join("\n").trim().to_string();
    (hints, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hints_unchanged() {
        let text = "hello world";
        assert_eq!(format_hints(text), text);
    }

    #[test]
    fn hints_only() {
        let text = "🔧 bash: ls";
        assert_eq!(format_hints(text), "[🔧 bash: ls]");
    }

    #[test]
    fn hints_above_response() {
        let text = "🔧 bash: ls\n\nhere are the files";
        assert_eq!(format_hints(text), "[🔧 bash: ls]\n\nhere are the files");
    }

    #[test]
    fn multiple_hints() {
        let text = "🔧 bash: ls\n🔧 file_read: main.rs\n\nresponse";
        assert_eq!(
            format_hints(text),
            "[🔧 bash: ls]\n[🔧 file_read: main.rs]\n\nresponse"
        );
    }
}
