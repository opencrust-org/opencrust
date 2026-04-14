//! Helper functions for resolving per-agent config overrides at request time.

/// Load all skills from `skills_dir` and return them as a flat Markdown block
/// suitable for injection into the system prompt.
///
/// Returns `None` if the directory does not exist, is empty, or contains no
/// valid skill files.
pub fn load_skills_flat_block(skills_dir: &str) -> Option<String> {
    let scanner = opencrust_skills::SkillScanner::new(skills_dir);
    let skills = scanner.discover().ok()?;
    if skills.is_empty() {
        return None;
    }
    let block = skills
        .iter()
        .map(|s| format!("### {}\n{}\n", s.frontmatter.name, s.body))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("## Agent Skills\n\n{block}"))
}
