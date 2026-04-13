use opencrust_common::{Error, Result};
use std::path::{Path, PathBuf};

#[cfg(test)]
const VALID_SKILL_MD: &str = "\
---
name: test-skill
description: A test skill for installer tests
---
Do something useful.
";

use crate::parser::{self, SkillDefinition};

pub struct SkillInstaller {
    skills_dir: PathBuf,
}

impl SkillInstaller {
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            skills_dir: skills_dir.into(),
        }
    }

    /// Install a skill from a URL. Downloads the content, parses and validates it,
    /// then writes it to the skills directory.
    pub async fn install_from_url(&self, url: &str) -> Result<SkillDefinition> {
        let response = reqwest::get(url)
            .await
            .map_err(|e| Error::Skill(format!("failed to download skill from {url}: {e}")))?;

        if !response.status().is_success() {
            return Err(Error::Skill(format!(
                "failed to download skill from {url}: HTTP {}",
                response.status()
            )));
        }

        let content = response
            .text()
            .await
            .map_err(|e| Error::Skill(format!("failed to read response body: {e}")))?;

        let skill = parser::parse_skill(&content)?;
        parser::validate_skill(&skill)?;

        self.write_skill(&skill.frontmatter.name, &content)?;

        let mut skill = skill;
        skill.source_path = Some(self.skill_path(&skill.frontmatter.name));
        Ok(skill)
    }

    /// Install a skill from a local file path. Reads, parses, validates, and copies
    /// to the skills directory.
    pub fn install_from_path(&self, path: &Path) -> Result<SkillDefinition> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| Error::Skill(format!("failed to read {}: {e}", path.display())))?;

        let skill = parser::parse_skill(&content)?;
        parser::validate_skill(&skill)?;

        self.write_skill(&skill.frontmatter.name, &content)?;

        let mut skill = skill;
        skill.source_path = Some(self.skill_path(&skill.frontmatter.name));
        Ok(skill)
    }

    /// Remove a skill by name. Returns true if the file existed and was removed.
    pub fn remove(&self, name: &str) -> Result<bool> {
        let path = self.skill_path(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.skills_dir.join(format!("{name}.md"))
    }

    fn write_skill(&self, name: &str, content: &str) -> Result<()> {
        if !self.skills_dir.exists() {
            std::fs::create_dir_all(&self.skills_dir)?;
        }
        let path = self.skill_path(name);
        if path.exists() {
            tracing::warn!("skill '{name}' already exists and will be overwritten");
        }
        std::fs::write(&path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_skills_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opencrust_installer_test_{suffix}"));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn install_from_path_creates_dir_and_file() {
        let skills_dir = temp_skills_dir("from_path");
        let installer = SkillInstaller::new(&skills_dir);

        // Write source skill to a temp file
        let src = std::env::temp_dir().join("test-skill-src.md");
        fs::write(&src, VALID_SKILL_MD).unwrap();

        let skill = installer.install_from_path(&src).unwrap();
        assert_eq!(skill.frontmatter.name, "test-skill");
        assert!(skills_dir.join("test-skill.md").exists());
        assert_eq!(skill.source_path, Some(skills_dir.join("test-skill.md")));

        let _ = fs::remove_dir_all(&skills_dir);
        let _ = fs::remove_file(&src);
    }

    #[test]
    fn install_from_path_creates_skills_dir_if_missing() {
        let skills_dir = temp_skills_dir("autocreate");
        assert!(!skills_dir.exists());

        let src = std::env::temp_dir().join("test-skill-autocreate.md");
        fs::write(&src, VALID_SKILL_MD).unwrap();

        let installer = SkillInstaller::new(&skills_dir);
        installer.install_from_path(&src).unwrap();
        assert!(skills_dir.exists());

        let _ = fs::remove_dir_all(&skills_dir);
        let _ = fs::remove_file(&src);
    }

    #[test]
    fn install_from_path_invalid_skill_returns_error() {
        let skills_dir = temp_skills_dir("invalid");
        let installer = SkillInstaller::new(&skills_dir);

        let src = std::env::temp_dir().join("bad-skill.md");
        fs::write(&src, "no frontmatter here").unwrap();

        let result = installer.install_from_path(&src);
        assert!(result.is_err());
        // Skills dir should NOT have been written to
        assert!(!skills_dir.join("bad-skill.md").exists());

        let _ = fs::remove_dir_all(&skills_dir);
        let _ = fs::remove_file(&src);
    }

    #[test]
    fn install_from_path_nonexistent_file_returns_error() {
        let skills_dir = temp_skills_dir("nofile");
        let installer = SkillInstaller::new(&skills_dir);
        let result = installer.install_from_path(Path::new("/tmp/does-not-exist-opencrust.md"));
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&skills_dir);
    }

    #[test]
    fn remove_existing_skill() {
        let skills_dir = temp_skills_dir("remove");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(skills_dir.join("my-skill.md"), VALID_SKILL_MD).unwrap();

        let installer = SkillInstaller::new(&skills_dir);
        assert!(installer.remove("my-skill").unwrap());
        assert!(!skills_dir.join("my-skill.md").exists());

        let _ = fs::remove_dir_all(&skills_dir);
    }

    #[test]
    fn remove_nonexistent_skill_returns_false() {
        let skills_dir = temp_skills_dir("remove_missing");
        fs::create_dir_all(&skills_dir).unwrap();

        let installer = SkillInstaller::new(&skills_dir);
        assert!(!installer.remove("ghost-skill").unwrap());

        let _ = fs::remove_dir_all(&skills_dir);
    }

    #[test]
    fn overwrite_existing_skill_succeeds() {
        let skills_dir = temp_skills_dir("overwrite");
        let installer = SkillInstaller::new(&skills_dir);

        let src = std::env::temp_dir().join("test-skill-overwrite.md");
        fs::write(&src, VALID_SKILL_MD).unwrap();

        installer.install_from_path(&src).unwrap();

        // Install again — should overwrite without error
        let updated = "\
---
name: test-skill
description: Updated description
---
Updated body.
";
        fs::write(&src, updated).unwrap();
        let skill = installer.install_from_path(&src).unwrap();
        assert_eq!(skill.frontmatter.description, "Updated description");

        let content = fs::read_to_string(skills_dir.join("test-skill.md")).unwrap();
        assert!(content.contains("Updated description"));

        let _ = fs::remove_dir_all(&skills_dir);
        let _ = fs::remove_file(&src);
    }
}
