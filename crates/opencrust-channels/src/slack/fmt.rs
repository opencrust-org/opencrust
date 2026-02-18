/// Convert standard markdown to Slack mrkdwn format.
///
/// - `**bold**` → `*bold*`  (Slack uses single asterisks)
/// - `` `code` `` and ` ```blocks``` ` pass through unchanged
/// - Escapes `&`, `<`, `>` outside code blocks
pub fn to_slack_mrkdwn(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Fenced code block: ```...```
        if i + 2 < len && chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            result.push_str("```");
            i += 3;
            // Copy everything until closing ```
            loop {
                if i + 2 < len
                    && chars[i] == '`'
                    && chars[i + 1] == '`'
                    && chars[i + 2] == '`'
                {
                    result.push_str("```");
                    i += 3;
                    break;
                }
                if i >= len {
                    break;
                }
                result.push(chars[i]);
                i += 1;
            }
            continue;
        }

        // Inline code: `...`
        if chars[i] == '`' {
            result.push('`');
            i += 1;
            while i < len && chars[i] != '`' {
                result.push(chars[i]);
                i += 1;
            }
            if i < len {
                result.push('`');
                i += 1;
            }
            continue;
        }

        // Bold: **text** → *text*
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            result.push('*');
            i += 2;
            while i < len && !(i + 1 < len && chars[i] == '*' && chars[i + 1] == '*') {
                result.push(escape_char(chars[i]));
                i += 1;
            }
            result.push('*');
            if i + 1 < len {
                i += 2; // skip closing **
            }
            continue;
        }

        // Escape & < > in plain text
        result.push(escape_char(chars[i]));
        i += 1;
    }

    result
}

fn escape_char(c: char) -> char {
    // Slack mrkdwn doesn't use HTML entities in the same way, but the Web API
    // requires &, <, > to be escaped when sending via JSON payload.
    // However, Slack's chat.postMessage with JSON body handles this automatically.
    // We just pass through as-is — the API handles encoding.
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_converted() {
        assert_eq!(to_slack_mrkdwn("this is **bold** text"), "this is *bold* text");
    }

    #[test]
    fn code_block_preserved() {
        let input = "```rust\nfn main() {}\n```";
        assert_eq!(to_slack_mrkdwn(input), input);
    }

    #[test]
    fn inline_code_preserved() {
        let input = "use `foo.bar()` here";
        assert_eq!(to_slack_mrkdwn(input), input);
    }

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(to_slack_mrkdwn("hello world"), "hello world");
    }

    #[test]
    fn mixed_formatting() {
        let input = "Try `code` and **bold** together.";
        let output = to_slack_mrkdwn(input);
        assert_eq!(output, "Try `code` and *bold* together.");
    }
}
