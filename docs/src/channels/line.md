# LINE Channel Setup

OpenCrust supports the LINE Messaging API for both 1-on-1 chats and group/room conversations.

## Prerequisites

1.  A **LINE Official Account**. Create one at the [LINE Business ID](https://manager.line.biz/) portal.
2.  A **Channel** in the [LINE Developers Console](https://developers.line.biz/console/).
3.  **Channel Access Token** (long-lived) and **Channel Secret**.

## Configuration

Add the `line` channel to your `~/.opencrust/config.yml`:

```yaml
channels:
  line:
    type: line
    enabled: true
    channel_access_token: "YOUR_CHANNEL_ACCESS_TOKEN"
    channel_secret: "YOUR_CHANNEL_SECRET"
```

You can also use environment variables instead of hardcoding credentials:

| Setting               | Environment variable         |
|-----------------------|------------------------------|
| `channel_access_token`| `LINE_CHANNEL_ACCESS_TOKEN`  |
| `channel_secret`      | `LINE_CHANNEL_SECRET`        |

## Access Control

### DM policy (`dm_policy`)

Controls who can send the bot direct messages.

| Value       | Behaviour                                                           |
|-------------|---------------------------------------------------------------------|
| `open`      | Anyone can message the bot (no auth required).                      |
| `pairing`   | New users must enter a 6-digit pairing code (default).              |
| `allowlist` | Only LINE user IDs listed under `allowlist` are accepted.           |

```yaml
channels:
  line:
    type: line
    enabled: true
    channel_access_token: "YOUR_CHANNEL_ACCESS_TOKEN"
    channel_secret: "YOUR_CHANNEL_SECRET"
    dm_policy: pairing        # open | pairing | allowlist
    allowlist:
      - "Uxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"   # LINE user ID
```

### Group policy (`group_policy`)

Controls how the bot behaves in LINE groups and rooms.

| Value      | Behaviour                                                           |
|------------|---------------------------------------------------------------------|
| `open`     | Respond to every message in the group (default).                    |
| `mention`  | Respond only when the bot is @mentioned in the group.               |
| `disabled` | Ignore all group/room messages.                                     |

```yaml
channels:
  line:
    type: line
    enabled: true
    channel_access_token: "YOUR_CHANNEL_ACCESS_TOKEN"
    channel_secret: "YOUR_CHANNEL_SECRET"
    group_policy: mention     # open | mention | disabled
```

> **Note:** Mention detection uses `message.mention.mentionees` from the LINE webhook payload. The bot's own user ID is resolved automatically from `GET /v2/bot/info` at startup — no manual configuration required.

## Webhook Setup

1.  In the LINE Developers Console, go to **Messaging API** settings.
2.  Set the **Webhook URL** to: `https://your-domain.com/webhooks/line`
3.  Enable **Use webhook**.
4.  (Optional) Disable **Auto-response messages** and **Greeting messages** in the LINE Official Account manager to avoid duplicate responses.

## Features

### Reply vs Push API
OpenCrust uses a "Reply-first" strategy:
-   **Reply API**: Used for immediate responses to user messages. This is free and does not count against your messaging limit.
-   **Push API**: Used as a fallback if the reply token expires or for proactive messages (like scheduled tasks). Note that Push messages may count toward your monthly free limit depending on your LINE plan.

### Groups and Rooms
The agent works in LINE groups and rooms.
-   **Session isolation**: Each group/room has its own conversation session, shared by all members.
-   **Mention detection**: With `group_policy: mention`, the bot responds only when directly @mentioned. The bot user ID is fetched from the LINE API automatically on startup.

### Voice Responses
When `voice.auto_reply_voice` is enabled in your config, the bot synthesizes TTS audio and attempts to deliver it as a voice message. LINE requires an externally accessible CDN URL for audio delivery; if unavailable the bot falls back to a text response.

## Security
All incoming requests are verified using the `X-Line-Signature` header (HMAC-SHA256) to ensure they originate from the LINE platform.

## Diagnostics

Use the `opencrust doctor` command to verify your LINE configuration:

```bash
opencrust doctor
```

It will check if the access token is valid and if the webhook endpoint is reachable.
