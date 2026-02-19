# Security-First Architecture

OpenCrust treats security as a core requirement, not an afterthought. AI agents that run 24/7, access private data, and communicate externally demand a higher standard than typical web applications.

## Credential Vault

All API keys and tokens are encrypted at rest using **AES-256-GCM** with keys derived via **PBKDF2-SHA256** (600,000 iterations). The implementation uses the `ring` crate (BoringSSL-derived, FIPS-grade primitives).

- **Storage:** `~/.opencrust/credentials/vault.json`
- **Salt:** 32 bytes, unique per vault, generated with `SystemRandom`
- **Nonce:** 12 bytes (AES-256-GCM standard), regenerated on every save
- **Key derivation:** PBKDF2-HMAC-SHA256, 600k iterations, 32-byte derived key
- **Resolution chain:** vault > config file > environment variable

Credentials never appear in plaintext on disk. The vault passphrase is prompted at `opencrust init` and required to unlock at startup.

## Authentication

### WebSocket Pairing

The gateway requires authentication by default. Clients must provide an API key via query parameter (`?token=...`) or `Authorization: Bearer` header. Key comparison uses constant-time comparison to prevent timing attacks.

### Channel Pairing Codes

Per-channel authentication uses one-time 6-digit pairing codes:

- Generated with cryptographic randomness (`rand` crate)
- 5-minute expiry window
- Single-use: code is consumed on first successful pairing
- Users must pair before the agent will respond on that channel

## Input Validation

### Prompt Injection Detection

All user input passes through `InputValidator` before reaching the LLM. Detection covers 14 known injection patterns:

- Instruction override: "ignore previous instructions", "disregard your instructions"
- Identity hijacking: "you are now", "pretend you are", "act as if"
- Directive injection: "new instructions:", "system prompt:"
- Safety bypass: "forget everything", "override your", "do not follow", "bypass your"
- Exfiltration: "reveal your system", "what is your system prompt"

Pattern matching is case-insensitive. Detected injections are rejected with a `prompt_injection_detected` error and logged for audit.

### Input Sanitization

Control characters (except `\n` and `\t`) are stripped before processing. Channel IDs are validated for length (max 256 characters) and non-empty constraints.

## User Allowlists

Each channel supports per-channel allowlists that control who can interact with the agent:

- **Closed mode:** only explicitly listed user IDs can message the agent
- **Open mode:** all users permitted (opt-in, not default)
- Unauthorized messages are silently dropped (no information leakage)

## WASM Plugin Sandboxing

Plugins run in a WebAssembly sandbox powered by `wasmtime`:

- Memory isolation: plugins cannot access host memory directly
- Controlled imports: only explicitly granted host functions are available
- Resource limits: configurable memory and execution bounds

## Network Security

- **Localhost binding:** gateway binds to `127.0.0.1` by default, not `0.0.0.0`
- **HTTP rate limiting:** per-IP rate limiting via Governor (configurable requests/second and burst size)
- **WebSocket limits:** max frame size (64 KB), max message size (256 KB), max text size (32 KB)
- **Heartbeat timeout:** connections without pong response for 90 seconds are closed
- **Per-WebSocket message rate limiting:** sliding window (30 messages/minute) prevents abuse

## Log Redaction

Sensitive tokens are automatically redacted from all log output using pattern matching:

- Anthropic API keys (`sk-ant-api...`)
- OpenAI-style keys (`sk-...`)
- Slack tokens (`xoxb-...`, `xapp-...`, `xoxp-...`)
- Discord bot tokens (`Bot ...`)

The `RedactingWriter` wraps the log output layer so redaction applies regardless of log level or destination.
