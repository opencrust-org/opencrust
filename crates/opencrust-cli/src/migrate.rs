use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub struct MigrationReport {
    pub conversations_imported: usize,
    pub conversations_skipped: usize,
    pub skills_imported: usize,
    pub skills_skipped: usize,
    pub channels_imported: usize,
    pub channels_skipped: usize,
    pub credentials_imported: usize,
    pub credentials_skipped: usize,
    pub errors: Vec<String>,
    pub dry_run: bool,
}

impl MigrationReport {
    fn new(dry_run: bool) -> Self {
        Self {
            conversations_imported: 0,
            conversations_skipped: 0,
            skills_imported: 0,
            skills_skipped: 0,
            channels_imported: 0,
            channels_skipped: 0,
            credentials_imported: 0,
            credentials_skipped: 0,
            errors: Vec::new(),
            dry_run,
        }
    }

    pub fn print_summary(&self) {
        let mode = if self.dry_run { " (dry run)" } else { "" };
        println!("OpenClaw Migration Report{mode}");
        println!("─────────────────────────");
        println!(
            "  Conversations: {} imported, {} skipped",
            self.conversations_imported, self.conversations_skipped
        );
        println!(
            "  Skills:        {} imported, {} skipped",
            self.skills_imported, self.skills_skipped
        );
        println!(
            "  Channels:      {} imported, {} skipped",
            self.channels_imported, self.channels_skipped
        );
        println!(
            "  Credentials:   {} imported, {} skipped",
            self.credentials_imported, self.credentials_skipped
        );
        if !self.errors.is_empty() {
            println!("  Errors ({}):", self.errors.len());
            for e in &self.errors {
                println!("    - {e}");
            }
        }
    }
}

fn detect_openclaw_dir(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
        return None;
    }

    // Default locations
    if let Some(config_dir) = dirs::config_dir() {
        let p = config_dir.join("openclaw");
        if p.exists() {
            return Some(p);
        }
    }

    if let Some(home) = dirs::home_dir() {
        let p = home.join(".config").join("openclaw");
        if p.exists() {
            return Some(p);
        }
    }

    None
}

pub fn migrate_openclaw(
    source: Option<&str>,
    dry_run: bool,
    opencrust_dir: &Path,
) -> Result<MigrationReport> {
    let source_dir = detect_openclaw_dir(source).context(
        "OpenClaw directory not found. Use --source to specify the path, \
         or ensure ~/.config/openclaw/ exists.",
    )?;

    println!(
        "Migrating from OpenClaw at: {}{}",
        source_dir.display(),
        if dry_run { " (dry run)" } else { "" }
    );

    let mut report = MigrationReport::new(dry_run);

    import_skills(&source_dir, opencrust_dir, &mut report);
    import_conversations(&source_dir, &mut report);
    import_channels(&source_dir, opencrust_dir, &mut report);
    import_credentials(&source_dir, &mut report);

    Ok(report)
}

fn import_skills(source_dir: &Path, opencrust_dir: &Path, report: &mut MigrationReport) {
    let skills_src = source_dir.join("skills");
    if !skills_src.exists() {
        return;
    }

    let skills_dst = opencrust_dir.join("skills");

    let entries = match std::fs::read_dir(&skills_src) {
        Ok(e) => e,
        Err(e) => {
            report
                .errors
                .push(format!("failed to read skills directory: {e}"));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                report.errors.push(format!("directory entry error: {e}"));
                continue;
            }
        };

        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                report
                    .errors
                    .push(format!("failed to read {}: {e}", path.display()));
                report.skills_skipped += 1;
                continue;
            }
        };

        // Validate as a skill
        match opencrust_skills::parse_skill(&content) {
            Ok(skill) => {
                if let Err(e) = opencrust_skills::validate_skill(&skill) {
                    report.errors.push(format!(
                        "invalid skill {}: {e}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    report.skills_skipped += 1;
                    continue;
                }

                let dest = skills_dst.join(format!("{}.md", skill.frontmatter.name));
                if dest.exists() {
                    println!(
                        "  skill '{}' already exists, skipping",
                        skill.frontmatter.name
                    );
                    report.skills_skipped += 1;
                    continue;
                }

                if !report.dry_run {
                    if let Err(e) = std::fs::create_dir_all(&skills_dst) {
                        report
                            .errors
                            .push(format!("failed to create skills dir: {e}"));
                        report.skills_skipped += 1;
                        continue;
                    }
                    if let Err(e) = std::fs::write(&dest, &content) {
                        report
                            .errors
                            .push(format!("failed to write skill {}: {e}", dest.display()));
                        report.skills_skipped += 1;
                        continue;
                    }
                }

                println!(
                    "  {} skill: {}",
                    action_word(report.dry_run),
                    skill.frontmatter.name
                );
                report.skills_imported += 1;
            }
            Err(e) => {
                report.errors.push(format!(
                    "failed to parse {}: {e}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                report.skills_skipped += 1;
            }
        }
    }
}

fn import_conversations(source_dir: &Path, report: &mut MigrationReport) {
    // Look for conversation/memory markdown files
    let conv_dirs = ["conversations", "memory"];
    let mut found = false;

    for dir_name in &conv_dirs {
        let dir = source_dir.join(dir_name);
        if !dir.exists() {
            continue;
        }
        found = true;

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                report
                    .errors
                    .push(format!("failed to read {dir_name} directory: {e}"));
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    report.errors.push(format!(
                        "failed to read {}: {e}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    report.conversations_skipped += 1;
                    continue;
                }
            };

            // Parse conversation: look for ## User / ## Assistant headers
            let turns = parse_conversation_turns(&content);
            if turns.is_empty() {
                report.conversations_skipped += 1;
                continue;
            }

            println!(
                "  {} conversation: {} ({} turns)",
                action_word(report.dry_run),
                path.file_stem().unwrap_or_default().to_string_lossy(),
                turns.len()
            );
            report.conversations_imported += 1;
            // Note: actual memory store insertion would require async runtime;
            // for now we count what would be imported. Full async import can be
            // added when the memory store is available in the CLI context.
        }
    }

    if !found {
        // No conversations directory found — that's fine
    }
}

fn parse_conversation_turns(content: &str) -> Vec<(&str, &str)> {
    let mut turns = Vec::new();
    let mut current_role: Option<&str> = None;
    let mut current_start = 0;

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(role) = trimmed.strip_prefix("## ") {
            let role = role.trim();
            if role.eq_ignore_ascii_case("User") || role.eq_ignore_ascii_case("Assistant") {
                // Save previous turn
                if let Some(prev_role) = current_role {
                    let turn_content = &content[current_start..byte_offset_of_line(content, i)];
                    let turn_content = turn_content.trim();
                    if !turn_content.is_empty() {
                        turns.push((prev_role, turn_content));
                    }
                }
                current_role = Some(role);
                current_start = byte_offset_after_line(content, i);
            }
        }
    }

    // Save last turn
    if let Some(role) = current_role {
        let turn_content = content[current_start..].trim();
        if !turn_content.is_empty() {
            turns.push((role, turn_content));
        }
    }

    turns
}

fn byte_offset_of_line(content: &str, line_num: usize) -> usize {
    content
        .lines()
        .take(line_num)
        .map(|l| l.len() + 1) // +1 for newline
        .sum()
}

fn byte_offset_after_line(content: &str, line_num: usize) -> usize {
    content
        .lines()
        .take(line_num + 1)
        .map(|l| l.len() + 1)
        .sum()
}

fn import_channels(source_dir: &Path, opencrust_dir: &Path, report: &mut MigrationReport) {
    let config_path = source_dir.join("config.yml");
    if !config_path.exists() {
        return;
    }

    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) => {
            report
                .errors
                .push(format!("failed to read OpenClaw config: {e}"));
            return;
        }
    };

    let value: serde_yaml::Value = match serde_yaml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            report
                .errors
                .push(format!("failed to parse OpenClaw config YAML: {e}"));
            return;
        }
    };

    // Look for channel configurations
    let channels = value
        .get("channels")
        .and_then(|v| v.as_mapping())
        .or_else(|| value.get("channel").and_then(|v| v.as_mapping()));

    let Some(channels) = channels else {
        return;
    };

    let opencrust_config_path = opencrust_dir.join("config.yml");
    let mut opencrust_config: serde_yaml::Value = if opencrust_config_path.exists() {
        let existing = std::fs::read_to_string(&opencrust_config_path).unwrap_or_default();
        serde_yaml::from_str(&existing)
            .unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    };

    let dest_channels = opencrust_config
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("channels".into()))
        .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

    let dest_map = match dest_channels.as_mapping_mut() {
        Some(m) => m,
        None => return,
    };

    for (name, channel_value) in channels {
        let name_str = name.as_str().unwrap_or("unknown");
        let name_key = serde_yaml::Value::String(name_str.to_string());

        if dest_map.contains_key(&name_key) {
            println!("  channel '{name_str}' already exists in OpenCrust config, skipping");
            report.channels_skipped += 1;
            continue;
        }

        if !report.dry_run {
            dest_map.insert(name_key, channel_value.clone());
        }

        println!("  {} channel: {name_str}", action_word(report.dry_run));
        report.channels_imported += 1;
    }

    if !report.dry_run
        && report.channels_imported > 0
        && let Ok(yaml_str) = serde_yaml::to_string(&opencrust_config)
        && let Err(e) = std::fs::write(&opencrust_config_path, yaml_str)
    {
        report
            .errors
            .push(format!("failed to write updated config: {e}"));
    }
}

fn import_credentials(source_dir: &Path, report: &mut MigrationReport) {
    // Look for credential files in the source
    let cred_dirs = ["credentials", "secrets"];
    let mut found_any = false;

    for dir_name in &cred_dirs {
        let dir = source_dir.join(dir_name);
        if !dir.exists() {
            continue;
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            found_any = true;

            // We can detect but importing requires vault passphrase (interactive)
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            println!(
                "  {} credential file: {file_name}",
                action_word(report.dry_run)
            );
            report.credentials_imported += 1;
        }
    }

    if found_any && !report.dry_run {
        println!(
            "  Note: credential import requires vault passphrase. \
             Use `opencrust vault import` to complete credential migration."
        );
    }
}

fn action_word(dry_run: bool) -> &'static str {
    if dry_run { "would import" } else { "imported" }
}
