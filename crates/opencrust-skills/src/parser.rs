use opencrust_common::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,

    // ── opencrust-native fields ──────────────────────────────────────────
    /// Why this skill was saved — recorded at creation time for auditability.
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,

    // ── agentskills.io compatible fields ────────────────────────────────
    /// Skill version string (e.g. "1.0.0"). Optional.
    #[serde(default)]
    pub version: Option<String>,
    /// License identifier or reference (e.g. "MIT", "Apache-2.0").
    #[serde(default)]
    pub license: Option<String>,
    /// Environment or product compatibility notes. Max 500 chars.
    #[serde(default)]
    pub compatibility: Option<String>,
    /// Arbitrary key-value metadata. Clients may use namespaced keys
    /// (e.g. `hermes:`, `opencrust:`) to avoid conflicts.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct SkillDefinition {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
    pub source_path: Option<PathBuf>,
}

/// Parse a SKILL.md file: YAML frontmatter between `---` delimiters, followed by markdown body.
pub fn parse_skill(content: &str) -> Result<SkillDefinition> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Err(Error::Skill(
            "missing frontmatter: file must start with ---".into(),
        ));
    }

    // Skip the opening `---` line
    let after_open = &trimmed[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);

    let close_pos = after_open
        .find("\n---")
        .ok_or_else(|| Error::Skill("missing closing --- for frontmatter".into()))?;

    let yaml_block = &after_open[..close_pos];
    let body_start = close_pos + 4; // skip \n---
    let body = if body_start < after_open.len() {
        after_open[body_start..].trim().to_string()
    } else {
        String::new()
    };

    let frontmatter: SkillFrontmatter = serde_yaml::from_str(yaml_block)
        .map_err(|e| Error::Skill(format!("invalid frontmatter YAML: {e}")))?;

    Ok(SkillDefinition {
        frontmatter,
        body,
        source_path: None,
    })
}

/// Validate a parsed skill definition.
pub fn validate_skill(skill: &SkillDefinition) -> Result<()> {
    let name = &skill.frontmatter.name;
    if name.is_empty() {
        return Err(Error::Skill("skill name must not be empty".into()));
    }
    // Name must be safe for use as a filename: letters, digits, hyphens, underscores only.
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::Skill(format!(
            "skill name '{name}' contains invalid characters. \
             Use letters, digits, hyphens, or underscores only."
        )));
    }
    if skill.frontmatter.description.is_empty() {
        return Err(Error::Skill("skill description must not be empty".into()));
    }
    if skill.body.is_empty() {
        return Err(Error::Skill("skill body must not be empty".into()));
    }
    if skill
        .frontmatter
        .compatibility
        .as_deref()
        .is_some_and(|c| c.len() > 500)
    {
        return Err(Error::Skill(
            "compatibility field exceeds 500 characters".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_SKILL: &str = r#"---
name: test-skill
description: A test skill
triggers:
  - hello
  - greet
dependencies:
  - other-skill
---

# Test Skill

This is the skill body with instructions.
"#;

    #[test]
    fn parse_valid_skill() {
        let skill = parse_skill(VALID_SKILL).unwrap();
        assert_eq!(skill.frontmatter.name, "test-skill");
        assert_eq!(skill.frontmatter.description, "A test skill");
        assert_eq!(skill.frontmatter.triggers, vec!["hello", "greet"]);
        assert_eq!(skill.frontmatter.dependencies, vec!["other-skill"]);
        assert!(skill.body.contains("# Test Skill"));
        assert!(skill.body.contains("This is the skill body"));
        validate_skill(&skill).unwrap();
    }

    #[test]
    fn missing_frontmatter() {
        let result = parse_skill("# Just a markdown file\nNo frontmatter here.");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing frontmatter"));
    }

    #[test]
    fn missing_name() {
        let content = "---\ndescription: test\n---\nBody here.";
        let result = parse_skill(content);
        // serde_yaml will error on missing required field
        assert!(result.is_err());
    }

    #[test]
    fn invalid_name_chars() {
        let content = "---\nname: bad name!\ndescription: test\n---\nBody here.";
        let skill = parse_skill(content).unwrap();
        let result = validate_skill(&skill);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid characters"));
    }

    #[test]
    fn empty_body() {
        let content = "---\nname: test\ndescription: test\n---\n";
        let skill = parse_skill(content).unwrap();
        let result = validate_skill(&skill);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("body must not be empty"));
    }

    #[test]
    fn parse_agentskills_compatible_fields() {
        let content = r#"---
name: pdf-processing
description: Extract PDF text, fill forms, merge files. Use when handling PDFs.
version: "1.0.0"
license: Apache-2.0
compatibility: Requires Python 3.11+ and pdfplumber
metadata:
  author: example-org
  tags: [pdf, document]
---
# PDF Processing
Step-by-step instructions for handling PDF files.
"#;
        let skill = parse_skill(content).unwrap();
        assert_eq!(skill.frontmatter.name, "pdf-processing");
        assert_eq!(skill.frontmatter.version.as_deref(), Some("1.0.0"));
        assert_eq!(skill.frontmatter.license.as_deref(), Some("Apache-2.0"));
        assert!(skill.frontmatter.compatibility.is_some());
        assert!(skill.frontmatter.metadata.is_some());
        validate_skill(&skill).unwrap();
    }

    #[test]
    fn agentskills_metadata_hermes_namespace() {
        let content = r#"---
name: systematic-debugging
description: Use for any bug or test failure. 4-phase root cause investigation.
version: "1.1.0"
license: MIT
metadata:
  hermes:
    tags: [debugging, troubleshooting]
    related_skills: [test-driven-development]
---
Follow the four phases systematically.
"#;
        let skill = parse_skill(content).unwrap();
        assert_eq!(skill.frontmatter.name, "systematic-debugging");
        let meta = skill.frontmatter.metadata.as_ref().unwrap();
        assert!(meta["hermes"]["tags"].is_array());
        validate_skill(&skill).unwrap();
    }

    #[test]
    fn compatibility_too_long_fails_validation() {
        let long_compat = "x".repeat(501);
        let content =
            format!("---\nname: test\ndescription: test\ncompatibility: {long_compat}\n---\nBody.");
        let skill = parse_skill(&content).unwrap();
        let err = validate_skill(&skill).unwrap_err().to_string();
        assert!(err.contains("500 characters"));
    }

    #[test]
    fn optional_agentskills_fields_default_to_none() {
        let content = "---\nname: minimal\ndescription: Minimal skill\n---\nMinimal body.";
        let skill = parse_skill(content).unwrap();
        assert!(skill.frontmatter.version.is_none());
        assert!(skill.frontmatter.license.is_none());
        assert!(skill.frontmatter.compatibility.is_none());
        assert!(skill.frontmatter.metadata.is_none());
        validate_skill(&skill).unwrap();
    }
}
