use opencrust_db::{RepeatedToolSequence, TrajectoryStore};
use opencrust_skills::{SkillDefinition, SkillScanner};
use std::path::Path;
use std::sync::Arc;
use tracing::warn;

/// A suggested skill derived from a repeated tool sequence in the trajectory log.
#[derive(Debug, Clone)]
pub struct SkillSuggestion {
    /// Human-readable sequence fingerprint, e.g. "web_search → summarize".
    pub fingerprint: String,
    /// Ordered tool names that make up the sequence.
    pub tools: Vec<String>,
    /// Number of turns where this sequence was observed.
    pub occurrences: usize,
    /// True if an existing skill already appears to cover this sequence.
    pub already_covered: bool,
    /// Name of the existing skill that covers it, if any.
    pub covered_by: Option<String>,
}

/// Analyse trajectory data against existing skills and return suggestions for
/// new skills that would codify frequently-repeated tool workflows.
pub fn suggest_from_trajectories(
    trajectory_store: &Arc<TrajectoryStore>,
    skills_dir: &Path,
    min_occurrences: usize,
) -> Vec<SkillSuggestion> {
    let sequences = match trajectory_store.find_repeated_tool_sequences(min_occurrences) {
        Ok(s) => s,
        Err(e) => {
            warn!("skill suggester: trajectory query failed: {e}");
            return Vec::new();
        }
    };

    if sequences.is_empty() {
        return Vec::new();
    }

    let existing_skills = load_existing_skills(skills_dir);

    sequences
        .into_iter()
        .map(|seq| {
            let (already_covered, covered_by) = check_coverage(&seq, &existing_skills);
            SkillSuggestion {
                fingerprint: seq.fingerprint,
                tools: seq.tools,
                occurrences: seq.occurrences,
                already_covered,
                covered_by,
            }
        })
        .collect()
}

fn load_existing_skills(skills_dir: &Path) -> Vec<SkillDefinition> {
    if !skills_dir.exists() {
        return Vec::new();
    }
    match SkillScanner::new(skills_dir).discover() {
        Ok(skills) => skills,
        Err(e) => {
            warn!("skill suggester: failed to scan skills directory: {e}");
            Vec::new()
        }
    }
}

/// Check if any existing skill already covers the given tool sequence.
///
/// Coverage is determined by checking whether the skill's name, description,
/// or triggers mention any of the tool names in the sequence. This is a
/// heuristic — it avoids duplicate suggestions for workflows that are already
/// captured as skills.
fn check_coverage(
    seq: &RepeatedToolSequence,
    skills: &[SkillDefinition],
) -> (bool, Option<String>) {
    for skill in skills {
        let haystack = format!(
            "{} {} {}",
            skill.frontmatter.name,
            skill.frontmatter.description,
            skill.frontmatter.triggers.join(" "),
        )
        .to_lowercase();

        let matches = seq
            .tools
            .iter()
            .filter(|tool| {
                haystack.contains(tool.replace('_', " ").as_str())
                    || haystack.contains(tool.as_str())
            })
            .count();

        // If at least half the tools in the sequence are mentioned in the skill → covered.
        if matches * 2 >= seq.tools.len() {
            return (true, Some(skill.frontmatter.name.clone()));
        }
    }
    (false, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_trajectory_store() -> Arc<TrajectoryStore> {
        Arc::new(TrajectoryStore::in_memory().expect("in-memory store"))
    }

    fn log_sequence(store: &TrajectoryStore, session: &str, turn: u32, tools: &[&str]) {
        for tool in tools {
            store.log_tool_call(session, turn, tool, "{}").unwrap();
            store
                .log_tool_result(session, turn, tool, "out", 10)
                .unwrap();
        }
        store.log_turn_end(session, turn, "done", 0).unwrap();
    }

    #[test]
    fn returns_empty_when_no_repeated_sequences() {
        let store = make_trajectory_store();
        log_sequence(&store, "s1", 0, &["web_search", "summarize"]);
        // Only 1 occurrence → below default min_occurrences=3
        let suggestions = suggest_from_trajectories(&store, Path::new("/nonexistent"), 3);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn returns_suggestion_when_sequence_repeated() {
        let store = make_trajectory_store();
        for i in 0..3u32 {
            log_sequence(&store, "s1", i, &["web_search", "doc_search"]);
        }
        let suggestions = suggest_from_trajectories(&store, Path::new("/nonexistent"), 3);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].fingerprint, "web_search → doc_search");
        assert_eq!(suggestions[0].occurrences, 3);
        assert!(!suggestions[0].already_covered);
    }

    #[test]
    fn not_covered_when_skills_dir_missing() {
        let store = make_trajectory_store();
        for i in 0..3u32 {
            log_sequence(&store, "s1", i, &["web_search", "summarize"]);
        }
        let suggestions = suggest_from_trajectories(&store, Path::new("/nonexistent/skills"), 3);
        assert_eq!(suggestions.len(), 1);
        assert!(!suggestions[0].already_covered);
        assert!(suggestions[0].covered_by.is_none());
    }

    /// End-to-end: trajectory → pattern detection → skill suggestion → coverage check
    /// with a real skill file on disk that partially covers the tool sequence.
    #[test]
    fn covered_when_skill_file_mentions_tools() {
        let store = make_trajectory_store();
        // Log "web_search → summarize" 3 times across different sessions
        for i in 0..3u32 {
            log_sequence(
                &store,
                &format!("session-{i}"),
                0,
                &["web_search", "summarize"],
            );
        }

        // Write a real skill file that mentions both tools
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_path = dir.path().join("web-summarise.md");
        std::fs::write(
            &skill_path,
            r#"---
name: web-summarise
description: Search the web and summarise the results using web_search and summarize tools
triggers:
  - research
  - summarise
---
Use web_search to find relevant pages, then summarize the content.
"#,
        )
        .unwrap();

        let suggestions = suggest_from_trajectories(&store, dir.path(), 3);

        assert_eq!(suggestions.len(), 1);
        assert!(
            suggestions[0].already_covered,
            "should detect that web-summarise skill covers this sequence"
        );
        assert_eq!(suggestions[0].covered_by.as_deref(), Some("web-summarise"));
    }

    /// End-to-end: unrelated skill does NOT suppress the suggestion.
    #[test]
    fn not_covered_when_skill_mentions_different_tools() {
        let store = make_trajectory_store();
        for i in 0..3u32 {
            log_sequence(&store, &format!("s-{i}"), 0, &["bash", "file_write"]);
        }

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("search-skill.md"),
            r#"---
name: search-skill
description: Runs web_search and doc_search to answer questions
triggers: []
---
"#,
        )
        .unwrap();

        let suggestions = suggest_from_trajectories(&store, dir.path(), 3);

        assert_eq!(suggestions.len(), 1);
        assert!(
            !suggestions[0].already_covered,
            "unrelated skill should not suppress suggestion"
        );
    }
}
