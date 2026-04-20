use async_trait::async_trait;
use opencrust_common::Result;
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolOutput};

/// Maximum number of skills allowed in the skills directory.
const MAX_SKILLS: usize = 30;
/// Minimum body length in characters to prevent trivial one-liner skills.
const MIN_BODY_LEN: usize = 80;
/// Minimum rationale length to ensure the agent genuinely reflects before saving.
const MIN_RATIONALE_LEN: usize = 40;

/// Allow the agent to save, update, or extend reusable skills.
///
/// Actions:
/// - `create` (default): save a new skill; enforces three quality-control layers
/// - `patch`: update body/description/triggers of an existing skill
/// - `write_file`: add a supplementary `.md` file inside an existing skill folder
pub struct CreateSkillTool {
    skills_dir: PathBuf,
}

impl CreateSkillTool {
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            skills_dir: skills_dir.into(),
        }
    }

    fn count_existing_skills(&self) -> usize {
        std::fs::read_dir(&self.skills_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        let p = e.path();
                        if p.is_dir() {
                            return p.join("SKILL.md").exists();
                        }
                        p.extension().and_then(|x| x.to_str()) == Some("md")
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// Returns true when a skill with this name already exists (either layout).
    fn skill_exists(&self, name: &str) -> bool {
        self.skills_dir.join(name).join("SKILL.md").exists()
            || self.skills_dir.join(format!("{name}.md")).exists()
    }

    /// Returns the canonical path of an existing skill file (folder layout preferred).
    fn skill_path_canonical(&self, name: &str) -> PathBuf {
        let folder = self.skills_dir.join(name).join("SKILL.md");
        if folder.exists() {
            folder
        } else {
            self.skills_dir.join(format!("{name}.md"))
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "opencrust_skill_{name}_{}.md",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ))
    }

    async fn execute_create(&self, input: &serde_json::Value) -> Result<ToolOutput> {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return Ok(ToolOutput::error("missing required parameter: 'name'")),
        };
        let description = match input.get("description").and_then(|v| v.as_str()) {
            Some(d) => d.to_string(),
            None => {
                return Ok(ToolOutput::error(
                    "missing required parameter: 'description'",
                ));
            }
        };
        let body = match input.get("body").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return Ok(ToolOutput::error("missing required parameter: 'body'")),
        };
        let rationale = match input.get("rationale").and_then(|v| v.as_str()) {
            Some(r) => r.to_string(),
            None => {
                return Ok(ToolOutput::error(
                    "missing required parameter: 'rationale' — explain why this skill is \
                     worth persisting before saving",
                ));
            }
        };
        let triggers: Vec<String> = input
            .get("triggers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let overwrite = input
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Layer 3: Reflection gate
        if rationale.trim().len() < MIN_RATIONALE_LEN {
            return Ok(ToolOutput::error(format!(
                "rationale too vague ({} chars, need ≥{MIN_RATIONALE_LEN}). \
                 Explain specifically: would you need to figure this out from scratch next time? \
                 If the answer isn't clearly yes, don't save this skill.",
                rationale.trim().len()
            )));
        }

        // Layer 2: Mechanical guardrails
        if body.trim().len() < MIN_BODY_LEN {
            return Ok(ToolOutput::error(format!(
                "skill body too short ({} chars, need ≥{MIN_BODY_LEN}). \
                 A useful skill needs enough detail to be actionable — \
                 single commands or one-liners don't qualify.",
                body.trim().len()
            )));
        }

        let existing = self.count_existing_skills();
        if existing >= MAX_SKILLS {
            return Ok(ToolOutput::error(format!(
                "skill library full ({existing}/{MAX_SKILLS}). \
                 Remove an outdated skill with `opencrust skill remove <name>` before adding new ones."
            )));
        }

        if self.skill_exists(&name) && !overwrite {
            return Ok(ToolOutput::error(format!(
                "skill '{name}' already exists. \
                 If you want to update it, call create_skill again with `overwrite: true`. \
                 If this is a different skill, choose a different name."
            )));
        }

        // Build SKILL.md content
        let mut content = format!("---\nname: {name}\ndescription: {description}\n");
        content.push_str(&format!(
            "rationale: \"{}\"\n",
            rationale.replace('"', "\\\"")
        ));
        if !triggers.is_empty() {
            content.push_str("triggers:\n");
            for t in &triggers {
                content.push_str(&format!("  - {t}\n"));
            }
        }
        content.push_str("---\n\n");
        content.push_str(&body);
        content.push('\n');

        let installer = opencrust_skills::SkillInstaller::new(&self.skills_dir);
        let tmp = Self::temp_path(&name);
        if let Err(e) = std::fs::write(&tmp, &content) {
            return Ok(ToolOutput::error(format!(
                "failed to stage skill file: {e}"
            )));
        }

        match installer.install_from_path(&tmp) {
            Ok(skill) => {
                let _ = std::fs::remove_file(&tmp);
                let action = if overwrite && self.skill_exists(&name) {
                    "updated"
                } else {
                    "saved"
                };
                Ok(ToolOutput::success(format!(
                    "skill '{}' {action} ({}/{MAX_SKILLS} skills used) — active immediately",
                    skill.frontmatter.name,
                    existing + 1,
                )))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Ok(ToolOutput::error(format!("invalid skill: {e}")))
            }
        }
    }

    async fn execute_patch(&self, input: &serde_json::Value) -> Result<ToolOutput> {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return Ok(ToolOutput::error("missing required parameter: 'name'")),
        };

        if !self.skill_exists(&name) {
            return Ok(ToolOutput::error(format!(
                "skill '{name}' not found. Use action='create' to create a new skill."
            )));
        }

        let new_description = input.get("description").and_then(|v| v.as_str());
        let new_body = input.get("body").and_then(|v| v.as_str());
        let new_triggers_val = input.get("triggers");

        if new_description.is_none() && new_body.is_none() && new_triggers_val.is_none() {
            return Ok(ToolOutput::error(
                "patch: provide at least one of: 'body', 'description', 'triggers'",
            ));
        }

        // Read and parse existing skill
        let existing_path = self.skill_path_canonical(&name);
        let existing_content = match std::fs::read_to_string(&existing_path) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "failed to read skill '{name}': {e}"
                )));
            }
        };
        let existing = match opencrust_skills::parse_skill(&existing_content) {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "failed to parse skill '{name}': {e}"
                )));
            }
        };

        // Apply patches — use provided value or fall back to existing
        let description = new_description.unwrap_or(&existing.frontmatter.description);
        let body = new_body.unwrap_or(&existing.body);
        let triggers: Vec<String> = if let Some(arr) = new_triggers_val.and_then(|v| v.as_array()) {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        } else {
            existing.frontmatter.triggers.clone()
        };

        // Rebuild SKILL.md (preserve rationale and other original fields)
        let mut content = format!("---\nname: {name}\ndescription: {description}\n");
        if let Some(rationale) = &existing.frontmatter.rationale {
            content.push_str(&format!(
                "rationale: \"{}\"\n",
                rationale.replace('"', "\\\"")
            ));
        }
        if !triggers.is_empty() {
            content.push_str("triggers:\n");
            for t in &triggers {
                content.push_str(&format!("  - {t}\n"));
            }
        }
        content.push_str("---\n\n");
        content.push_str(body);
        content.push('\n');

        // Install via temp file (runs parse + validate + security scan + write)
        let installer = opencrust_skills::SkillInstaller::new(&self.skills_dir);
        let tmp = Self::temp_path(&name);
        if let Err(e) = std::fs::write(&tmp, &content) {
            return Ok(ToolOutput::error(format!("failed to stage patch: {e}")));
        }
        match installer.install_from_path(&tmp) {
            Ok(skill) => {
                let _ = std::fs::remove_file(&tmp);
                Ok(ToolOutput::success(format!(
                    "skill '{}' patched",
                    skill.frontmatter.name
                )))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Ok(ToolOutput::error(format!("patch failed: {e}")))
            }
        }
    }

    async fn execute_write_file(&self, input: &serde_json::Value) -> Result<ToolOutput> {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return Ok(ToolOutput::error("missing required parameter: 'name'")),
        };
        let filename = match input.get("filename").and_then(|v| v.as_str()) {
            Some(f) => f.to_string(),
            None => return Ok(ToolOutput::error("missing required parameter: 'filename'")),
        };
        let content = match input.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return Ok(ToolOutput::error("missing required parameter: 'content'")),
        };

        if !self.skill_exists(&name) {
            return Ok(ToolOutput::error(format!("skill '{name}' not found")));
        }

        // Reject path traversal and unsafe filenames
        if filename.contains("..") || filename.starts_with('/') || filename.starts_with('\\') {
            return Ok(ToolOutput::error(
                "filename must not contain '..' or start with '/' or '\\'",
            ));
        }
        if !filename.ends_with(".md") {
            return Ok(ToolOutput::error("filename must end with '.md'"));
        }
        if filename == "SKILL.md" {
            return Ok(ToolOutput::error(
                "use action='patch' to modify SKILL.md — write_file is for supplementary files only",
            ));
        }

        let skill_dir = self.skills_dir.join(&name);
        if !skill_dir.is_dir() {
            return Ok(ToolOutput::error(format!(
                "skill '{name}' is not in folder layout. \
                 Recreate it with action='create' and overwrite=true first."
            )));
        }

        let target = skill_dir.join(&filename);
        if let Err(e) = std::fs::write(&target, &content) {
            return Ok(ToolOutput::error(format!(
                "failed to write '{filename}': {e}"
            )));
        }
        Ok(ToolOutput::success(format!(
            "wrote '{filename}' to skill '{name}' folder"
        )))
    }
}

#[async_trait]
impl Tool for CreateSkillTool {
    fn name(&self) -> &str {
        "create_skill"
    }

    fn description(&self) -> &str {
        "Save, update, or extend a reusable skill. \
         action='create' (default): save a new skill. \
         action='patch': update body/description/triggers of an existing skill. \
         action='write_file': add a supplementary .md file inside an existing skill folder."
    }

    fn system_hint(&self) -> Option<&str> {
        Some(
            "Persist a reusable multi-step workflow you had to reason through. \
             See '## Self-Learning' in the system prompt for full guidance. \
             Always provide a specific `rationale` when creating.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "patch", "write_file"],
                    "description": "create (default): save a new skill. patch: update fields of an existing skill. write_file: add a supplementary .md file to an existing skill folder."
                },
                "name": {
                    "type": "string",
                    "description": "Unique skill name. Must be ASCII: letters, digits, hyphens, underscores only (e.g. 'disk-cleanup'). Always English."
                },
                "description": {
                    "type": "string",
                    "description": "One-line description. Required for create; optional for patch (replaces existing description)."
                },
                "body": {
                    "type": "string",
                    "description": "Markdown step-by-step instructions. Required for create (min 80 chars); optional for patch (replaces existing body)."
                },
                "rationale": {
                    "type": "string",
                    "description": "Why is this skill worth saving? Required for create (min 40 chars). Not used for patch or write_file."
                },
                "triggers": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Keywords that suggest using this skill. Optional for create; if provided in patch, replaces the trigger list."
                },
                "overwrite": {
                    "type": "boolean",
                    "description": "create only: set true to replace an existing skill with the same name."
                },
                "filename": {
                    "type": "string",
                    "description": "write_file only: name of the supplementary file to create (e.g. 'examples.md'). Must end in .md and must not be SKILL.md."
                },
                "content": {
                    "type": "string",
                    "description": "write_file only: content to write to the supplementary file."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        match input
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("create")
        {
            "create" => self.execute_create(&input).await,
            "patch" => self.execute_patch(&input).await,
            "write_file" => self.execute_write_file(&input).await,
            other => Ok(ToolOutput::error(format!(
                "unknown action '{other}'. Valid actions: create, patch, write_file"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
            allowed_tools: None,
        }
    }

    const GOOD_BODY: &str = "1. Run `df -h` to see disk usage by partition.\n\
        2. Identify partitions above 90% — those need attention.\n\
        3. Run `du -sh * | sort -rh | head -20` to find largest directories.\n\
        4. Remove caches with `brew cleanup` or clear Xcode derived data.";

    const GOOD_RATIONALE: &str = "This is a multi-step workflow I had to reason through — \
         the specific command combination is not obvious and I would need to look it up again.";

    // ── create action ────────────────────────────────────────────────────

    #[tokio::test]
    async fn creates_skill_with_all_layers_satisfied() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "name": "disk-cleanup",
                    "description": "Free up disk space step by step",
                    "body": GOOD_BODY,
                    "rationale": GOOD_RATIONALE,
                    "triggers": ["disk full", "free space"]
                }),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "unexpected error: {}", out.content);
        assert!(dir.path().join("disk-cleanup").join("SKILL.md").exists());
        assert!(out.content.contains("1/30"));

        let saved =
            std::fs::read_to_string(dir.path().join("disk-cleanup").join("SKILL.md")).unwrap();
        assert!(saved.contains("rationale:"));
        assert!(saved.contains("multi-step workflow"));
    }

    #[tokio::test]
    async fn layer3_rejects_vague_rationale() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "name": "my-skill",
                    "description": "Does something",
                    "body": GOOD_BODY,
                    "rationale": "useful"
                }),
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("rationale too vague"));
    }

    #[tokio::test]
    async fn layer2_rejects_short_body() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "name": "tiny",
                    "description": "Too short",
                    "body": "Run df -h.",
                    "rationale": GOOD_RATIONALE
                }),
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("body too short"));
    }

    #[tokio::test]
    async fn layer2_blocks_duplicate_without_overwrite() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        let input = serde_json::json!({
            "name": "disk-cleanup",
            "description": "Free up disk space",
            "body": GOOD_BODY,
            "rationale": GOOD_RATIONALE
        });

        tool.execute(&ctx(), input.clone()).await.unwrap();
        let out = tool.execute(&ctx(), input).await.unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("already exists"));
        assert!(out.content.contains("overwrite: true"));
    }

    #[tokio::test]
    async fn layer2_allows_overwrite_when_flag_set() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        let base = serde_json::json!({
            "name": "disk-cleanup",
            "description": "Original",
            "body": GOOD_BODY,
            "rationale": GOOD_RATIONALE
        });
        tool.execute(&ctx(), base).await.unwrap();

        let update = serde_json::json!({
            "name": "disk-cleanup",
            "description": "Updated version",
            "body": GOOD_BODY,
            "rationale": GOOD_RATIONALE,
            "overwrite": true
        });
        let out = tool.execute(&ctx(), update).await.unwrap();
        assert!(!out.is_error, "{}", out.content);
    }

    #[tokio::test]
    async fn layer2_enforces_hard_cap() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        for i in 0..MAX_SKILLS {
            let content = format!(
                "---\nname: skill-{i:02}\ndescription: test\n---\n{}",
                "x".repeat(MIN_BODY_LEN)
            );
            std::fs::write(dir.path().join(format!("skill-{i:02}.md")), content).unwrap();
        }

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "name": "overflow",
                    "description": "One too many",
                    "body": GOOD_BODY,
                    "rationale": GOOD_RATIONALE
                }),
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("skill library full"));
    }

    // ── patch action ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn patch_updates_body() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        // Create first
        tool.execute(
            &ctx(),
            serde_json::json!({
                "name": "my-skill",
                "description": "Original description",
                "body": GOOD_BODY,
                "rationale": GOOD_RATIONALE
            }),
        )
        .await
        .unwrap();

        let new_body = "1. Step one: check logs.\n\
            2. Step two: identify the error line and grep for it.\n\
            3. Step three: trace to the root cause using git log.\n\
            4. Step four: fix and verify with tests.";

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "action": "patch",
                    "name": "my-skill",
                    "body": new_body
                }),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("patched"));

        let saved = std::fs::read_to_string(dir.path().join("my-skill").join("SKILL.md")).unwrap();
        assert!(saved.contains("Step one: check logs"));
        // Original description preserved
        assert!(saved.contains("Original description"));
    }

    #[tokio::test]
    async fn patch_updates_description() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        tool.execute(
            &ctx(),
            serde_json::json!({
                "name": "my-skill",
                "description": "Old description",
                "body": GOOD_BODY,
                "rationale": GOOD_RATIONALE
            }),
        )
        .await
        .unwrap();

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "action": "patch",
                    "name": "my-skill",
                    "description": "New description"
                }),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "{}", out.content);
        let saved = std::fs::read_to_string(dir.path().join("my-skill").join("SKILL.md")).unwrap();
        assert!(saved.contains("New description"));
    }

    #[tokio::test]
    async fn patch_fails_if_skill_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "action": "patch",
                    "name": "ghost-skill",
                    "body": GOOD_BODY
                }),
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("not found"));
    }

    #[tokio::test]
    async fn patch_requires_at_least_one_field() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        tool.execute(
            &ctx(),
            serde_json::json!({
                "name": "my-skill",
                "description": "A skill",
                "body": GOOD_BODY,
                "rationale": GOOD_RATIONALE
            }),
        )
        .await
        .unwrap();

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({ "action": "patch", "name": "my-skill" }),
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("at least one of"));
    }

    // ── write_file action ────────────────────────────────────────────────

    #[tokio::test]
    async fn write_file_creates_supplementary_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        tool.execute(
            &ctx(),
            serde_json::json!({
                "name": "my-skill",
                "description": "A skill",
                "body": GOOD_BODY,
                "rationale": GOOD_RATIONALE
            }),
        )
        .await
        .unwrap();

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "action": "write_file",
                    "name": "my-skill",
                    "filename": "examples.md",
                    "content": "# Examples\n\n- Example 1: run on a full disk\n"
                }),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "{}", out.content);
        assert!(dir.path().join("my-skill").join("examples.md").exists());
    }

    #[tokio::test]
    async fn write_file_rejects_path_traversal() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        tool.execute(
            &ctx(),
            serde_json::json!({
                "name": "my-skill",
                "description": "A skill",
                "body": GOOD_BODY,
                "rationale": GOOD_RATIONALE
            }),
        )
        .await
        .unwrap();

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "action": "write_file",
                    "name": "my-skill",
                    "filename": "../evil.md",
                    "content": "bad content"
                }),
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("'..'"));
    }

    #[tokio::test]
    async fn write_file_rejects_non_md() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        tool.execute(
            &ctx(),
            serde_json::json!({
                "name": "my-skill",
                "description": "A skill",
                "body": GOOD_BODY,
                "rationale": GOOD_RATIONALE
            }),
        )
        .await
        .unwrap();

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "action": "write_file",
                    "name": "my-skill",
                    "filename": "script.sh",
                    "content": "rm -rf /"
                }),
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("must end with '.md'"));
    }

    #[tokio::test]
    async fn write_file_rejects_skill_md_override() {
        let dir = tempfile::TempDir::new().unwrap();
        let tool = CreateSkillTool::new(dir.path());

        tool.execute(
            &ctx(),
            serde_json::json!({
                "name": "my-skill",
                "description": "A skill",
                "body": GOOD_BODY,
                "rationale": GOOD_RATIONALE
            }),
        )
        .await
        .unwrap();

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "action": "write_file",
                    "name": "my-skill",
                    "filename": "SKILL.md",
                    "content": "overwrite attempt"
                }),
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("action='patch'"));
    }
}
