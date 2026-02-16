# OpenCrust

A personal AI assistant platform, rewritten in Rust from [OpenClaw](https://github.com/openclaw/openclaw).

OpenCrust aims to be a high-performance, low-resource, single-binary alternative to OpenClaw. It keeps the same multi-channel, multi-LLM architecture while gaining the safety, speed, and deployment simplicity that Rust provides.

## Why Rust?

| Benefit | What it means for you |
|---------|----------------------|
| **Single binary** | `cargo install opencrust` and you're done. No Node.js, no pnpm, no runtime dependencies. |
| **Low memory** | Runs comfortably on a Raspberry Pi alongside local LLMs. 10-50x less RAM than the Node.js version. |
| **Always-on reliability** | Memory safety without garbage collection pauses. If it compiles, it runs. |
| **Fast startup** | Sub-second cold start vs multi-second Node.js boot. |
| **Easy cross-compilation** | Build for Linux ARM, x86, macOS from a single machine. |

## Architecture

OpenCrust is organized as a Cargo workspace with focused crates:

```
crates/
  opencrust-cli/        # CLI entry point and commands
  opencrust-gateway/    # WebSocket gateway and HTTP API (axum/tokio)
  opencrust-config/     # YAML/TOML configuration loading
  opencrust-channels/   # Messaging channel trait and implementations
  opencrust-agents/     # LLM provider abstraction and agent runtime
  opencrust-db/         # SQLite storage and vector search
  opencrust-plugins/    # WASM-based plugin system
  opencrust-media/      # Image, audio, video processing
  opencrust-security/   # Validation, allowlists, pairing codes
  opencrust-common/     # Shared types, errors, utilities
```

### How it maps to OpenClaw

| OpenClaw (TypeScript) | OpenCrust (Rust) | Status |
|----------------------|------------------|--------|
| `src/gateway/` | `opencrust-gateway` | Scaffolded |
| `src/channels/` + `src/discord/` etc. | `opencrust-channels` | Trait defined |
| `src/agents/` | `opencrust-agents` | Trait defined |
| `src/config/` | `opencrust-config` | Working |
| `src/memory/` | `opencrust-db` | Scaffolded |
| `src/plugins/` | `opencrust-plugins` | Scaffolded |
| `src/media/` | `opencrust-media` | Scaffolded |
| `src/security/` + `src/pairing/` | `opencrust-security` | Scaffolded |
| `src/cli/` | `opencrust-cli` | Working |
| `src/infra/` | `opencrust-common` | Working |
| `apps/macos/` | Keep as-is (Swift) | N/A |
| `apps/ios/` | Keep as-is (Swift) | N/A |
| `apps/android/` | Keep as-is (Kotlin) | N/A |
| `ui/` | Keep as-is or port to Leptos | N/A |

## Getting Started

### Prerequisites

- Rust 1.85+ (install via [rustup](https://rustup.rs))

### Build

```bash
cargo build
```

### Run

```bash
# Initialize config directory
cargo run -- init

# Start the gateway
cargo run -- start

# Check status
cargo run -- status
```

### Configuration

OpenCrust looks for config at `~/.opencrust/config.yml` (or `config.toml`):

```yaml
gateway:
  host: "127.0.0.1"
  port: 3000

channels:
  my-telegram:
    type: telegram
    enabled: true
    bot_token: "your-token-here"

llm:
  anthropic:
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    api_key: "sk-..."
```

## Contributing

OpenCrust is open source under the MIT license. Contributions are welcome.

### How to contribute

1. Check the [open issues](https://github.com/moecandoit/opencrust/issues) for work that needs doing
2. Issues are labeled by area (`channel`, `agent`, `gateway`, etc.) and effort (`good-first-issue`, `help-wanted`)
3. Fork, branch, and submit a PR
4. Make sure `cargo check`, `cargo test`, and `cargo clippy` pass

### Project priorities

The current focus areas, roughly in order:

1. **Core gateway** - Get the WebSocket server and message routing production-ready
2. **LLM providers** - Implement Anthropic, OpenAI, and Ollama providers
3. **Channel implementations** - Start with Telegram (`teloxide`) and Discord (`serenity`)
4. **Database layer** - Session persistence and vector search with sqlite-vec
5. **Plugin system** - WASM runtime for extensions
6. **Remaining channels** - Slack, WhatsApp, Signal, and others

### Code style

- Run `cargo fmt` before committing
- Run `cargo clippy` and fix warnings
- Keep crate boundaries clean (no circular dependencies)
- Prefer concrete types over dynamic dispatch where possible
- Write tests for new functionality

## License

MIT
