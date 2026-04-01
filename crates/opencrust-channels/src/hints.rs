/// Extract hint lines from text, returning `(hints, body)`.
/// Lines that start with `🔧` are treated as hints and kept as-is.
/// `hints` is `None` if no hint lines were found.
pub fn split_hints(text: &str) -> (Option<String>, String) {
    let mut hint_lines = Vec::new();
    let mut rest = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('🔧') {
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
    fn no_hints() {
        let (hints, body) = split_hints("hello world");
        assert!(hints.is_none());
        assert_eq!(body, "hello world");
    }

    #[test]
    fn hints_only() {
        let (hints, body) = split_hints("🔧 bash: ls");
        assert_eq!(hints.unwrap(), "🔧 bash: ls");
        assert!(body.is_empty());
    }

    #[test]
    fn hints_and_response() {
        let (hints, body) = split_hints("🔧 bash: ls\n\nhere are the files");
        assert_eq!(hints.unwrap(), "🔧 bash: ls");
        assert_eq!(body, "here are the files");
    }

    #[test]
    fn multiple_hints() {
        let (hints, body) = split_hints("🔧 bash: ls\n🔧 file_read: main.rs\n\nresponse");
        assert_eq!(hints.unwrap(), "🔧 bash: ls\n🔧 file_read: main.rs");
        assert_eq!(body, "response");
    }
}
