use opencrust_common::{Error, Result};
use std::path::{Path, PathBuf};

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
        std::fs::write(&path, content)?;
        Ok(())
    }
}
