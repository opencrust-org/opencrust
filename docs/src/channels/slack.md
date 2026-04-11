# Slack Channel Setup

OpenCrust connects to Slack via **Socket Mode** - no public URL or webhook needed. The bot receives messages over a WebSocket and responds with streaming edits.

## Prerequisites

1. A **Slack workspace** where you have permission to install apps.
2. A **Slack app** created at [api.slack.com/apps](https://api.slack.com/apps).

## Step-by-step Setup

### 1. Create a Slack App

1. Go to [api.slack.com/apps](https://api.slack.com/apps) and click **Create New App**.
2. Choose **From scratch**.
3. Name your app (e.g. "OpenCrust") and select your workspace.

### 2. Enable Socket Mode

1. In the left sidebar, go to **Socket Mode**.
2. Toggle **Enable Socket Mode** on.
3. You will be prompted to create an **app-level token**. Name it anything (e.g. "opencrust-socket") and add the `connections:write` scope.
4. Copy the token - it starts with `xapp-`. This is your **App Token**.

### 3. Subscribe to Events

1. In the left sidebar, go to **Event Subscriptions**.
2. Toggle **Enable Events** on.
3. Under **Subscribe to bot events**, add:
   - `message.im` - messages in direct messages
   - `message.channels` - messages in public channels (if you want group support)
   - `message.groups` - messages in private channels (if you want group support)

### 4. Set OAuth Scopes

1. In the left sidebar, go to **OAuth & Permissions**.
2. Under **Bot Token Scopes**, add:
   - `chat:write` - send messages
   - `files:read` - download shared files (needed for document ingestion)
   - `users:read` - look up user info (optional, for display names)

### 5. Install to Workspace

1. In the left sidebar, go to **Install App**.
2. Click **Install to Workspace** and authorize.
3. Copy the **Bot User OAuth Token** - it starts with `xoxb-`. This is your **Bot Token**.

## Configuration

Add the `slack` channel to your `~/.opencrust/config.yml`:

```yaml
channels:
  slack:
    type: slack
    enabled: true
    bot_token: "xoxb-your-bot-token"
    app_token: "xapp-your-app-token"
```

You can also use environment variables: `SLACK_BOT_TOKEN` and `SLACK_APP_TOKEN`.

### Using the Setup Wizard

Run `opencrust init` and select **Slack** when prompted for channels. The wizard will ask for both tokens and validate the bot token against the Slack API before saving.

## Features

### Streaming Responses
The bot posts an initial message and edits it as the LLM streams tokens, giving a real-time typing effect.

### Groups and Channels
The bot can operate in public and private channels. In group contexts:
- It responds to all messages by default.
- Session IDs are scoped per channel (`slack-C12345`), so each channel has its own conversation history.
- DMs use the same session scoping (`slack-D12345`).

### Document Ingestion
Users can share files in Slack and use `!ingest` to add them to the bot's memory:
1. Share a file in a message - the bot will download it and prompt you.
2. Send `!ingest` to ingest the pending file.
3. Or share a file with "ingest" in the caption for immediate ingestion.

Files are capped at 10 MiB.

### Security
- **Allowlist/Pairing**: Configure `dm_policy` and `group_policy` per channel to control who can interact with the bot.
- **Rate limiting**: Per-user rate limits and token budgets apply.

## Diagnostics

Use the `opencrust doctor` command to verify your Slack configuration:

```bash
opencrust doctor
```

It will test the bot token against `auth.test` and confirm your workspace connection.
