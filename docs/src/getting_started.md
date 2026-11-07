# Getting Started

## Quick Start

The fastest way to get started is using the install script:

```bash
# Install (Linux, macOS)
curl -fsSL https://raw.githubusercontent.com/opencrust-org/opencrust/main/install.sh | sh

# Interactive setup - pick your LLM provider and channels
opencrust init

# Start - on first message, the agent learns your preferences
opencrust start
```

## Build from Source

You can also build from source if you have Rust installed (1.96+).

```bash
cargo build --release
./target/release/opencrust init
./target/release/opencrust start
```

## Configuration

OpenCrust looks for its configuration file at `~/.opencrust/config.yml`.

Example configuration:

```yaml
gateway:
  host: "127.0.0.1"
  port: 3888

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
  # Personality is configured via ~/.opencrust/dna.md (auto-created on first message)
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

## Personality (DNA)

On first message, if no `~/.opencrust/dna.md` exists, the agent will introduce itself and ask a few questions:

1. What should I call you?
2. What should I call myself?
3. How do you prefer I communicate - casual, professional, or something else?
4. Any specific guidelines or things to avoid?

The agent then writes `~/.opencrust/dna.md` with your answers and continues helping with whatever you originally asked. The file hot-reloads - edit it anytime and the agent adapts immediately without a restart.

You can also create `dna.md` manually:

```markdown
# Identity
Neo

# User Preferences

## Name
Morpheus

## Communication Style
Casual

## Guidelines
- Keep things relaxed and conversational
```

## Migrating from OpenClaw

If you are migrating from OpenClaw, you can use the migration tool to import your skills, channel configs, credentials, and personality (`SOUL.md` is imported as `dna.md`).

```bash
opencrust migrate openclaw
```

Use `--dry-run` to preview changes before committing. Use `--source /path/to/openclaw` to specify a custom OpenClaw config directory.
