use std::path::Path;

use colored::Colorize;
use opencrust_config::AppConfig;

/// Print the chat-mode banner shown when `opencrust chat` starts.
pub fn print_chat_banner(url: &str, agent_id: Option<&str>) {
    let version = env!("CARGO_PKG_VERSION");
    let width = 50usize;

    let title = format!("OpenCrust Chat v{version}");
    let title_dashes = width.saturating_sub(2 + 5 + title.len()); // 2 ╭╮, 5 "─── " + " "
    let top = format!("╭─── {title} {}╮", "─".repeat(title_dashes));
    let bottom = format!("╰{}╯", "─".repeat(width - 2));
    let inner_w = width - 4; // 4 = "│ " + " │"
    let row = |s: &str| format!("│ {:<inner_w$} │", s);
    let blank = row("");

    let o = |s: String| s.truecolor(255, 140, 0).to_string();

    println!("{}", o(top));
    println!("{}", o(blank.clone()));
    println!("{}", o(row("        _~^~^~_                 ")));
    println!("{}", o(row("    \\) /  o o  \\ (/             ")));
    println!("{}", o(row("      '_   -   _'               ")));
    println!("{}", o(row("      / '-----' \\               ")));
    println!("{}", o(blank.clone()));
    println!("{}", o(row(&format!("  Gateway  {url}"))));
    let agent_label = agent_id.unwrap_or("default");
    println!("{}", o(row(&format!("  Agent    {agent_label}"))));
    println!("{}", o(blank.clone()));
    println!("{}", o(row("  Type /help for commands")));
    println!("{}", o(blank));
    println!("{}", o(bottom));
    println!();
}

/// Print the startup banner with Ferris and config summary.
pub fn print_banner(host: &str, port: u16, config: &AppConfig, config_dir: &Path) {
    let version = env!("CARGO_PKG_VERSION");

    // Gather info
    let provider = config
        .agent
        .default_provider
        .as_deref()
        .or_else(|| config.llm.keys().next().map(|s| s.as_str()))
        .unwrap_or("none");

    let channels = if config.channels.is_empty() {
        "none".to_string()
    } else {
        let mut names: Vec<_> = config.channels.keys().cloned().collect();
        names.sort();
        names.join(", ")
    };

    let skill_count = config_dir
        .join("skills")
        .read_dir()
        .map(|rd| {
            rd.filter(|e| {
                e.as_ref()
                    .map(|e| e.path().extension().is_some_and(|x| x == "md"))
                    .unwrap_or(false)
            })
            .count()
        })
        .unwrap_or(0);
    let skills = if skill_count == 0 {
        "none".to_string()
    } else {
        format!("{skill_count} loaded")
    };

    let mcp_count = config.mcp.len();
    let mcp = if mcp_count == 0 {
        "none".to_string()
    } else {
        format!(
            "{mcp_count} server{}",
            if mcp_count == 1 { "" } else { "s" }
        )
    };

    let url = format!("http://{host}:{port}");
    let dir_display = match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => config_dir.to_string_lossy().replace(&home, "~"),
        _ => config_dir.to_string_lossy().to_string(),
    };

    // Layout
    let width = 70;
    let left_w = 33;
    let right_w = width - left_w - 3; // 3 for "│ " + "│"

    let title = format!("OpenCrust v{version}");
    let title_dashes = width - 2 - title.len() - 5; // 2 for ╭╮, 5 for "─── " + " "
    let top = format!("╭─── {title} {}╮", "─".repeat(title_dashes));
    let bottom = format!("╰{}╯", "─".repeat(width - 2));

    let row = |l: &str, r: &str| format!("│ {:<left_w$}│  {:<right_w$}│", l, r);

    let o = |s: String| s.truecolor(255, 140, 0).to_string();

    println!("{}", o(top));
    println!("{}", o(row("", "")));
    println!("{}", o(row("  Welcome to OpenCrust!", "Gateway")));
    println!("{}", o(row("", &url)));
    println!("{}", o(row("      _~^~^~_", &"─".repeat(right_w - 2))));
    println!(
        "{}",
        o(row(
            "  \\) /  o o  \\ (/",
            &format!("Provider    {provider}")
        ))
    );
    println!(
        "{}",
        o(row("    '_   -   _'", &format!("Channels    {channels}")))
    );
    println!(
        "{}",
        o(row("    / '-----' \\", &format!("Skills      {skills}")))
    );
    println!("{}", o(row("", &format!("MCP         {mcp}"))));
    println!("{}", o(row("  Rust · Personal AI", "")));
    println!(
        "{}",
        o(row(&format!("  {dir_display}"), "Press Ctrl+C to stop"))
    );
    println!("{}", o(row("", "")));
    println!("{}", o(bottom));
}
