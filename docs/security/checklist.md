# AI Agent Security Checklist

A vendor-neutral audit checklist for securing AI agent deployments. Applicable to any framework, not just OpenCrust.

## Credentials & Secrets

- [ ] API keys and tokens are encrypted at rest (not plaintext in config files)
- [ ] Secrets are never logged, even at debug/trace level
- [ ] Key rotation is possible without redeployment
- [ ] Environment variables are used as fallback only, not primary storage
- [ ] Vault/secret manager integration is available for production deployments
- [ ] Default credentials are absent; setup requires explicit configuration

## Authentication & Authorization

- [ ] Agent endpoints require authentication by default (not opt-in)
- [ ] WebSocket connections are authenticated before any message processing
- [ ] API key comparison uses constant-time operations (prevent timing attacks)
- [ ] Per-channel user allowlists restrict who can interact with the agent
- [ ] Pairing codes or equivalent are time-limited and single-use
- [ ] Admin operations are separated from user operations

## Input Validation

- [ ] All user input is sanitized (control characters stripped)
- [ ] Prompt injection patterns are detected and rejected before LLM processing
- [ ] Message size limits are enforced at the transport layer
- [ ] Input validation rules are updatable without redeployment
- [ ] Rejection events are logged with session context for audit

## Output Filtering

- [ ] LLM responses are checked before delivery to users
- [ ] Sensitive data patterns (API keys, credentials) are redacted from output
- [ ] Tool execution output is bounded in size
- [ ] Error messages do not leak internal state or stack traces to users

## Network Security

- [ ] Agent binds to localhost by default (not 0.0.0.0)
- [ ] HTTP rate limiting is enabled per-IP with configurable thresholds
- [ ] WebSocket connections have frame/message size limits
- [ ] Idle connections are cleaned up (heartbeat + timeout)
- [ ] TLS is enforced for all external API calls
- [ ] DNS rebinding protections are in place for webhook endpoints

## Tool & Plugin Security

- [ ] Tool execution has iteration limits (prevent runaway loops)
- [ ] File system tools are restricted to allowed paths
- [ ] Shell/bash tools have configurable command allowlists
- [ ] Plugins run in a sandbox (WASM, containers, or equivalent)
- [ ] Plugin capabilities are declared and enforced (no ambient authority)
- [ ] MCP server connections are authenticated and timeout-bounded

## Session Management

- [ ] Sessions have a maximum lifetime (TTL)
- [ ] Disconnected sessions are cleaned up on a schedule
- [ ] Session IDs are generated with cryptographic randomness
- [ ] Session history is bounded (prevent memory exhaustion)
- [ ] Concurrent session limits are configurable per user

## Monitoring & Incident Response

- [ ] Security events (injection attempts, auth failures) are logged
- [ ] Log output redacts sensitive tokens automatically
- [ ] Alerting is configured for repeated security events
- [ ] Agent can be stopped remotely (kill switch)
- [ ] Audit trail includes session ID, channel, user, and timestamp
- [ ] Configuration changes are logged
