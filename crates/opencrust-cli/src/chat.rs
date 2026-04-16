use anyhow::{Context, Result};
use colored::Colorize;
use reqwest::Client;
use std::io::{self, BufRead, Write};

use crate::banner;

struct ChatSession {
    client: Client,
    base_url: String,
    agent_id: Option<String>,
    session_id: Option<String>,
}

impl ChatSession {
    fn new(base_url: String, agent_id: Option<String>) -> Self {
        Self {
            client: Client::new(),
            base_url,
            agent_id,
            session_id: None,
        }
    }

    async fn create_session(&mut self) -> Result<()> {
        let body = serde_json::json!({ "agent_id": self.agent_id });
        let resp = self
            .client
            .post(format!("{}/api/sessions", self.base_url))
            .json(&body)
            .send()
            .await
            .context("failed to connect to gateway — is `opencrust start` running?")?;

        if !resp.status().is_success() {
            anyhow::bail!("gateway returned {}", resp.status());
        }

        let json: serde_json::Value = resp.json().await.context("invalid session response")?;
        self.session_id = json["session_id"].as_str().map(str::to_string);
        Ok(())
    }

    async fn send(&self, text: &str) -> Result<String> {
        let session_id = self.session_id.as_deref().context("no active session")?;
        let resp = self
            .client
            .post(format!(
                "{}/api/sessions/{}/messages",
                self.base_url, session_id
            ))
            .json(&serde_json::json!({ "content": text }))
            .send()
            .await
            .context("failed to send message")?;

        if !resp.status().is_success() {
            anyhow::bail!("gateway returned {}", resp.status());
        }

        let json: serde_json::Value = resp.json().await.context("invalid message response")?;
        Ok(json["content"].as_str().unwrap_or("").to_string())
    }
}

pub async fn run(base_url: String, agent_id: Option<String>) -> Result<()> {
    let mut session = ChatSession::new(base_url.clone(), agent_id.clone());

    banner::print_chat_banner(&base_url, agent_id.as_deref());

    session
        .create_session()
        .await
        .context("could not start chat session")?;

    let stdin = io::stdin();
    loop {
        print!("{} ", "you ›".cyan().bold());
        io::stdout().flush()?;

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF (Ctrl-D)
            Ok(_) => {}
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        }

        let text = line.trim();
        if text.is_empty() {
            continue;
        }

        match text {
            "/exit" | "/quit" => {
                println!("{}", "Goodbye!".dimmed());
                break;
            }
            "/help" => {
                println!("{}", "Commands:".bold());
                println!("  /exit, /quit     — end the session");
                println!("  /new             — start a fresh conversation");
                println!("  /agent <id>      — switch to a different agent");
                println!("  /clear           — clear the screen");
                println!("  /help            — show this message");
            }
            "/new" => {
                session.create_session().await?;
                println!("{}", "New session started.".dimmed());
            }
            "/clear" => {
                print!("\x1b[2J\x1b[1;1H");
                io::stdout().flush()?;
            }
            _ if text.starts_with("/agent ") => {
                let id = text[7..].trim().to_string();
                if id.is_empty() {
                    println!("{}", "Usage: /agent <agent-id>".yellow());
                } else {
                    session.agent_id = Some(id.clone());
                    session.create_session().await?;
                    println!("{}", format!("Switched to agent: {id}").dimmed());
                }
            }
            _ if text.starts_with('/') => {
                println!(
                    "{}",
                    format!("Unknown command: {text}  (type /help for help)").yellow()
                );
            }
            _ => match session.send(text).await {
                Ok(reply) => {
                    println!("{} {}", "bot ›".green().bold(), reply);
                    println!();
                }
                Err(e) => {
                    println!("{} {e}", "error ›".red().bold());
                }
            },
        }
    }

    Ok(())
}
