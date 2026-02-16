use opencrust_common::Result;

/// Input validation and sanitization for messages and commands.
pub struct InputValidator;

impl InputValidator {
    /// Check for potential prompt injection patterns.
    pub fn check_prompt_injection(input: &str) -> bool {
        let patterns = [
            "ignore previous instructions",
            "ignore all previous",
            "disregard your instructions",
            "you are now",
            "new instructions:",
            "system prompt:",
        ];

        let lower = input.to_lowercase();
        patterns.iter().any(|p| lower.contains(p))
    }

    /// Sanitize user input by removing control characters.
    pub fn sanitize(input: &str) -> String {
        input
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect()
    }

    /// Validate that a channel identifier is well-formed.
    pub fn validate_channel_id(id: &str) -> Result<()> {
        if id.is_empty() {
            return Err(opencrust_common::Error::Security(
                "channel ID cannot be empty".into(),
            ));
        }
        if id.len() > 256 {
            return Err(opencrust_common::Error::Security(
                "channel ID too long".into(),
            ));
        }
        Ok(())
    }
}
