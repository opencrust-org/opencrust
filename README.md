<p align="center">
  <img src="assets/logo.png" alt="OpenCrust" width="280" />
</p>

<h1 align="center">OpenCrust</h1>

<p align="center">
  <strong>The secure, lightweight open-source AI agent framework.</strong>
</p>

<p align="center">
  <a href="https://github.com/opencrust-org/opencrust/actions"><img src="https://github.com/opencrust-org/opencrust/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/opencrust-org/opencrust/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://github.com/opencrust-org/opencrust/stargazers"><img src="https://img.shields.io/github/stars/opencrust-org/opencrust?style=flat" alt="Stars"></a>
  <a href="https://github.com/opencrust-org/opencrust/issues"><img src="https://img.shields.io/github/issues/opencrust-org/opencrust" alt="Issues"></a>
  <a href="https://github.com/opencrust-org/opencrust/issues?q=label%3Agood-first-issue+is%3Aopen"><img src="https://img.shields.io/github/issues/opencrust-org/opencrust/good-first-issue?color=7057ff&label=good%20first%20issues" alt="Good First Issues"></a>
  <a href="https://discord.gg/97jTJEUz4"><img src="https://img.shields.io/badge/discord-join-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
</p>

<p align="center">
  🇺🇸 <strong>English</strong> &middot;
  <a href="i18n/README.th.md">🇹🇭 ไทย</a> &middot;
  <a href="i18n/README.zh.md">🇨🇳 简体中文</a> &middot;
  <a href="i18n/README.hi.md">🇮🇳 हिन्दी</a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &middot;
  <a href="#why-opencrust">Why OpenCrust?</a> &middot;
  <a href="#features">Features</a> &middot;
  <a href="#security">Security</a> &middot;
  <a href="#architecture">Architecture</a> &middot;
  <a href="#migrating-from-openclaw">Migrate from OpenClaw</a> &middot;
  <a href="#contributing">Contributing</a>
</p>

---

A single 16 MB binary that runs your AI agents across Telegram, Discord, Slack, WhatsApp, WhatsApp Web, LINE, WeChat, iMessage and MQTT - with encrypted credential storage, config hot-reload, and 13 MB of RAM at idle. Built in Rust for the security and reliability that AI agents demand.

## Quick Start

```bash
# Install (Linux, macOS)
curl -fsSL https://raw.githubusercontent.com/opencrust-org/opencrust/main/install.sh | sh

# Interactive setup - pick your LLM provider and channels
opencrust init

# Start - on first message, the agent will introduce itself and learn your preferences
opencrust start

# Diagnose configuration, connectivity, and database health
opencrust doctor
```

<details>
<summary>Build from source</summary>

```bash
# Requires Rust 1.85+
cargo build --release
./target/release/opencrust init
./target/release/opencrust start

# Optional: include WASM plugin support
cargo build --release --features plugins
```
</details>

Pre-compiled binaries for Linux (x86_64, aarch64), macOS (Intel, Apple Silicon), and Windows (x86_64) are available on [GitHub Releases](https://github.com/opencrust-org/opencrust/releases).

## Why OpenCrust?

### vs OpenClaw, ZeroClaw, and other AI agent frameworks

| | **OpenCrust** | **OpenClaw** (Node.js) | **ZeroClaw** (Rust) |
|---|---|---|---|
| **Binary size** | 16 MB | ~1.2 GB (with node_modules) | ~25 MB |
| **Memory at idle** | 13 MB | ~388 MB | ~20 MB |
| **Cold start** | 3 ms | 13.9 s | ~50 ms |
| **Credential storage** | AES-256-GCM encrypted vault | Plaintext config file | Plaintext config file |
| **Auth default** | Enabled (WebSocket pairing) | Disabled by default | Disabled by default |
| **Scheduling** | Cron, interval, one-shot | Yes | No |
| **Multi-agent routing** | Yes (named agents) | Yes (agentId) | No |
| **Session orchestration** | Yes | Yes | No |
| **MCP support** | Stdio + HTTP | Stdio + HTTP | Stdio |
| **Channels** | 9 | 6+ | 4 |
| **LLM providers** | 15 | 10+ | 22+ |
| **Pre-compiled binaries** | Yes | N/A (Node.js) | Build from source |
| **Config hot-reload** | Yes | No | No |
| **WASM plugin system** | Optional (sandboxed) | No | No |
| **Self-update** | Yes (`opencrust update`) | npm | Build from source |

*Benchmarks measured on a 1 vCPU, 1 GB RAM DigitalOcean droplet.*

## Security

OpenCrust is built for the security requirements of always-on AI agents that access private data and communicate externally.

- **Encrypted credential vault** - API keys and tokens stored with AES-256-GCM encryption at `~/.opencrust/credentials/vault.json`. Never plaintext on disk.
- **Authentication by default** - WebSocket gateway requires pairing codes. No unauthenticated access out of the box.
- **Per-channel authorization policies** - DM policies (open, pairing, allowlist) and group policies (open, mention-only, disabled) per channel. Unauthorized messages are silently dropped.
- **Prompt injection detection** - input validation and sanitization before content reaches the LLM.
- **Rate limiting** - per-user sliding-window rate limits with configurable cooldown to prevent abuse.
- **Token budgets** - per-session, daily, and monthly token caps to control LLM cost per user.
- **Tool allowlists** - restrict which tools an agent may call per session, with a per-session call budget cap.
- **Log secret redaction** - API keys and tokens automatically redacted from log output.
- **WASM sandboxing** - optional plugin sandbox via WebAssembly runtime with controlled host access (compile with `--features plugins`).
- **Localhost-only binding** - gateway binds to `127.0.0.1` by default, not `0.0.0.0`.

## Features

### LLM Providers

**Native providers:**

- **Anthropic Claude** - streaming (SSE), tool use
- **OpenAI** - GPT-4o, Azure, any OpenAI-compatible endpoint via `base_url`
- **Ollama** - local models with streaming

**OpenAI-compatible providers:**

- **Sansa** - regional LLM via [sansaml.com](https://sansaml.com)
- **DeepSeek** - DeepSeek Chat
- **Mistral** - Mistral Large
- **Gemini** - Google Gemini via OpenAI-compatible API
- **Falcon** - TII Falcon 180B (AI71)
- **Jais** - Core42 Jais 70B
- **Qwen** - Alibaba Qwen Plus
- **Yi** - 01.AI Yi Large
- **Cohere** - Command R Plus
- **MiniMax** - MiniMax Text 01
- **Moonshot** - Kimi K2
- **vLLM** - self-hosted models via vLLM's OpenAI-compatible server

### Voice I/O
- **TTS (Text-to-Speech)** — Kokoro (self-hosted via kokoro-fastapi), OpenAI TTS (`tts-1`, `tts-1-hd`), any OpenAI-compatible endpoint
- **STT (Speech-to-Text)** — local Whisper (faster-whisper-server), OpenAI Whisper API
- `auto_reply_voice: true` synthesizes every text response as audio automatically
- `tts_max_chars` limits synthesis length; long responses are truncated with a warning
- Per-channel delivery: Discord (file attachment), WeChat (Customer Service voice API), Telegram/LINE (native audio), Slack (text fallback)

### Channels
- **Telegram** - streaming responses, MarkdownV2, bot commands, typing indicators, user allowlist with pairing codes, photo/vision support, voice messages (Whisper STT), TTS auto-reply, document/file handling
- **Discord** - slash commands, event-driven message handling, session management, voice responses (TTS file attachment)
- **Slack** - Socket Mode, streaming responses, allowlist/pairing
- **WhatsApp** - Meta Cloud API webhooks, allowlist/pairing
- **WhatsApp Web** - QR code pairing via Baileys Node.js sidecar, no Meta Business account required, auth state persistence
- **iMessage** - macOS native via chat.db polling, group chats, AppleScript sending ([setup guide](docs/src/channels/imessage.md))
- **LINE** - Messaging API webhooks, reply/push fallback, group/room support, allowlist/pairing, voice responses (TTS, falls back to text)
- **WeChat** - Official Account Platform webhooks, SHA-1 signature verification, synchronous XML reply, image/voice/video/location dispatch, Customer Service API push, voice responses (TTS), allowlist/pairing
- **MQTT** - native broker client (Mosquitto, EMQX, HiveMQ), Mode A (plain text, one session per channel) and Mode B (JSON `{"user_id","text"}`, one session per device), auto-detection, exponential backoff reconnect, QoS 0/1/2, optional TLS (`mqtts://`)

### MCP (Model Context Protocol)
- Connect any MCP-compatible server (filesystem, GitHub, databases, web search)
- Stdio and HTTP (Streamable HTTP) transport
- Tools appear as native agent tools with namespaced names (`server_tool`)
- Resource tool - LLM can list and read MCP server resources on demand
- Server instructions captured from handshake and appended to system prompt
- Health monitor with 30s ping and auto-reconnect
- Configure in `config.yml` or `~/.opencrust/mcp.json` (Claude Desktop compatible)
- CLI: `opencrust mcp list`, `opencrust mcp inspect <name>`, `opencrust mcp resources <name>`, `opencrust mcp prompts <name>`

### Personality (DNA)
- On first message, the agent introduces itself and asks a few questions to learn your preferences
- Writes `~/.opencrust/dna.md` with your name, communication style, guidelines, and the bot's own identity
- No config files to edit, no wizard sections to fill out - just a conversation
- Hot-reloads on edit - change `dna.md` and the agent adapts immediately
- Migrating from OpenClaw? `opencrust migrate openclaw` imports your existing `SOUL.md`

### Agent Runtime
- Tool execution loop - bash, file_read, file_write, web_fetch, web_search (Brave or Google Custom Search), doc_search, schedule_heartbeat, cancel_heartbeat, list_heartbeats, mcp_resources (up to 10 iterations)
- SQLite-backed conversation memory with vector search (sqlite-vec + Cohere embeddings)
- Context window management - rolling conversation summarization at 75% context window
- Scheduled tasks - cron, interval, and one-shot scheduling

### Skills
- Define agent skills as Markdown files (SKILL.md) with YAML frontmatter
- Auto-discovery from `~/.opencrust/skills/` - injected into the system prompt
- CLI: `opencrust skill list`, `opencrust skill install <url>`, `opencrust skill remove <name>`

### Infrastructure
- **Config hot-reload** - edit `config.yml`, changes apply without restart
- **Daemonization** - `opencrust start --daemon` with PID management
- **Self-update** - `opencrust update` downloads the latest release with SHA-256 verification, `opencrust rollback` to revert
- **Restart** - `opencrust restart` gracefully stops and starts the daemon
- **Runtime provider switching** - add or switch LLM providers via the webchat UI or REST API without restarting
- **Migration tool** - `opencrust migrate openclaw` imports skills, channels, and credentials
- **Conversation summarization** - rolling summary at 75% context window, session summaries persisted across restarts
- **Interactive setup** - `opencrust init` wizard for provider and channel configuration
- **Diagnostics** - `opencrust doctor` checks config, data directory, credential vault, LLM provider reachability, channel credentials, MCP server connectivity, and database integrity

## Migrating from OpenClaw?

One command imports your skills, channel configs, credentials (encrypted into the vault), and personality (`SOUL.md` as `dna.md`):

```bash
opencrust migrate openclaw
```

Use `--dry-run` to preview changes before committing. Use `--source /path/to/openclaw` to specify a custom OpenClaw config directory.

## Configuration

OpenCrust looks for config at `~/.opencrust/config.yml`:

```yaml
gateway:
  host: "127.0.0.1"
  port: 3888
  # api_key: "your-secret-key"  # optional: protects /api/* endpoints when exposed publicly
                                 # generate with: openssl rand -hex 32

llm:
  claude:
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    # api_key resolved from: vault > config > ANTHROPIC_API_KEY env var

  ollama-local:
    provider: ollama
    model: llama3.1
    base_url: "http://localhost:11434"

channels:
  telegram:
    type: telegram
    enabled: true
    bot_token: "your-bot-token"  # or TELEGRAM_BOT_TOKEN env var

  line:
    type: line
    enabled: true
    channel_access_token: "your-access-token"  # or LINE_CHANNEL_ACCESS_TOKEN env var
    channel_secret: "your-secret"              # or LINE_CHANNEL_SECRET env var
    dm_policy: pairing     # open | pairing | allowlist (default: pairing)
    group_policy: mention  # open | mention | disabled (default: open)

agent:
  # Personality is configured via ~/.opencrust/dna.md (auto-created on first message)
  max_tokens: 4096
  max_context_tokens: 100000

guardrails:
  max_input_chars: 16000            # reject messages longer than this (default: 16000)
  max_output_chars: 32000           # truncate responses longer than this (default: 32000)
  token_budget_session: 10000       # max input+output tokens per session
  token_budget_user_daily: 100000   # max tokens per user per day
  token_budget_user_monthly: 500000 # max tokens per user per month
  allowed_tools:                    # null = all tools allowed; [] = no tools allowed
    - web_search
    - file_read
  session_tool_call_budget: 15      # max tool calls per session

gateway:
  rate_limit:
    max_messages_per_minute: 10     # per-user message rate limit
    cooldown_seconds: 30            # cooldown period after limit is exceeded

memory:
  enabled: true

# MCP servers for external tools
mcp:
  filesystem:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
  remote-server:
    transport: http
    url: "https://mcp.example.com/sse"
```

See the [full configuration reference](https://opencrust-org.github.io/opencrust/) for all options including Discord, Slack, WhatsApp, WhatsApp Web, iMessage, embeddings, and MCP server setup.

## Architecture

```
crates/
  opencrust-cli/        # CLI, init wizard, daemon management
  opencrust-gateway/    # WebSocket gateway, HTTP API, sessions
  opencrust-config/     # YAML/TOML loading, hot-reload, MCP config
  opencrust-channels/   # Discord, Telegram, Slack, WhatsApp, WhatsApp Web, iMessage, LINE, WeChat, MQTT
  opencrust-agents/     # LLM providers, tools, MCP client, agent runtime
  opencrust-db/         # SQLite memory, vector search (sqlite-vec)
  opencrust-plugins/    # WASM plugin sandbox (wasmtime)
  opencrust-media/      # TTS (Kokoro, OpenAI), STT (Whisper), media processing
  opencrust-security/   # Credential vault, allowlists, pairing, validation
  opencrust-skills/     # SKILL.md parser, scanner, installer
  opencrust-common/     # Shared types, errors, utilities
```

| Component | Status |
|-----------|--------|
| Gateway (WebSocket, HTTP, sessions) | Working |
| Telegram (streaming, commands, pairing, photos, voice, documents) | Working |
| Discord (slash commands, sessions) | Working |
| Slack (Socket Mode, streaming) | Working |
| WhatsApp (webhooks) | Working |
| WhatsApp Web (QR code, Baileys sidecar) | Working |
| iMessage (macOS, group chats) | Working |
| LINE (webhooks, reply/push fallback) | Working |
| WeChat (Official Account webhooks, media dispatch) | Working |
| MQTT (broker client, Mode A/B auto-detect, reconnect, QoS 0/1/2) | Working |
| LLM providers (15: Anthropic, OpenAI, Ollama + 12 OpenAI-compatible) | Working |
| Agent tools (bash, file_read, file_write, web_fetch, web_search, doc_search, schedule_heartbeat, cancel_heartbeat, list_heartbeats, mcp_resources) | Working |
| MCP client (stdio, HTTP, tool bridging, resources, instructions) | Working |
| A2A protocol (Agent-to-Agent) | Working |
| Multi-agent routing (named agents) | Working |
| Skills (SKILL.md, auto-discovery) | Working |
| Config (YAML/TOML, hot-reload) | Working |
| Personality (DNA bootstrap, hot-reload) | Working |
| Memory (SQLite, vector search, summarization) | Working |
| Security (vault, allowlist, pairing, per-channel policies, log redaction) | Working |
| Scheduling (cron, interval, one-shot) | Working |
| CLI (init, start/stop/restart, update, migrate, mcp, skills, doctor) | Working |
| Plugin system (WASM sandbox) | Scaffolded |
| TTS (Kokoro, OpenAI) + STT (Whisper, OpenAI) | Working |

## Contributing

OpenCrust is open source under the MIT license. Join the [Discord](https://discord.gg/97jTJEUz4) to chat with contributors, ask questions, or share what you're building. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup instructions, code guidelines, and the crate overview.

### Current priorities

| Priority | Issue | Description |
|----------|-------|-------------|
| **P0** | [#99](https://github.com/opencrust-org/opencrust/issues/99) | Brand facelift: logo, images, visual identity |
| **P1** | [#150](https://github.com/opencrust-org/opencrust/issues/150) | Fallback model chain: auto-retry with backup providers |
| **P1** | [#152](https://github.com/opencrust-org/opencrust/issues/152) | Token usage tracking and cost reporting |
| **P1** | [#153](https://github.com/opencrust-org/opencrust/issues/153) | `opencrust doctor` diagnostic command |
| **P1** | [#146](https://github.com/opencrust-org/opencrust/issues/146) | Guardrails: safety, rate limits, and cost controls |
| **P2** | [#185](https://github.com/opencrust-org/opencrust/issues/185) | MCP: Apps support (interactive HTML interfaces) |
| **P2** | [#158](https://github.com/opencrust-org/opencrust/issues/158) | Auto-backup config files before changes |
| **P2** | [#142](https://github.com/opencrust-org/opencrust/issues/142) | Web-based setup wizard at /setup |

Browse all [open issues](https://github.com/opencrust-org/opencrust/issues) or filter by [`good-first-issue`](https://github.com/opencrust-org/opencrust/issues?q=label%3Agood-first-issue+is%3Aopen) to find a place to start.

## Contributors

<a href="https://github.com/opencrust-org/opencrust/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=opencrust-org/opencrust" />
</a>

## License

MIT
