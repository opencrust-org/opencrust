# Integrations

## Google OAuth

OpenCrust supports Google OAuth for Gmail and other Google services.

### Prerequisites

1. **Gateway API key** - All Google integration endpoints require authentication. Set one of:
   - `OPENCRUST_GATEWAY_API_KEY` environment variable, or
   - `gateway.api_key` in `~/.opencrust/config.yml`, or
   - `OPENCRUST_GATEWAY_API_KEY` in the vault

   Without this, all `/api/integrations/google/*` endpoints return **403 Forbidden**.

2. **Google OAuth credentials** - Create a project in the [Google Cloud Console](https://console.cloud.google.com/), enable the APIs you need, and create OAuth 2.0 credentials.

### Configuration

```yaml
# ~/.opencrust/config.yml
gateway:
  host: "127.0.0.1"
  port: 3888
  api_key: "your-secret-key"
```

### Gmail send scope

By default, the Gmail send scope is **not** requested during OAuth. To enable sending emails, set:

```bash
export OPENCRUST_GOOGLE_ENABLE_GMAIL_SEND_SCOPE=true
```

### Endpoints

All endpoints under `/api/integrations/google/` require the gateway API key in the `Authorization` header (except the OAuth callback, which is unauthenticated so Google can redirect to it).
