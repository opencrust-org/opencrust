use async_trait::async_trait;
use opencrust_common::{Error, Result};
use regex::RegexBuilder;
use std::path::{Path, PathBuf};

use super::{Tool, ToolContext, ToolOutput};

const DEFAULT_MAX_RESULTS: usize = 50;
const HARD_MAX_RESULTS: usize = 200;
const MAX_FILE_BYTES: u64 = 512 * 1024; // 512 KB per file

/// Search file contents by regex pattern, or find files by name glob.
pub struct SearchFilesTool;

impl SearchFilesTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SearchFilesTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a simple glob pattern (*, ?) into a full-match regex string.
fn glob_to_regex(glob: &str) -> String {
    let mut out = String::from("^");
    for ch in glob.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            c if ".\\ +^${}[]|()".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('$');
    out
}

/// Iterative directory walker — returns all file paths under `dir`.
async fn walk_files(root: &Path, glob_regex: Option<&regex::Regex>) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut dirs = vec![root.to_path_buf()];

    while let Some(dir) = dirs.pop() {
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };

        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let Ok(meta) = tokio::fs::metadata(&path).await else {
                continue;
            };

            if meta.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.starts_with('.') {
                    dirs.push(path);
                }
            } else if meta.is_file() {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if let Some(rx) = glob_regex {
                    if !rx.is_match(file_name) {
                        continue;
                    }
                }
                results.push(path);
            }
        }
    }

    results
}

#[async_trait]
impl Tool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }

    fn description(&self) -> &str {
        "Search file contents for a regex pattern, or find files by name glob. \
         Returns matching lines with file path and line number. \
         Use instead of running grep/find via bash."
    }

    fn system_hint(&self) -> Option<&str> {
        Some(
            "Use search_files instead of bash grep/find for searching code or text. \
             Supply a glob (e.g. \"*.rs\") to restrict which files are searched.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for in file contents"
                },
                "path": {
                    "type": "string",
                    "description": "Directory (or file) to search in (default: current working directory)"
                },
                "glob": {
                    "type": "string",
                    "description": "File name glob filter, e.g. \"*.rs\" or \"*.{ts,tsx}\" (default: all files)"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive match (default: false)"
                },
                "max_results": {
                    "type": "number",
                    "description": "Maximum number of matching lines to return (1–200, default 50)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'pattern' parameter".into()))?;

        let search_path = input
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        let glob = input.get("glob").and_then(|v| v.as_str());

        let case_sensitive = input
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let max_results = input
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).clamp(1, HARD_MAX_RESULTS))
            .unwrap_or(DEFAULT_MAX_RESULTS);

        // Compile the search regex
        let regex = RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| Error::Agent(format!("invalid regex pattern: {e}")))?;

        // Compile the glob filter regex if provided
        let glob_regex = if let Some(g) = glob {
            let glob_pattern = glob_to_regex(g);
            Some(
                RegexBuilder::new(&glob_pattern)
                    .case_insensitive(true)
                    .build()
                    .map_err(|e| Error::Agent(format!("invalid glob pattern: {e}")))?,
            )
        } else {
            None
        };

        // Collect files to search
        let meta = tokio::fs::metadata(&search_path)
            .await
            .map_err(|e| Error::Agent(format!("cannot access path: {e}")))?;

        let files = if meta.is_file() {
            vec![search_path.clone()]
        } else {
            walk_files(&search_path, glob_regex.as_ref()).await
        };

        if files.is_empty() {
            return Ok(ToolOutput::success(
                "No files found matching the given path and glob.",
            ));
        }

        let mut matches: Vec<String> = Vec::new();

        'files: for file_path in &files {
            // Skip files that are too large
            let Ok(fm) = tokio::fs::metadata(file_path).await else {
                continue;
            };
            if fm.len() > MAX_FILE_BYTES {
                continue;
            }

            let Ok(content) = tokio::fs::read_to_string(file_path).await else {
                // Skip binary / unreadable files
                continue;
            };

            let display = file_path.display().to_string();

            for (line_no, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    matches.push(format!("{}:{}: {}", display, line_no + 1, line));
                    if matches.len() >= max_results {
                        break 'files;
                    }
                }
            }
        }

        if matches.is_empty() {
            return Ok(ToolOutput::success(format!(
                "No matches found for pattern {:?} in {} file(s).",
                pattern,
                files.len()
            )));
        }

        let truncated = matches.len() >= max_results;
        let mut output = format!(
            "{} match(es) in {} file(s):\n\n",
            matches.len(),
            files.len()
        );
        output.push_str(&matches.join("\n"));
        if truncated {
            output.push_str(&format!(
                "\n\n... results truncated at {max_results} — use max_results or a more specific pattern."
            ));
        }

        Ok(ToolOutput::success(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
            allowed_tools: None,
        }
    }

    #[tokio::test]
    async fn finds_pattern_in_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello world").unwrap();
        writeln!(tmp, "goodbye world").unwrap();
        writeln!(tmp, "hello rust").unwrap();

        let tool = SearchFilesTool::new();
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "pattern": "hello",
                    "path": tmp.path().to_str().unwrap()
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("hello world"));
        assert!(output.content.contains("hello rust"));
        assert!(!output.content.contains("goodbye"));
    }

    #[tokio::test]
    async fn case_insensitive_by_default() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "Hello World").unwrap();

        let tool = SearchFilesTool::new();
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "pattern": "hello",
                    "path": tmp.path().to_str().unwrap()
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("Hello World"));
    }

    #[tokio::test]
    async fn case_sensitive_mode() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "Hello World").unwrap();
        writeln!(tmp, "hello world").unwrap();

        let tool = SearchFilesTool::new();
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "pattern": "hello",
                    "path": tmp.path().to_str().unwrap(),
                    "case_sensitive": true
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("hello world"));
        assert!(!output.content.contains("Hello World"));
    }

    #[tokio::test]
    async fn glob_filter_restricts_files() {
        let dir = TempDir::new().unwrap();
        let rs_file = dir.path().join("main.rs");
        let txt_file = dir.path().join("notes.txt");
        std::fs::write(&rs_file, "fn main() {}\n").unwrap();
        std::fs::write(&txt_file, "fn main is not here\n").unwrap();

        let tool = SearchFilesTool::new();
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "pattern": "fn main",
                    "path": dir.path().to_str().unwrap(),
                    "glob": "*.rs"
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("main.rs"));
        assert!(!output.content.contains("notes.txt"));
    }

    #[tokio::test]
    async fn no_match_returns_success_message() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "nothing here").unwrap();

        let tool = SearchFilesTool::new();
        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "pattern": "zzznomatch",
                    "path": tmp.path().to_str().unwrap()
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error);
        assert!(output.content.contains("No matches"));
    }

    #[tokio::test]
    async fn invalid_regex_returns_error() {
        let tool = SearchFilesTool::new();
        let result = tool
            .execute(&ctx(), serde_json::json!({ "pattern": "[invalid" }))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn glob_to_regex_wildcard() {
        let rx = regex::Regex::new(&glob_to_regex("*.rs")).unwrap();
        assert!(rx.is_match("main.rs"));
        assert!(rx.is_match("lib.rs"));
        assert!(!rx.is_match("main.txt"));
    }

    #[test]
    fn glob_to_regex_question_mark() {
        let rx = regex::Regex::new(&glob_to_regex("file?.txt")).unwrap();
        assert!(rx.is_match("file1.txt"));
        assert!(rx.is_match("fileA.txt"));
        assert!(!rx.is_match("file12.txt"));
    }
}
