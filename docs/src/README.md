# OpenCrust

**The secure, lightweight open-source AI agent framework.**

A single 17 MB binary that runs your AI agents across Telegram, Discord, Slack, WhatsApp, LINE, and iMessage - with encrypted credential storage, config hot-reload, and 13 MB of RAM at idle. Built in Rust for the security and reliability that AI agents demand.

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
| **Channels** | 6 | 6+ | 4 |
| **LLM providers** | 15 | 10+ | 22+ |
| **Pre-compiled binaries** | Yes | N/A (Node.js) | Build from source |
| **Config hot-reload** | Yes | No | No |
| **WASM plugin system** | Yes (sandboxed) | No | No |

*Benchmarks measured on a 1 vCPU, 1 GB RAM DigitalOcean droplet. [Reproduce them yourself](bench/).*

## Features

- **LLM Providers**: 15 providers - Anthropic Claude, OpenAI, Ollama, and 12 OpenAI-compatible (Sansa, DeepSeek, Mistral, Gemini, Falcon, Jais, Qwen, Yi, Cohere, MiniMax, Moonshot).
- **Channels**: Telegram, Discord, Slack, WhatsApp, LINE, iMessage.
- **MCP**: Connect any MCP-compatible server for external tools.
- **Personality (DNA)**: Conversational bootstrap on first message - the agent asks your preferences and writes `~/.opencrust/dna.md`. Hot-reloads on edit.
- **Agent Runtime**: 6 built-in tools (bash, file_read, file_write, web_fetch, web_search, schedule_heartbeat), memory with vector search, conversation summarization, scheduled tasks.
- **Skills**: Define skills as Markdown files.
- **Infrastructure**: Config hot-reload, daemonization, self-update, migration tools.
- **Diagnostics**: `opencrust doctor` checks config, credential vault, LLM provider reachability, channel credentials, MCP server connectivity, and database integrity.

## Documentation Structure

- **[Getting Started](./getting_started.md)**: Install and configure OpenCrust.
- **[Architecture](./architecture.md)**: Understand the internal design.
- **[Channels](./channels.md)**: Configure communication channels.
- **[Providers](./providers.md)**: Set up LLM providers.
- **[Tools](./tools.md)**: Built-in agent tools reference.
- **[MCP](./mcp.md)**: Connect external tools via Model Context Protocol.
- **[Security](./security.md)**: Learn about security features.
- **[Plugins](./plugins.md)**: Extend functionality with WASM plugins.
