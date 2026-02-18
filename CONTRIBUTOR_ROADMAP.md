# OpenCrust Contributor Roadmap

This roadmap tracks project progress and upcoming work areas.

## Completed

The following milestones have been achieved:

- **Core runtime** — WebSocket gateway with session management, message routing to agent runtime, graceful shutdown
- **LLM providers** — Anthropic (streaming), OpenAI (streaming), Ollama (streaming)
- **Agent tools** — bash, file_read, file_write, web_fetch with 10-iteration tool loop
- **Channels** — Telegram (streaming, MarkdownV2, commands, allowlist/pairing), Discord (serenity, event-driven), Slack (Socket Mode, streaming), WhatsApp (webhook, Meta Cloud API)
- **MCP client** — stdio transport, tool bridging into agent runtime, dual config (config.yml + mcp.json), CLI commands (`mcp list`, `mcp inspect`)
- **Skills** — SKILL.md format with YAML frontmatter, auto-discovery, system prompt injection, CLI management
- **Memory** — SQLite-backed with sqlite-vec, Cohere embeddings, semantic recall, session continuity
- **Security** — AES-256-GCM credential vault, user allowlists, 6-digit pairing codes, prompt injection detection
- **Config** — YAML/TOML loading, hot-reload file watcher, MCP config merging
- **CLI** — init wizard, start/stop/status, daemon mode, channel/plugin/skill/mcp commands, OpenClaw migration
- **CI** — cargo check, test, clippy, fmt checks on every push/PR
- **Test coverage** — 127+ tests across all crates, including gateway integration tests

## Current Priorities

### Phase 2 — In Progress

| Area | Issue | Description |
|------|-------|-------------|
| MCP enhancements | [#80](https://github.com/opencrust-org/opencrust/issues/80) | Resources, prompts, HTTP transport, auto-reconnect |
| Discord full spec | [#77](https://github.com/opencrust-org/opencrust/issues/77) | Streaming, threads, slash commands |
| Test suite | [#72](https://github.com/opencrust-org/opencrust/issues/72) | Comprehensive test coverage and benchmarks |
| Documentation | [#75](https://github.com/opencrust-org/opencrust/issues/75) | rustdoc + mdbook site |
| CI/CD | [#73](https://github.com/opencrust-org/opencrust/issues/73) | Matrix builds, crates.io publishing, Docker, SBOM |
| Security hardening | [#74](https://github.com/opencrust-org/opencrust/issues/74) | cargo audit, rate limiting, log redaction |
| Feature flags | [#76](https://github.com/opencrust-org/opencrust/issues/76) | Feature flags and build targets |
| Cost budgeting | [#66](https://github.com/opencrust-org/opencrust/issues/66) | Usage tracking and model routing |
| Observability | [#67](https://github.com/opencrust-org/opencrust/issues/67) | OpenTelemetry (opencrust-telemetry crate) |

### Phase 3 — Planned

| Area | Issue | Description |
|------|-------|-------------|
| WebChat UI | [#27](https://github.com/opencrust-org/opencrust/issues/27) | Web-based chat interface |
| A2A protocol | [#71](https://github.com/opencrust-org/opencrust/issues/71) | Agent-to-Agent protocol support |
| Multi-user | [#70](https://github.com/opencrust-org/opencrust/issues/70) | Team support and user management |
| CrustHub | [#69](https://github.com/opencrust-org/opencrust/issues/69) | Skill registry |

### Backlog

See [all open issues](https://github.com/opencrust-org/opencrust/issues) for the full list, including:
- Additional channels (Matrix, IRC, Google Chat, Microsoft Teams)
- Additional LLM providers (Gemini, AWS Bedrock)
- Telegram enhancements (voice, images, file handling, group chat)
- Browser automation, web search tool, cron scheduler

## How to Pick Up Work

1. Check the [issues page](https://github.com/opencrust-org/opencrust/issues) — filter by `good-first-issue` or `help-wanted`
2. Comment on an issue to claim it
3. Fork, branch, implement, test, PR
4. All PRs must pass: `cargo check && cargo test && cargo clippy && cargo fmt --check`

## Definition of Done for Any PR

- Scope is small and single-purpose
- Tests included or updated
- `cargo fmt`, `cargo clippy`, and `cargo test` pass
- User-facing behavior documented if changed
