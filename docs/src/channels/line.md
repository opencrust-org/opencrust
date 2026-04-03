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

You can also use environment variables: `LINE_CHANNEL_ACCESS_TOKEN` and `LINE_CHANNEL_SECRET`.

## Webhook Setup

1.  In the LINE Developers Console, go to **Messaging API** settings.
2.  Set the **Webhook URL** to: `https://your-domain.com/line/webhook`
3.  Enable **Use webhook**.
4.  (Optional) Disable **Auto-response messages** and **Greeting messages** in the LINE Official Account manager to avoid duplicate responses.

## Features

### Reply vs Push API
OpenCrust uses a "Reply-first" strategy:
-   **Reply API**: Used for immediate responses to user messages. This is free and does not count against your messaging limit.
-   **Push API**: Used as a fallback if the reply token expires or for proactive messages (like scheduled tasks). Note that Push messages may count toward your monthly free limit depending on your LINE plan.

### Groups and Rooms
The agent works in LINE groups and rooms. 
-   **Session Isolation**: Each group/room has its own conversation session, shared by all members.
-   **Mentioning**: Standard LINE bots do not receive a "mentioned" flag for simple text messages. By default, the agent responds to *all* messages in a group if it is added. You can configure filters in the code if needed.

### Security
All incoming requests are verified using the `X-Line-Signature` header (HMAC-SHA256) to ensure they originate from the LINE platform.

## Diagnostics

Use the `opencrust doctor` command to verify your LINE configuration:

```bash
opencrust doctor
```

It will check if the access token is valid and if the webhook endpoint is reachable.
