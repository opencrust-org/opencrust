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

/// Allow the agent to save a reusable skill discovered during a conversation.
///
/// Enforces three layers of quality control:
/// - Layer 1 (prompt): `## Self-Learning` section in the system prompt (positive trigger + gate)
/// - Layer 2 (mechanical): hard cap, min body length, duplicate guard
/// - Layer 3 (reflection): `rationale` field forces the agent to justify the save
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
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
                    .count()
            })
            .unwrap_or(0)
    }
}

#[async_trait]
impl Tool for CreateSkillTool {
    fn name(&self) -> &str {
        "create_skill"
    }

    fn description(&self) -> &str {
        "Save a reusable skill to the skills directory. \
         Active immediately in future conversations without restarting."
    }

    fn system_hint(&self) -> Option<&str> {
        Some(
            "Persist a reusable multi-step workflow you had to reason through. \
             See '## Self-Learning' in the system prompt for full guidance. \
             Always provide a specific `rationale`.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Unique skill name in hyphen-case (e.g. 'disk-cleanup'). Only alphanumeric and hyphens."
                },
                "description": {
                    "type": "string",
                    "description": "One-line description of what this skill does."
                },
                "body": {
                    "type": "string",
                    "description": "Markdown step-by-step instructions (minimum 80 characters)."
                },
                "rationale": {
                    "type": "string",
                    "description": "Why is this skill worth saving? Would you need to figure this out again from scratch? (minimum 40 characters — be specific)"
                },
                "triggers": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Keywords that suggest using this skill (e.g. ['disk full', 'free space'])."
                },
                "overwrite": {
                    "type": "boolean",
                    "description": "Set to true to explicitly replace an existing skill with the same name."
                }
            },
            "required": ["name", "description", "body", "rationale"]
        })
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        // --- Parse parameters ---
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

        // --- Layer 3: Reflection gate ---
        if rationale.trim().len() < MIN_RATIONALE_LEN {
            return Ok(ToolOutput::error(format!(
                "rationale too vague ({} chars, need ≥{MIN_RATIONALE_LEN}). \
                 Explain specifically: would you need to figure this out from scratch next time? \
                 If the answer isn't clearly yes, don't save this skill.",
                rationale.trim().len()
            )));
        }

        // --- Layer 2: Mechanical guardrails ---

        // Min body length
        if body.trim().len() < MIN_BODY_LEN {
            return Ok(ToolOutput::error(format!(
                "skill body too short ({} chars, need ≥{MIN_BODY_LEN}). \
                 A useful skill needs enough detail to be actionable — \
                 single commands or one-liners don't qualify.",
                body.trim().len()
            )));
        }

        // Hard cap
        let existing = self.count_existing_skills();
        if existing >= MAX_SKILLS {
            return Ok(ToolOutput::error(format!(
                "skill library full ({existing}/{MAX_SKILLS}). \
                 Remove an outdated skill with `opencrust skill remove <name>` before adding new ones."
            )));
        }

        // Duplicate guard
        let skill_path = self.skills_dir.join(format!("{name}.md"));
        if skill_path.exists() && !overwrite {
            return Ok(ToolOutput::error(format!(
                "skill '{name}' already exists. \
                 If you want to update it, call create_skill again with `overwrite: true`. \
                 If this is a different skill, choose a different name."
            )));
        }

        // --- Build and write SKILL.md ---
        let mut content = format!("---\nname: {name}\ndescription: {description}\n");
        // Store rationale for auditability — lets operators review why each skill was saved.
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
        // Use a unique tmp name per invocation to avoid races under parallel tests.
        let tmp = std::env::temp_dir().join(format!(
            "opencrust_skill_{name}_{}.md",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        if let Err(e) = std::fs::write(&tmp, &content) {
            return Ok(ToolOutput::error(format!(
                "failed to stage skill file: {e}"
            )));
        }

        match installer.install_from_path(&tmp) {
            Ok(skill) => {
                let _ = std::fs::remove_file(&tmp);
                let action = if overwrite && skill_path.exists() {
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
        assert!(dir.path().join("disk-cleanup.md").exists());
        assert!(out.content.contains("1/30"));

        // Rationale must be persisted in the skill file for auditability.
        let saved = std::fs::read_to_string(dir.path().join("disk-cleanup.md")).unwrap();
        assert!(
            saved.contains("rationale:"),
            "skill file should contain rationale field"
        );
        assert!(
            saved.contains("multi-step workflow"),
            "rationale content should be stored verbatim"
        );
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
                    "rationale": "useful"  // too short
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

        // Fill up to the cap
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
}
