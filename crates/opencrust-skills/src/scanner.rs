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

    /// Discover all valid skills in the skills directory.
    ///
    /// Supports two layouts (both may coexist):
    /// - Flat file: `skills/skill-name.md`
    /// - Folder:    `skills/skill-name/SKILL.md`  (agentskills.io format)
    ///
    /// Invalid entries are warned and skipped. Results are sorted by name.
    pub fn discover(&self) -> Result<Vec<SkillDefinition>> {
        let mut skills = Vec::new();

        if !self.skills_dir.exists() {
            return Ok(skills);
        }

        let entries = std::fs::read_dir(&self.skills_dir)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                // Flat layout: skill-name.md
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
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
            } else if path.is_dir() {
                // Folder layout: skill-name/SKILL.md
                let skill_md = path.join("SKILL.md");
                if !skill_md.exists() {
                    continue;
                }
                match self.load_skill(&skill_md) {
                    Ok(skill) => {
                        tracing::info!("discovered skill (folder): {}", skill.frontmatter.name);
                        skills.push(skill);
                    }
                    Err(e) => {
                        tracing::warn!("skipping invalid skill at {}: {}", skill_md.display(), e);
                    }
                }
            }
        }

        skills.sort_by(|a, b| a.frontmatter.name.cmp(&b.frontmatter.name));
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
    fn skills_sorted_by_name() {
        let dir = std::env::temp_dir().join("opencrust_test_sort_skills");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(
            dir.join("zebra.md"),
            "---\nname: zebra\ndescription: Z skill\n---\nDo Z.",
        )
        .unwrap();
        fs::write(
            dir.join("apple.md"),
            "---\nname: apple\ndescription: A skill\n---\nDo A.",
        )
        .unwrap();
        fs::write(
            dir.join("mango.md"),
            "---\nname: mango\ndescription: M skill\n---\nDo M.",
        )
        .unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert_eq!(skills.len(), 3);
        assert_eq!(skills[0].frontmatter.name, "apple");
        assert_eq!(skills[1].frontmatter.name, "mango");
        assert_eq!(skills[2].frontmatter.name, "zebra");

        let _ = fs::remove_dir_all(&dir);
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

    // ── Folder-layout tests ──────────────────────────────────────────────

    #[test]
    fn discovers_folder_skill() {
        let dir = std::env::temp_dir().join("opencrust_test_folder_skill");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let skill_dir = dir.join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: Folder skill\n---\nDo something useful here.",
        )
        .unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].frontmatter.name, "my-skill");
        assert!(
            skills[0]
                .source_path
                .as_ref()
                .unwrap()
                .ends_with("SKILL.md")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_folder_without_skill_md() {
        let dir = std::env::temp_dir().join("opencrust_test_folder_no_skill_md");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Folder with no SKILL.md — should be skipped
        fs::create_dir_all(dir.join("empty-folder")).unwrap();
        fs::write(dir.join("empty-folder/README.md"), "no skill here").unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert!(skills.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flat_and_folder_coexist_sorted() {
        let dir = std::env::temp_dir().join("opencrust_test_mixed_layout");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Flat file skill
        fs::write(
            dir.join("alpha.md"),
            "---\nname: alpha\ndescription: Flat skill\n---\nFlat body content here.",
        )
        .unwrap();

        // Folder skill
        let folder = dir.join("beta");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("SKILL.md"),
            "---\nname: beta\ndescription: Folder skill\n---\nFolder body content here.",
        )
        .unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].frontmatter.name, "alpha");
        assert_eq!(skills[1].frontmatter.name, "beta");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_folder_skill_md_is_skipped() {
        let dir = std::env::temp_dir().join("opencrust_test_invalid_folder_skill");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let skill_dir = dir.join("broken-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "no frontmatter at all").unwrap();

        // Also add a valid flat skill so we confirm the invalid one is skipped
        fs::write(
            dir.join("good.md"),
            "---\nname: good\ndescription: Good skill\n---\nWorks fine.",
        )
        .unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].frontmatter.name, "good");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_skill_source_path_points_to_skill_md() {
        let dir = std::env::temp_dir().join("opencrust_test_folder_source_path");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let skill_dir = dir.join("debug-tool");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: debug-tool\ndescription: Debugging helper\n---\nStep-by-step debug guide.",
        )
        .unwrap();

        let scanner = SkillScanner::new(&dir);
        let skills = scanner.discover().unwrap();
        assert_eq!(skills.len(), 1);

        let src = skills[0].source_path.as_ref().unwrap();
        assert!(src.ends_with("debug-tool/SKILL.md"));

        let _ = fs::remove_dir_all(&dir);
    }
}
