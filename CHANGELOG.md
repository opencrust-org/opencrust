# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.1.5] - 2026-02-21

### Changed
- Feature-gate wasmtime/plugins behind opt-in `--features plugins` cargo feature
- Default release binary reduced from 22 MB to 16 MB (27% smaller)
- Removed unused `opencrust-plugins` dependency from gateway crate

## [0.1.4] - 2026-02-21

### Added
- `opencrust restart` command - gracefully stops daemon (if running) then starts a new one
- `try_stop_daemon()` helper that silently handles "no daemon running" case

### Changed
- Post-update message now suggests `opencrust restart` instead of `opencrust stop && opencrust start`

## [0.1.3] - 2026-02-21

### Fixed
- Bumped workspace version to match release tags (was stuck at 0.1.0, causing false update notices)

## [0.1.2] - 2026-02-21

### Added
- `opencrust update` command - downloads latest release from GitHub with SHA-256 checksum verification, atomic binary replacement, and backup
- `opencrust rollback` command - restores the previous binary from `.old` backup
- Background version check with 24h cached TTL (`~/.opencrust/update-check.json`)
- CLI update notice printed on every command when a newer version is available
- Webchat dismissible update banner when `/api/status` reports a newer version
- `version` and `latest_version` fields in `/api/status` response

## [0.1.1] - 2026-02-20

### Added
- Runtime LLM provider switching via webchat dropdown and REST API (`GET/POST /api/providers`)
- `AgentRuntime` interior mutability (`RwLock<Vec<Arc<dyn LlmProvider>>>`) for adding providers after startup
- `OpenAiProvider::with_name()` builder for OpenAI-compatible APIs with distinct provider IDs
- `try_vault_set()` for best-effort API key persistence at runtime
- WebSocket messages accept optional `provider` field for per-message provider routing
- Webchat sidebar with provider dropdown, API key input, and "Save & Activate" button

## [0.1.0] - 2026-02-20

### Added
- **A2A protocol** - Agent-to-agent communication with agent card (`/.well-known/agent.json`), task CRUD endpoints, and outbound `A2AClient` (#71)
- **Multi-agent routing** - Named agent configs, priority-based agent router, and REST session API (`POST /api/sessions`, `POST /api/sessions/:id/messages`, `GET /api/sessions/:id/history`) (#108)
- **MCP enhancements** - Resources, prompts, HTTP transport (`mcp-http` feature), auto-reconnect health monitor, `mcp resources` and `mcp prompts` CLI commands (#80)
- **Security hardening** - Shared log redaction crate, configurable HTTP rate limits, per-WebSocket sliding window throttle (30 msg/min) (#74)
- **Security documentation** - Architecture overview, vendor-neutral audit checklist, AI agent attack surfaces guide (#113)
- **Install script** - `curl -fsSL` one-liner with OS/arch detection, SHA-256 verification, smart install directory (#109)
- **Release matrix** - Linux aarch64 (via `cross`) and Windows x86_64 CI targets (#110)
- **Scheduling hardening** - Recursive self-scheduling guard, delay cap (24h), per-session pending limit (5), failing task retry with backoff (#107)
- **Built-in skills** - 6 starter skills: summarize, translate, code-review, explain, rewrite, brainstorm (#106)
- **README overhaul** - Competitive positioning, benchmark numbers, updated Quick Start (#103)
- **iMessage channel** - macOS-native iMessage adapter with group chats, attachments, reconnect backoff, deployment docs (#100, #101)
- **Sansa LLM provider** - Integration with Sansa AI (#98)
- **Security & sandbox fixes** - Path traversal prevention, SSRF blocking, WASM sandbox limits, test coverage (#97)
- **OpenClaw migration** - Migration tool for conversations and credentials from OpenClaw (TypeScript predecessor) (#103)
- **Discord channel** - Bot integration with streaming, slash command mapping, callback pipeline (#95)
- **Scheduling system** - Persistent task scheduling with heartbeat execution (#96)
- **WASM plugin sandbox** - Hot-reload registry, epoch deadlines, sandbox resource limits (#94)
- **Chat persistence** - Session hydration and history persistence across channels
- **WebSocket authentication** - API key auth for WebSocket handler with query param and header support
- **MCP client** - Model Context Protocol support with stdio transport, tool bridging, namespaced tools
- **Slack channel** - Socket Mode integration with markdown formatting
- **WhatsApp channel** - Webhook-based integration with verification endpoint
- **SKILL.md support** - Skill file parser, scanner, and installer
- **OpenAI streaming** - Streaming response support for OpenAI provider
- **Telegram channel** - Bot with allowlist, commands, typing indicator, streaming, markdown formatting, context window management
- **Ollama provider** - Local LLM support with tool calling
- **Agent orchestration** - Conversation loop with tool execution (max 10 iterations) and memory recall
- **Cohere embeddings** - Embedding provider for vector search in memory store
- **Memory store** - SQLite-backed memory with sqlite-vec for vector search, cross-channel continuity
- **Core providers** - Anthropic and OpenAI LLM providers with tool support
- **Core tools** - Bash, file read, file write, web fetch
- **Credential vault** - AES-256-GCM encrypted secret storage with PBKDF2-SHA256
- **CLI** - `opencrust init` wizard, daemon mode, MCP/skill/channel/plugin commands
- **Gateway** - Axum-based WebSocket gateway with HTTP API, session management, config hot-reload
- **Security** - Allowlists, pairing codes, prompt injection detection (14 patterns), input validation
- **CI/CD** - GitHub Actions: check, test, clippy, fmt, cargo-deny, release pipeline

### Changed
- Repository moved to `opencrust-org` organization
- Config loading follows XDG standards with backward compatibility
- `cargo-deny` migrated to v2 config format
- `opencrust.dev` references updated to `opencrust.org`

### Fixed
- Dangling symlink vulnerability in media processor
- Insecure file operation in `MediaProcessor`
- Clippy warnings and formatting across workspace

### Security
- Path traversal prevention in WASM plugin sandbox (post-canonicalize boundary check)
- SSRF blocking (private IP range rejection in plugins)
- Log redaction for API keys (Anthropic, OpenAI, Slack tokens)
- Rate limiting on HTTP and WebSocket endpoints
- Prompt injection detection with 14 pattern categories
