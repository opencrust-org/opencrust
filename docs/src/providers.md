# Providers

OpenCrust supports 15 LLM providers. Four are native implementations with provider-specific APIs. The remaining eleven use the OpenAI-compatible chat completions format and are built on top of the `OpenAiProvider` with custom base URLs.

All providers support streaming responses and tool use.

## API Key Resolution

For every provider, API keys are resolved in this order:

1. **Credential vault** - `~/.opencrust/credentials/vault.json` (requires `OPENCRUST_VAULT_PASSPHRASE`)
2. **Config file** - `api_key` field under the `llm:` section in `config.yml`
3. **Environment variable** - provider-specific env var (listed below)

## Custom Base URL

All providers support custom base URLs via the `base_url` configuration field. This is useful for:

- **Proxies and gateways** - Route requests through a custom endpoint
- **Self-hosted models** - Point to your own API server
- **Regional endpoints** - Use region-specific API URLs
- **Development/testing** - Connect to local or staging environments

### Configuration

Add the `base_url` field to any provider configuration:

```yaml
llm:
  custom-openai:
    provider: openai
    model: gpt-4o
    base_url: "https://my-proxy.example.com/v1"
    api_key: sk-...
  
  remote-ollama:
    provider: ollama
    model: llama3.1
    base_url: "http://192.168.1.100:11434"
  
  custom-anthropic:
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    base_url: "https://my-anthropic-proxy.com"
    api_key: sk-ant-...
```

### URL Format

- URLs should include the protocol (`http://` or `https://`)
- Trailing slashes are automatically handled
- For OpenAI-compatible providers, the `/v1/chat/completions` path is appended automatically
- For Anthropic, the `/v1/messages` path is appended automatically
- For Ollama, the `/api/chat` path is appended automatically

### Validation

The setup wizard validates base URLs to ensure they:
- Use valid HTTP/HTTPS protocols
- Have proper URL format
- Are not empty or malformed

## Native Providers

### Anthropic Claude

Claude models with native streaming (SSE) and tool use via the Anthropic Messages API.

| Field | Value |
|-------|-------|
| Config type | `anthropic` |
| Default model | `claude-sonnet-4-5-20250929` |
| Base URL | `https://api.anthropic.com` |
| Env var | `ANTHROPIC_API_KEY` |

```yaml
llm:
  claude:
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    # api_key: sk-... (or use vault / ANTHROPIC_API_KEY env var)
```

### OpenAI

GPT models via the OpenAI Chat Completions API. Also works with Azure OpenAI or any OpenAI-compatible endpoint by overriding `base_url`.

| Field | Value |
|-------|-------|
| Config type | `openai` |
| Default model | `gpt-4o` |
| Base URL | `https://api.openai.com` |
| Env var | `OPENAI_API_KEY` |

```yaml
llm:
  gpt:
    provider: openai
    model: gpt-4o
    # base_url: https://your-azure-endpoint.openai.azure.com  # optional override
```

### Ollama

Run local models with streaming. No API key required.

| Field | Value |
|-------|-------|
| Config type | `ollama` |
| Default model | `llama3.1` |
| Base URL | `http://localhost:11434` |
| Env var | None |

```yaml
llm:
  local:
    provider: ollama
    model: llama3.1
    base_url: "http://localhost:11434"
```

### Codex OAuth

Codex models through OpenAI's ChatGPT-backed Codex Responses API, authenticated with OAuth tokens instead of an API key.

| Field | Value |
|-------|-------|
| Config type | `codex` |
| Default model | `gpt-5.3-codex` |
| Base URL | `https://chatgpt.com/backend-api/codex` |
| Env vars | `CODEX_ACCESS_TOKEN`, `CODEX_REFRESH_TOKEN`, `CODEX_ACCOUNT_ID`, `CODEX_ID_TOKEN` |

You can connect this provider directly from the webchat sidebar with `Connect with Codex`.

```yaml
llm:
  codex:
    provider: codex
    model: gpt-5.3-codex
    # Optional config keys if you are not using env vars or the vault:
    # access_token: eyJ...
    # refresh_token: ...
    # account_id: org_...
    # id_token: eyJ...
```

## OpenAI-Compatible Providers

These providers all use the OpenAI chat completions wire format. OpenCrust sends requests to their respective API endpoints using the standard `Authorization: Bearer` header.

### Sansa

Regional LLM from [sansaml.com](https://sansaml.com).

| Field | Value |
|-------|-------|
| Config type | `sansa` |
| Default model | `sansa-auto` |
| Base URL | `https://api.sansaml.com` |
| Env var | `SANSA_API_KEY` |

```yaml
llm:
  sansa:
    provider: sansa
    model: sansa-auto
```

### DeepSeek

| Field | Value |
|-------|-------|
| Config type | `deepseek` |
| Default model | `deepseek-chat` |
| Base URL | `https://api.deepseek.com` |
| Env var | `DEEPSEEK_API_KEY` |

```yaml
llm:
  deepseek:
    provider: deepseek
    model: deepseek-chat
```

### Mistral

| Field | Value |
|-------|-------|
| Config type | `mistral` |
| Default model | `mistral-large-latest` |
| Base URL | `https://api.mistral.ai` |
| Env var | `MISTRAL_API_KEY` |

```yaml
llm:
  mistral:
    provider: mistral
    model: mistral-large-latest
```

### Gemini

Google Gemini via the OpenAI-compatible endpoint.

| Field | Value |
|-------|-------|
| Config type | `gemini` |
| Default model | `gemini-2.5-flash` |
| Base URL | `https://generativelanguage.googleapis.com/v1beta/openai/` |
| Env var | `GEMINI_API_KEY` |

```yaml
llm:
  gemini:
    provider: gemini
    model: gemini-2.5-flash
```

### Falcon

TII Falcon 180B via AI71.

| Field | Value |
|-------|-------|
| Config type | `falcon` |
| Default model | `tiiuae/falcon-180b-chat` |
| Base URL | `https://api.ai71.ai/v1` |
| Env var | `FALCON_API_KEY` |

```yaml
llm:
  falcon:
    provider: falcon
    model: tiiuae/falcon-180b-chat
```

### Jais

Core42 Jais 70B.

| Field | Value |
|-------|-------|
| Config type | `jais` |
| Default model | `jais-adapted-70b-chat` |
| Base URL | `https://api.core42.ai/v1` |
| Env var | `JAIS_API_KEY` |

```yaml
llm:
  jais:
    provider: jais
    model: jais-adapted-70b-chat
```

### Qwen

Alibaba Qwen via DashScope international.

| Field | Value |
|-------|-------|
| Config type | `qwen` |
| Default model | `qwen-plus` |
| Base URL | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` |
| Env var | `QWEN_API_KEY` |

```yaml
llm:
  qwen:
    provider: qwen
    model: qwen-plus
```

### Yi

01.AI Yi Large.

| Field | Value |
|-------|-------|
| Config type | `yi` |
| Default model | `yi-large` |
| Base URL | `https://api.lingyiwanwu.com/v1` |
| Env var | `YI_API_KEY` |

```yaml
llm:
  yi:
    provider: yi
    model: yi-large
```

### Cohere

Cohere Command R Plus via the compatibility endpoint.

| Field | Value |
|-------|-------|
| Config type | `cohere` |
| Default model | `command-r-plus` |
| Base URL | `https://api.cohere.com/compatibility/v1` |
| Env var | `COHERE_API_KEY` |

```yaml
llm:
  cohere:
    provider: cohere
    model: command-r-plus
```

### MiniMax

| Field | Value |
|-------|-------|
| Config type | `minimax` |
| Default model | `MiniMax-Text-01` |
| Base URL | `https://api.minimaxi.chat/v1` |
| Env var | `MINIMAX_API_KEY` |

```yaml
llm:
  minimax:
    provider: minimax
    model: MiniMax-Text-01
```

### Moonshot

Kimi models from Moonshot AI.

| Field | Value |
|-------|-------|
| Config type | `moonshot` |
| Default model | `kimi-k2-0711-preview` |
| Base URL | `https://api.moonshot.cn/v1` |
| Env var | `MOONSHOT_API_KEY` |

```yaml
llm:
  moonshot:
    provider: moonshot
    model: kimi-k2-0711-preview
```

## Runtime Provider Switching

You can add or switch providers at runtime without restarting the daemon.

**REST API:**

```bash
# List active providers
curl http://127.0.0.1:3888/api/providers

# Add a new provider
curl -X POST http://127.0.0.1:3888/api/providers \
  -H "Content-Type: application/json" \
  -d '{"provider": "deepseek", "api_key": "sk-..."}'
```

**WebSocket:** Include an optional `provider` field in your message to route it to a specific provider:

```json
{"type": "message", "content": "Hello", "provider": "deepseek"}
```

**Webchat UI:** The sidebar has a provider dropdown and API key input. Click "Save & Activate" to register a new provider at runtime. API keys are persisted to the vault when `OPENCRUST_VAULT_PASSPHRASE` is set.

## Multiple Instances

You can configure multiple instances of the same provider type with different models or settings:

```yaml
llm:
  claude-sonnet:
    provider: anthropic
    model: claude-sonnet-4-5-20250929

  claude-haiku:
    provider: anthropic
    model: claude-haiku-4-5-20251001

  gpt4o:
    provider: openai
    model: gpt-4o

  gpt4o-mini:
    provider: openai
    model: gpt-4o-mini
```

The first configured provider is used by default. Use the `provider` field in WebSocket messages or the webchat dropdown to select a specific one.
