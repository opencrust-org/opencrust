<p align="center">
  <img src="assets/logo.png" alt="OpenCrust" width="280" />
</p>

<h1 align="center">OpenCrust - Trust the Crust</h1>

<p align="center">
  <strong>A personal AI assistant platform, written in Rust.</strong>
</p>

<p align="center">
  <a href="https://github.com/opencrust-org/opencrust/actions"><img src="https://github.com/opencrust-org/opencrust/workflows/CI/badge.svg" alt="CI"></a>
  <a href="https://github.com/opencrust-org/opencrust/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://github.com/opencrust-org/opencrust/issues"><img src="https://img.shields.io/github/issues/opencrust-org/opencrust" alt="Issues"></a>
  <a href="https://github.com/opencrust-org/opencrust/issues?q=label%3Agood-first-issue+is%3Aopen"><img src="https://img.shields.io/github/issues/opencrust-org/opencrust/good-first-issue?color=7057ff&label=good%20first%20issues" alt="Good First Issues"></a>
</p>

<p align="center">
  <a href="#getting-started">Getting Started</a> &middot;
  <a href="#features">Features</a> &middot;
  <a href="#architecture">Architecture</a> &middot;
  <a href="#contributing">Contributing</a> &middot;
  <a href="https://github.com/opencrust-org/opencrust/issues">Roadmap</a>
</p>

---

Rewritten from [OpenClaw](https://github.com/openclaw/openclaw). High-performance, low-resource, single-binary. Same multi-channel, multi-LLM architecture with the safety, speed, and deployment simplicity that Rust provides.

## Why Rust?

| Benefit | What it means for you |
|---------|----------------------|
| **Single binary** | `cargo install opencrust` and you're done. No Node.js, no pnpm, no runtime dependencies. |
| **Low memory** | Runs comfortably on a Raspberry Pi alongside local LLMs. 10-50x less RAM than the Node.js version. |
| **Always-on reliability** | Memory safety without garbage collection pauses. If it compiles, it runs. |
| **Fast startup** | Sub-second cold start vs multi-second Node.js boot. |
| **Easy cross-compilation** | Build for Linux ARM, x86, macOS from a single machine. |

## Features

### LLM Providers
- **Anthropic Claude** — full support with streaming (SSE)
- **OpenAI / compatible APIs** — GPT-4o, Azure, local endpoints via `base_url`, with streaming
- **Ollama** — local models with streaming support

### Channels
- **Telegram** — streaming responses, MarkdownV2 formatting, bot commands (`/start`, `/help`, `/clear`, `/pair`, `/users`), user allowlist with pairing codes, typing indicators
- **Discord** — full integration via serenity, event-driven message handling, session management
- **Slack** — Socket Mode support, streaming responses via delta channel, allowlist/pairing
- **WhatsApp** — webhook-driven via Meta Cloud API, allowlist/pairing, command handling

### MCP (Model Context Protocol)
- **External tool servers** — connect any MCP-compatible server (filesystem, GitHub, databases, web search, etc.)
- **Stdio transport** — spawns MCP servers as child processes
- **Tool bridging** — MCP tools appear as native agent tools with namespaced names (`server.tool`)
- **Dual config** — configure in `config.yml` or `~/.opencrust/mcp.json` (Claude Desktop compatible)
- **CLI management** — `opencrust mcp list` and `opencrust mcp inspect <name>`

### Agent Runtime
- **Tool execution loop** — bash, file read/write, web fetch with up to 10 tool iterations per request
- **MCP tools** — additional tools from external MCP servers
- **Memory recall** — SQLite-backed conversation memory with vector search (sqlite-vec)
- **Embedding support** — Cohere embeddings for semantic memory retrieval
- **Prompt injection detection** — input validation and sanitization
- **Context window management** — automatic history trimming to stay within token limits

### Skills
- **SKILL.md format** — define agent skills as Markdown files with YAML frontmatter
- **Auto-discovery** — skills in `~/.opencrust/skills/` are injected into the system prompt
- **CLI management** — `opencrust skill list`, `opencrust skill install <url>`, `opencrust skill remove <name>`
- **Trigger-based activation** — skills declare trigger keywords for contextual activation

### Infrastructure
- **Credential vault** — AES-256-GCM encrypted API key storage (`~/.opencrust/credentials/vault.json`)
- **Config hot-reload** — edit `config.yml` and changes apply without restart (agent settings, log level)
- **Daemonization** — `opencrust start --daemon` with PID file and log redirection
- **WebSocket gateway** — session resume, ping/pong heartbeat, graceful shutdown
- **Interactive setup** — `opencrust init` wizard guides through provider and API key configuration
- **Migration tool** — `opencrust migrate openclaw` imports data from OpenClaw

## Getting Started

### Prerequisites

- Rust 1.85+ (install via [rustup](https://rustup.rs))

### Build

```bash
cargo build --release
```

### Quick Start

```bash
# Interactive setup — picks your LLM provider, stores API keys, writes config
opencrust init

# Start the gateway (foreground)
opencrust start

# Or run as a background daemon
opencrust start --daemon

# Check status
opencrust status

# Stop the daemon
opencrust stop
```

### Configuration

OpenCrust looks for config at `~/.opencrust/config.yml` (or `config.toml`):

```yaml
gateway:
  host: "127.0.0.1"
  port: 3000

llm:
  claude:
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    # api_key can also be stored in the credential vault or ANTHROPIC_API_KEY env var
    api_key: "sk-..."

  ollama-local:
    provider: ollama
    model: llama3.1
    base_url: "http://localhost:11434"

channels:
  telegram:
    type: telegram
    enabled: true
    # bot_token can also be set via TELEGRAM_BOT_TOKEN env var
    bot_token: "your-bot-token"

  discord:
    type: discord
    enabled: true
    # bot_token and application_id can be set via env vars
    bot_token: "your-bot-token"
    application_id: 123456789

  slack:
    type: slack
    enabled: true
    bot_token: "xoxb-..."
    app_token: "xapp-..."

  whatsapp:
    type: whatsapp
    enabled: true
    access_token: "your-access-token"
    phone_number_id: "123456789"
    verify_token: "your-verify-token"

agent:
  system_prompt: "You are a helpful assistant."
  max_tokens: 4096
  max_context_tokens: 100000  # auto-trims old messages to fit

memory:
  enabled: true
  # Optional: add an embedding provider for semantic memory recall
  # embedding_provider: "cohere"

# Optional: embedding provider for vector search in memory
# embeddings:
#   cohere:
#     provider: cohere
#     model: embed-english-v3.0
#     api_key: "your-cohere-key"

# Optional: MCP servers for external tools
mcp:
  filesystem:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    timeout: 30
```

API keys are resolved in order: **credential vault** > **config file** > **environment variable**.

#### MCP Configuration

MCP servers can also be configured in `~/.opencrust/mcp.json` (Claude Desktop compatible format):

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_TOKEN": "ghp_..."
      }
    }
  }
}
```

Both config sources are merged at startup (`config.yml` wins on conflicts).

## Architecture

OpenCrust is organized as a Cargo workspace with focused crates:

```
crates/
  opencrust-cli/        # CLI entry point, init wizard, daemon management
  opencrust-gateway/    # WebSocket gateway, HTTP API, session management
  opencrust-config/     # YAML/TOML loading, hot-reload file watcher, MCP config
  opencrust-channels/   # Channel trait + Discord, Telegram, Slack, WhatsApp
  opencrust-agents/     # LLM providers, tool execution, MCP client, streaming, agent runtime
  opencrust-db/         # SQLite memory store, vector search (sqlite-vec)
  opencrust-plugins/    # WASM-based plugin system
  opencrust-media/      # Image, audio, video processing
  opencrust-security/   # Credential vault, allowlists, pairing, input validation
  opencrust-skills/     # SKILL.md parser, scanner, installer
  opencrust-common/     # Shared types, errors, utilities
```

### Status

| Component | Crate | Status |
|-----------|-------|--------|
| Gateway (WebSocket, HTTP, sessions) | `opencrust-gateway` | **Working** |
| Discord channel | `opencrust-channels` | **Working** |
| Telegram channel (streaming) | `opencrust-channels` | **Working** |
| Slack channel (Socket Mode, streaming) | `opencrust-channels` | **Working** |
| WhatsApp channel (webhook) | `opencrust-channels` | **Working** |
| LLM providers (Anthropic, OpenAI, Ollama) | `opencrust-agents` | **Working** |
| Agent tools (bash, file_read, file_write, web_fetch) | `opencrust-agents` | **Working** |
| MCP client (stdio transport, tool bridging) | `opencrust-agents` | **Working** |
| Skills (SKILL.md, auto-discovery, install) | `opencrust-skills` | **Working** |
| Config (YAML/TOML, hot-reload, MCP config) | `opencrust-config` | **Working** |
| Memory (SQLite, sqlite-vec, embeddings) | `opencrust-db` | **Working** |
| Security (vault, allowlist, pairing) | `opencrust-security` | **Working** |
| CLI (init, start/stop, channels, skills, mcp, migrate) | `opencrust-cli` | **Working** |
| Plugin system (WASM) | `opencrust-plugins` | Scaffolded |
| Media processing | `opencrust-media` | Scaffolded |

## Contributing

OpenCrust is open source under the MIT license. Contributions are welcome.

### How to contribute

1. Check the [open issues](https://github.com/opencrust-org/opencrust/issues) for work that needs doing
2. Issues are labeled by area (`channel`, `agent`, `phase-2`, `security`) and effort (`good-first-issue`, `help-wanted`)
3. Fork, branch, and submit a PR
4. Make sure `cargo check`, `cargo test`, `cargo clippy`, and `cargo fmt --check` pass

### Current priorities

1. **MCP enhancements** — resources, prompts, HTTP transport ([#80](https://github.com/opencrust-org/opencrust/issues/80))
2. **Discord full spec** — streaming, threads, slash commands ([#77](https://github.com/opencrust-org/opencrust/issues/77))
3. **Test suite** — comprehensive integration and unit test coverage ([#72](https://github.com/opencrust-org/opencrust/issues/72))
4. **Documentation** — rustdoc + mdbook site ([#75](https://github.com/opencrust-org/opencrust/issues/75))

### Code style

- Run `cargo fmt` before committing
- Run `cargo clippy` and fix warnings
- Keep crate boundaries clean (no circular dependencies)
- Prefer concrete types over dynamic dispatch where possible
- Write tests for new functionality

## License

MIT
