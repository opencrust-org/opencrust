<p align="center">
  <img src="assets/logo.png" alt="OpenCrust" width="280" />
</p>

<h1 align="center">OpenCrust</h1>

<p align="center">
  <strong>The secure, lightweight open-source AI agent framework.</strong>
</p>

<p align="center">
  <a href="https://github.com/opencrust-org/opencrust/actions"><img src="https://github.com/opencrust-org/opencrust/workflows/CI/badge.svg" alt="CI"></a>
  <a href="https://github.com/opencrust-org/opencrust/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://github.com/opencrust-org/opencrust/stargazers"><img src="https://img.shields.io/github/stars/opencrust-org/opencrust" alt="Stars"></a>
  <a href="https://github.com/opencrust-org/opencrust/issues"><img src="https://img.shields.io/github/issues/opencrust-org/opencrust" alt="Issues"></a>
  <a href="https://github.com/opencrust-org/opencrust/issues?q=label%3Agood-first-issue+is%3Aopen"><img src="https://img.shields.io/github/issues/opencrust-org/opencrust/good-first-issue?color=7057ff&label=good%20first%20issues" alt="Good First Issues"></a>
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

A single 17 MB binary that runs your AI agents across Telegram, Discord, Slack, WhatsApp, and iMessage — with encrypted credential storage, config hot-reload, and 13 MB of RAM at idle. Built in Rust for the security and reliability that AI agents demand.

<!-- TODO: Add VHS terminal demo GIF here (#103) -->

## Quick Start

```bash
# Build from source (Rust 1.85+)
cargo build --release

# Interactive setup — pick your LLM provider, store API keys in encrypted vault
./target/release/opencrust init

# Start
./target/release/opencrust start
```

Pre-compiled binaries for Linux (x86_64), macOS (Intel, Apple Silicon) are available on [GitHub Releases](https://github.com/opencrust-org/opencrust/releases).

## Why OpenCrust?

### vs OpenClaw, ZeroClaw, and other AI agent frameworks

| | **OpenCrust** | **OpenClaw** (Node.js) | **ZeroClaw** (Rust) |
|---|---|---|---|
| **Binary size** | 17 MB | ~1.2 GB (with node_modules) | ~25 MB |
| **Memory at idle** | 13 MB | ~388 MB | ~20 MB |
| **Cold start** | 3 ms | 13.9 s | ~50 ms |
| **Credential storage** | AES-256-GCM encrypted vault | Plaintext config file | Plaintext config file |
| **Auth default** | Enabled (WebSocket pairing) | Disabled by default | Disabled by default |
| **Scheduling** | Cron, interval, one-shot | Yes | No |
| **Multi-agent routing** | Planned (#108) | Yes (agentId) | No |
| **Session orchestration** | Planned (#108) | Yes | No |
| **MCP support** | Stdio | Stdio + HTTP | Stdio |
| **Channels** | 5 | 6+ | 4 |
| **LLM providers** | 4 | 10+ | 22+ |
| **Pre-compiled binaries** | Yes | N/A (Node.js) | Build from source |
| **Config hot-reload** | Yes | No | No |
| **WASM plugin system** | Yes (sandboxed) | No | No |

*Benchmarks measured on a 1 vCPU, 1 GB RAM DigitalOcean droplet. [Reproduce them yourself](bench/).*

## Security

OpenCrust is built for the security requirements of always-on AI agents that access private data and communicate externally.

- **Encrypted credential vault** — API keys and tokens stored with AES-256-GCM encryption at `~/.opencrust/credentials/vault.json`. Never plaintext on disk.
- **Authentication by default** — WebSocket gateway requires pairing codes. No unauthenticated access out of the box.
- **User allowlists** — per-channel allowlists control who can interact with the agent. Unauthorized messages are silently dropped.
- **Prompt injection detection** — input validation and sanitization before content reaches the LLM.
- **WASM sandboxing** — plugins run in a sandboxed WebAssembly runtime with controlled host access.
- **Localhost-only binding** — gateway binds to `127.0.0.1` by default, not `0.0.0.0`.

## Features

### LLM Providers
- **Anthropic Claude** — streaming (SSE), tool use
- **OpenAI / compatible APIs** — GPT-4o, Azure, any OpenAI-compatible endpoint via `base_url`
- **Ollama** — local models with streaming
- **Sansa** — regional LLM via [sansaml.com](https://sansaml.com)

### Channels
- **Telegram** — streaming responses, MarkdownV2, bot commands, typing indicators, user allowlist with pairing codes
- **Discord** — slash commands, event-driven message handling, session management
- **Slack** — Socket Mode, streaming responses, allowlist/pairing
- **WhatsApp** — Meta Cloud API webhooks, allowlist/pairing
- **iMessage** — macOS native via chat.db polling, group chats, AppleScript sending ([setup guide](docs/imessage-setup.md))

### MCP (Model Context Protocol)
- Connect any MCP-compatible server (filesystem, GitHub, databases, web search)
- Tools appear as native agent tools with namespaced names (`server.tool`)
- Configure in `config.yml` or `~/.opencrust/mcp.json` (Claude Desktop compatible)
- CLI: `opencrust mcp list`, `opencrust mcp inspect <name>`

### Agent Runtime
- Tool execution loop — bash, file read/write, web fetch (up to 10 iterations)
- SQLite-backed conversation memory with vector search (sqlite-vec + Cohere embeddings)
- Context window management — automatic history trimming
- Scheduled tasks — cron, interval, and one-shot scheduling

### Skills
- Define agent skills as Markdown files (SKILL.md) with YAML frontmatter
- Auto-discovery from `~/.opencrust/skills/` — injected into the system prompt
- CLI: `opencrust skill list`, `opencrust skill install <url>`, `opencrust skill remove <name>`

### Infrastructure
- **Config hot-reload** — edit `config.yml`, changes apply without restart
- **Daemonization** — `opencrust start --daemon` with PID management
- **Migration tool** — `opencrust migrate openclaw` imports skills, channels, and credentials
- **Interactive setup** — `opencrust init` wizard for provider and API key configuration

## Migrating from OpenClaw?

One command imports your skills, channel configs, and credentials (encrypted into the vault):

```bash
opencrust migrate openclaw
```

Use `--dry-run` to preview changes before committing. Use `--source /path/to/openclaw` to specify a custom OpenClaw config directory.

## Configuration

OpenCrust looks for config at `~/.opencrust/config.yml`:

```yaml
gateway:
  host: "127.0.0.1"
  port: 3000

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

agent:
  system_prompt: "You are a helpful assistant."
  max_tokens: 4096
  max_context_tokens: 100000

memory:
  enabled: true

# MCP servers for external tools
mcp:
  filesystem:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
```

See the [full configuration reference](docs/) for all options including Discord, Slack, WhatsApp, iMessage, embeddings, and MCP server setup.

## Architecture

```
crates/
  opencrust-cli/        # CLI, init wizard, daemon management
  opencrust-gateway/    # WebSocket gateway, HTTP API, sessions
  opencrust-config/     # YAML/TOML loading, hot-reload, MCP config
  opencrust-channels/   # Discord, Telegram, Slack, WhatsApp, iMessage
  opencrust-agents/     # LLM providers, tools, MCP client, agent runtime
  opencrust-db/         # SQLite memory, vector search (sqlite-vec)
  opencrust-plugins/    # WASM plugin sandbox (wasmtime)
  opencrust-media/      # Media processing (scaffolded)
  opencrust-security/   # Credential vault, allowlists, pairing, validation
  opencrust-skills/     # SKILL.md parser, scanner, installer
  opencrust-common/     # Shared types, errors, utilities
```

| Component | Status |
|-----------|--------|
| Gateway (WebSocket, HTTP, sessions) | Working |
| Telegram (streaming, commands, pairing) | Working |
| Discord (slash commands, sessions) | Working |
| Slack (Socket Mode, streaming) | Working |
| WhatsApp (webhooks) | Working |
| iMessage (macOS, group chats) | Working |
| LLM providers (Anthropic, OpenAI, Ollama, Sansa) | Working |
| Agent tools (bash, file_read, file_write, web_fetch) | Working |
| MCP client (stdio, tool bridging) | Working |
| Skills (SKILL.md, auto-discovery) | Working |
| Config (YAML/TOML, hot-reload) | Working |
| Memory (SQLite, vector search) | Working |
| Security (vault, allowlist, pairing) | Working |
| Scheduling (cron, interval, one-shot) | Working |
| CLI (init, start/stop, migrate, mcp, skills) | Working |
| Plugin system (WASM sandbox) | Scaffolded |
| Media processing | Scaffolded |

## Contributing

OpenCrust is open source under the MIT license. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup instructions, code guidelines, and the crate overview.

### Current priorities

| Priority | Issue | Description |
|----------|-------|-------------|
| **P0** | [#103](https://github.com/opencrust-org/opencrust/issues/103) | README and positioning |
| **P0** | [#104](https://github.com/opencrust-org/opencrust/issues/104) | Website: opencrust.dev |
| **P0** | [#105](https://github.com/opencrust-org/opencrust/issues/105) | Discord community |
| **P1** | [#106](https://github.com/opencrust-org/opencrust/issues/106) | Built-in starter skills |
| **P1** | [#107](https://github.com/opencrust-org/opencrust/issues/107) | Scheduling hardening |
| **P1** | [#108](https://github.com/opencrust-org/opencrust/issues/108) | Multi-agent routing |
| **P1** | [#109](https://github.com/opencrust-org/opencrust/issues/109) | Install script |
| **P1** | [#110](https://github.com/opencrust-org/opencrust/issues/110) | Linux aarch64 + Windows releases |
| **P1** | [#80](https://github.com/opencrust-org/opencrust/issues/80) | MCP: HTTP transport, resources, prompts |

Browse all [open issues](https://github.com/opencrust-org/opencrust/issues) or filter by [`good-first-issue`](https://github.com/opencrust-org/opencrust/issues?q=label%3Agood-first-issue+is%3Aopen) to find a place to start.

## License

MIT
