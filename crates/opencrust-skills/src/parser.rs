use opencrust_common::{Error, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
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
    // Name must be alphanumeric + hyphens
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-') {
        return Err(Error::Skill(format!(
            "skill name '{}' contains invalid characters (only alphanumeric and hyphens allowed)",
            name
        )));
    }
    if skill.frontmatter.description.is_empty() {
        return Err(Error::Skill("skill description must not be empty".into()));
    }
    if skill.body.is_empty() {
        return Err(Error::Skill("skill body must not be empty".into()));
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
}
