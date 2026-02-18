use opencrust_common::Result;
use std::path::PathBuf;
use tracing;

use crate::parser::{self, SkillDefinition};

pub struct SkillScanner {
    skills_dir: PathBuf,
}

impl SkillScanner {
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            skills_dir: skills_dir.into(),
        }
    }

    /// Discover all valid skill files in the skills directory.
    /// Invalid files are warned and skipped, following the PluginLoader::discover() pattern.
    pub fn discover(&self) -> Result<Vec<SkillDefinition>> {
        let mut skills = Vec::new();

        if !self.skills_dir.exists() {
            return Ok(skills);
        }

        let entries = std::fs::read_dir(&self.skills_dir)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            let ext = path.extension().and_then(|e| e.to_str());
            if ext != Some("md") {
                continue;
            }

            match self.load_skill(&path) {
                Ok(skill) => {
                    tracing::info!("discovered skill: {}", skill.frontmatter.name);
                    skills.push(skill);
                }
                Err(e) => {
                    tracing::warn!("skipping invalid skill at {}: {}", path.display(), e);
                }
            }
        }

        Ok(skills)
    }

    fn load_skill(&self, path: &std::path::Path) -> Result<SkillDefinition> {
        let content = std::fs::read_to_string(path)?;
        let mut skill = parser::parse_skill(&content)?;
        parser::validate_skill(&skill)?;
        skill.source_path = Some(path.to_path_buf());
        Ok(skill)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn empty_dir() {
        let dir = std::env::temp_dir().join("opencrust_test_empty_skills");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert!(skills.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nonexistent_dir() {
        let scanner = SkillScanner::new("/tmp/opencrust_nonexistent_skills_dir");
        let skills = scanner.discover().unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn discovers_valid_skill_file() {
        let dir = std::env::temp_dir().join("opencrust_test_valid_skills");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(
            dir.join("greet.md"),
            "---\nname: greet\ndescription: Greeting skill\n---\nSay hello to the user.",
        )
        .unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].frontmatter.name, "greet");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_invalid_files() {
        let dir = std::env::temp_dir().join("opencrust_test_skip_invalid");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Valid skill
        fs::write(
            dir.join("valid.md"),
            "---\nname: valid\ndescription: Valid skill\n---\nBody content.",
        )
        .unwrap();
        // Invalid skill (no frontmatter)
        fs::write(dir.join("invalid.md"), "Just plain markdown.").unwrap();
        // Non-md file (should be ignored)
        fs::write(dir.join("notes.txt"), "not a skill").unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].frontmatter.name, "valid");

        let _ = fs::remove_dir_all(&dir);
    }
}
