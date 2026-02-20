use std::sync::Arc;
use std::time::{Duration, Instant};

use serenity::all::{
    self as serenity_model, CommandInteraction, Context, CreateMessage, EditMessage, EventHandler,
    Interaction as SerenityInteraction, Message as SerenityMessage, MessageId, Ready,
};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::traits::{ChannelEvent, ChannelStatus};

use super::{DiscordOnMessageFn, commands, convert};

/// Serenity event handler that bridges Discord events into OpenCrust `ChannelEvent`s.
pub struct DiscordHandler {
    /// Broadcast sender for emitting channel events to subscribers.
    event_tx: broadcast::Sender<ChannelEvent>,

    /// The channel identifier string used in OpenCrust messages.
    channel_id: String,

    /// Guild IDs for slash command registration. Empty means global commands.
    guild_ids: Vec<u64>,

    /// Callback for processing incoming user messages.
    on_message: DiscordOnMessageFn,
}

impl DiscordHandler {
    pub fn new(
        event_tx: broadcast::Sender<ChannelEvent>,
        channel_id: String,
        guild_ids: Vec<u64>,
        on_message: DiscordOnMessageFn,
    ) -> Self {
        Self {
            event_tx,
            channel_id,
            guild_ids,
            on_message,
        }
    }

    fn emit(&self, event: ChannelEvent) {
        if let Err(e) = self.event_tx.send(event) {
            warn!("no subscribers for channel event: {e}");
        }
    }

    async fn process_message(
        &self,
        ctx: &Context,
        channel_id: serenity_model::ChannelId,
        user_id: String,
        user_name: String,
        text: String,
    ) {
        if text.trim().is_empty() {
            return;
        }

        // Keep typing indicator alive while callback/streaming is in progress.
        let typing_http = ctx.http.clone();
        let typing_channel = channel_id;
        let typing_handle = tokio::spawn(async move {
            loop {
                let _ = typing_channel.broadcast_typing(&typing_http).await;
                tokio::time::sleep(Duration::from_secs(4)).await;
            }
        });

        let (delta_tx, mut delta_rx) = mpsc::channel::<String>(64);
        let on_message = Arc::clone(&self.on_message);
        let cb_channel_id = channel_id.to_string();
        let cb_user_id = user_id.clone();
        let cb_user_name = user_name.clone();
        let cb_text = text.clone();

        let callback_handle = tokio::spawn(async move {
            on_message(
                cb_channel_id,
                cb_user_id,
                cb_user_name,
                cb_text,
                Some(delta_tx),
            )
            .await
        });

        let mut accumulated = String::new();
        let mut sent: Vec<(MessageId, String)> = Vec::new();
        let mut first_delta_at: Option<Instant> = None;
        let mut last_update = Instant::now();

        while let Some(delta) = delta_rx.recv().await {
            accumulated.push_str(&delta);
            if first_delta_at.is_none() {
                first_delta_at = Some(Instant::now());
            }

            if first_delta_at
                .map(|t| t.elapsed() >= Duration::from_secs(1))
                .unwrap_or(false)
                && last_update.elapsed() >= Duration::from_millis(1000)
            {
                if let Err(e) =
                    sync_discord_chunks(ctx, channel_id, &accumulated, &mut sent, false).await
                {
                    warn!("failed to stream Discord update: {e}");
                    break;
                }
                last_update = Instant::now();
            }
        }

        typing_handle.abort();

        let result = callback_handle
            .await
            .unwrap_or_else(|e| Err(format!("task panic: {e}")));

        match result {
            Ok(final_text) => {
                if let Err(e) =
                    sync_discord_chunks(ctx, channel_id, &final_text, &mut sent, true).await
                {
                    warn!("failed to send Discord final response: {e}");
                }
            }
            Err(e) if e == "__blocked__" => {}
            Err(e) => {
                let err_text = format!("Sorry, an error occurred: {e}");
                if let Err(send_err) =
                    sync_discord_chunks(ctx, channel_id, &err_text, &mut sent, true).await
                {
                    warn!("failed to send Discord error response: {send_err}");
                }
            }
        }
    }

    async fn process_slash_command(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        slash: commands::DiscordSlashCommand,
    ) {
        if let Err(e) = command.defer(&ctx.http).await {
            warn!("failed to defer slash command response: {e}");
            return;
        }

        let user_id = command.user.id.to_string();
        let user_name = command
            .member
            .as_ref()
            .and_then(|m| m.nick.clone())
            .unwrap_or_else(|| command.user.name.clone());
        let text = format!("/{}", slash.as_str());

        let on_message = Arc::clone(&self.on_message);
        let result = on_message(
            command.channel_id.to_string(),
            user_id,
            user_name,
            text,
            None,
        )
        .await;

        let response_text = match result {
            Ok(t) => t,
            Err(e) if e == "__blocked__" => "You are not authorized to use this bot.".to_string(),
            Err(e) => format!("Sorry, an error occurred: {e}"),
        };

        let response_text = convert::to_discord_markdown(&response_text);
        let chunks = convert::split_discord_chunks(&response_text);
        let first_chunk = chunks
            .first()
            .cloned()
            .unwrap_or_else(|| "\u{200B}".to_string());

        if let Err(e) = command
            .edit_response(
                &ctx.http,
                serenity_model::EditInteractionResponse::new().content(first_chunk),
            )
            .await
        {
            warn!("failed to edit deferred slash response: {e}");
            return;
        }

        if chunks.len() > 1 {
            for chunk in chunks.into_iter().skip(1) {
                if let Err(e) = command
                    .create_followup(
                        &ctx.http,
                        serenity_model::CreateInteractionResponseFollowup::new().content(chunk),
                    )
                    .await
                {
                    warn!("failed to create slash followup response: {e}");
                    break;
                }
            }
        }
    }
}

#[serenity::async_trait]
impl EventHandler for DiscordHandler {
    /// Fired when the bot successfully connects and is ready.
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(
            "Discord bot connected as {}#{} (guilds: {})",
            ready.user.name,
            ready
                .user
                .discriminator
                .map(|d| d.to_string())
                .unwrap_or_default(),
            ready.guilds.len()
        );

        let command_defs = commands::all_commands();
        if let Err(e) = commands::register_commands(&ctx, &self.guild_ids, &command_defs).await {
            warn!("failed to register discord slash commands: {e}");
        } else {
            info!("registered {} discord slash command(s)", command_defs.len());
        }

        self.emit(ChannelEvent::StatusChanged(ChannelStatus::Connected));
    }

    /// Fired when the bot resumes a previously interrupted gateway connection.
    async fn resume(&self, _ctx: Context, _: serenity_model::ResumedEvent) {
        info!("Discord gateway connection resumed");
        self.emit(ChannelEvent::StatusChanged(ChannelStatus::Connected));
    }

    /// Fired when a message is received in any channel the bot can see.
    async fn message(&self, ctx: Context, msg: SerenityMessage) {
        if msg.author.bot {
            return;
        }

        let opencrust_msg = convert::discord_message_to_opencrust(&msg, &self.channel_id);
        self.emit(ChannelEvent::MessageReceived(opencrust_msg));

        tracing::debug!(
            message_id = %msg.id,
            author = %msg.author.name,
            channel = %msg.channel_id,
            "received discord message"
        );

        self.process_message(
            &ctx,
            msg.channel_id,
            msg.author.id.to_string(),
            msg.author
                .global_name
                .clone()
                .unwrap_or_else(|| msg.author.name.clone()),
            msg.content.clone(),
        )
        .await;
    }

    /// Fired when a slash command interaction is created.
    async fn interaction_create(&self, ctx: Context, interaction: SerenityInteraction) {
        let SerenityInteraction::Command(command) = interaction else {
            return;
        };

        let Some(slash) = commands::DiscordSlashCommand::from_name(&command.data.name) else {
            return;
        };

        self.process_slash_command(&ctx, &command, slash).await;
    }

    /// Fired when a reaction is added to a message.
    async fn reaction_add(&self, _ctx: Context, reaction: serenity_model::Reaction) {
        let opencrust_msg = convert::reaction_to_opencrust(&reaction, &self.channel_id);

        tracing::debug!(
            emoji = ?reaction.emoji,
            message_id = %reaction.message_id,
            "received discord reaction"
        );

        self.emit(ChannelEvent::MessageReceived(opencrust_msg));
    }

    /// Fired when a thread is created.
    async fn thread_create(&self, _ctx: Context, thread: serenity_model::GuildChannel) {
        info!(
            thread_id = %thread.id,
            thread_name = %thread.name,
            "new discord thread created"
        );
    }
}

async fn sync_discord_chunks(
    ctx: &Context,
    channel_id: serenity_model::ChannelId,
    text: &str,
    sent: &mut Vec<(MessageId, String)>,
    is_final: bool,
) -> std::result::Result<(), String> {
    let formatted = convert::to_discord_markdown(text);
    let chunks = convert::split_discord_chunks(&formatted);

    for (idx, chunk) in chunks.iter().enumerate() {
        if idx < sent.len() {
            if sent[idx].1 != *chunk {
                channel_id
                    .edit_message(&ctx.http, sent[idx].0, EditMessage::new().content(chunk))
                    .await
                    .map_err(|e| format!("failed to edit Discord message: {e}"))?;
                sent[idx].1 = chunk.clone();
            }
        } else {
            let msg = channel_id
                .send_message(&ctx.http, CreateMessage::new().content(chunk))
                .await
                .map_err(|e| format!("failed to send Discord chunk: {e}"))?;
            sent.push((msg.id, chunk.clone()));
        }
    }

    if is_final && sent.len() > chunks.len() {
        for (id, _) in sent.drain(chunks.len()..) {
            let _ = channel_id.delete_message(&ctx.http, id).await;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    #[test]
    fn handler_construction() {
        let (tx, _rx) = broadcast::channel::<ChannelEvent>(16);
        let on_msg: DiscordOnMessageFn = Arc::new(|_ch, _uid, _user, _text, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let handler = DiscordHandler::new(tx, "discord".to_string(), vec![], on_msg);
        assert_eq!(handler.channel_id, "discord");
        assert!(handler.guild_ids.is_empty());
    }

    #[test]
    fn emit_with_no_subscribers_does_not_panic() {
        let (tx, _) = broadcast::channel::<ChannelEvent>(16);
        let on_msg: DiscordOnMessageFn = Arc::new(|_ch, _uid, _user, _text, _delta_tx| {
            Box::pin(async { Ok("test".to_string()) })
        });
        let handler = DiscordHandler::new(tx, "discord".to_string(), vec![], on_msg);
        handler.emit(ChannelEvent::StatusChanged(ChannelStatus::Connected));
    }
}
