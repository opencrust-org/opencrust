# Contributing to OpenCrust

Thanks for your interest in contributing to OpenCrust!

## Quick Start

```bash
# Clone the repo
git clone https://github.com/opencrust-org/opencrust.git
cd opencrust

# Build
cargo build

# Run tests
cargo test

# Run linter
cargo clippy

# Format code
cargo fmt
```

## Finding Work

- Check the [Issues](https://github.com/opencrust-org/opencrust/issues) page
- Issues labeled `good-first-issue` are great starting points
- Issues labeled `help-wanted` are open for anyone to pick up
- Comment on an issue before starting work to avoid duplicate effort

### Current Priorities

| Priority | Issue | Description |
|----------|-------|-------------|
| **P0** | [#104](https://github.com/opencrust-org/opencrust/issues/104) | Website: opencrust.dev with alternatives pages |
| **P0** | [#105](https://github.com/opencrust-org/opencrust/issues/105) | Discord community |
| **P1** | [#106](https://github.com/opencrust-org/opencrust/issues/106) | Built-in starter skills |
| **P1** | [#107](https://github.com/opencrust-org/opencrust/issues/107) | Scheduling hardening |
| **P1** | [#108](https://github.com/opencrust-org/opencrust/issues/108) | Multi-agent routing and session orchestration |
| **P1** | [#80](https://github.com/opencrust-org/opencrust/issues/80) | MCP: resources, prompts, HTTP transport |
| **P1** | [#74](https://github.com/opencrust-org/opencrust/issues/74) | Security hardening |
| **P2** | [#72](https://github.com/opencrust-org/opencrust/issues/72) | Comprehensive test suite and benchmarks |
| **P2** | [#73](https://github.com/opencrust-org/opencrust/issues/73) | CI/CD: matrix builds, crates.io, Docker |
| **P2** | [#77](https://github.com/opencrust-org/opencrust/issues/77) | Discord full spec: streaming, threads |

## Pull Request Process

1. Fork the repository
2. Create a feature branch from `main`
3. Make your changes
4. Ensure all checks pass: `cargo check && cargo test && cargo clippy && cargo fmt --check`
5. Submit a PR with a clear description of what changed and why

## Code Guidelines

- Each crate has a focused responsibility. Keep boundaries clean.
- Prefer `Result<T, E>` over panics. Use `opencrust_common::Error` for crate-level errors.
- Write tests for new functionality. Place unit tests in the same file, integration tests in `tests/`.
- Keep functions short. If a function is doing too much, split it.
- Document public APIs with doc comments.

## Crate Overview

| Crate | Purpose |
|-------|---------|
| `opencrust-cli` | CLI binary, command parsing, daemon management, init wizard |
| `opencrust-gateway` | WebSocket server, HTTP API, session management, channel bootstrap |
| `opencrust-config` | Config file loading (YAML/TOML), hot-reload watcher, MCP config |
| `opencrust-channels` | Channel trait + Discord, Telegram, Slack, WhatsApp, iMessage implementations |
| `opencrust-agents` | LLM providers (Anthropic, OpenAI, Ollama), tools, MCP client, agent runtime |
| `opencrust-db` | SQLite memory store, vector search (sqlite-vec) |
| `opencrust-plugins` | WASM plugin loading and execution |
| `opencrust-media` | Media format handling and conversion |
| `opencrust-security` | Credential vault, allowlists, pairing codes, input validation |
| `opencrust-skills` | SKILL.md parser, scanner, installer |
| `opencrust-common` | Shared types, error enum, message model |

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
