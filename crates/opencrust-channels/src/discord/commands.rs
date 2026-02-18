use opencrust_common::Result;
use serenity::all::{Command, Context, CreateCommand, GuildId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordSlashCommand {
    Start,
    Help,
    Clear,
    Pair,
    Users,
}

impl DiscordSlashCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Help => "help",
            Self::Clear => "clear",
            Self::Pair => "pair",
            Self::Users => "users",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "start" => Some(Self::Start),
            "help" => Some(Self::Help),
            "clear" => Some(Self::Clear),
            "pair" => Some(Self::Pair),
            "users" => Some(Self::Users),
            _ => None,
        }
    }
}

pub fn all_commands() -> Vec<CreateCommand> {
    vec![
        CreateCommand::new("start").description("Initialize the bot and access flow"),
        CreateCommand::new("help").description("Show available OpenCrust commands"),
        CreateCommand::new("clear").description("Clear conversation history for this thread/DM"),
        CreateCommand::new("pair").description("Generate a pairing code (owner only)"),
        CreateCommand::new("users").description("List allowed users (owner only)"),
    ]
}

/// Register slash commands with Discord for the given guild IDs.
///
/// If `guild_ids` is empty, commands are registered globally.
/// If `guild_ids` is provided, commands are registered per-guild.
pub async fn register_commands(
    ctx: &Context,
    guild_ids: &[u64],
    commands: &[CreateCommand],
) -> Result<()> {
    if guild_ids.is_empty() {
        tracing::info!("registering {} slash commands globally", commands.len());
        Command::set_global_commands(&ctx.http, commands.to_vec())
            .await
            .map_err(|e| {
                opencrust_common::Error::Channel(format!("command registration failed: {e}"))
            })?;
    } else {
        for &guild_id in guild_ids {
            tracing::info!(
                "registering {} slash commands for guild {}",
                commands.len(),
                guild_id
            );
            GuildId::new(guild_id)
                .set_commands(&ctx.http, commands.to_vec())
                .await
                .map_err(|e| {
                    opencrust_common::Error::Channel(format!(
                        "guild command registration failed for {guild_id}: {e}"
                    ))
                })?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_commands_includes_expected_names() {
        let commands = all_commands();
        assert_eq!(commands.len(), 5);
    }

    #[test]
    fn from_name_maps_known_commands() {
        assert_eq!(
            DiscordSlashCommand::from_name("start"),
            Some(DiscordSlashCommand::Start)
        );
        assert_eq!(DiscordSlashCommand::from_name("nope"), None);
    }
}
