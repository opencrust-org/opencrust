# AI Agent Attack Surfaces

AI agents introduce unique attack surfaces beyond traditional web applications. This document maps the primary threats and references industry research.

## OWASP LLM Top 10

The [OWASP Top 10 for LLM Applications](https://owasp.org/www-project-top-10-for-large-language-model-applications/) identifies the most critical risks:

| # | Risk | Agent Relevance |
|---|------|-----------------|
| LLM01 | Prompt Injection | **Critical**  - agents execute tools based on LLM output; injected instructions can trigger unintended actions |
| LLM02 | Insecure Output Handling | High  - agent responses may be rendered in chat UIs or forwarded to other systems |
| LLM03 | Training Data Poisoning | Medium  - primarily a provider-side risk, but affects agent behavior |
| LLM04 | Model Denial of Service | High  - large inputs can exhaust context windows and provider budgets |
| LLM05 | Supply Chain Vulnerabilities | High  - MCP servers, plugins, and skills are all supply chain vectors |
| LLM06 | Sensitive Information Disclosure | **Critical**  - agents access API keys, user data, and internal systems |
| LLM07 | Insecure Plugin Design | High  - plugins with ambient authority can be exploited via prompt injection |
| LLM08 | Excessive Agency | **Critical**  - agents with tool access can take real-world actions (shell commands, file writes, API calls) |
| LLM09 | Overreliance | Medium  - users may trust agent output without verification |
| LLM10 | Model Theft | Low  - agents use hosted models via API |

## Prompt Injection

The most critical and least-solved attack surface for AI agents.

**Direct injection:** Attacker crafts input that overrides the system prompt, causing the agent to ignore instructions, reveal configuration, or execute unintended tools.

**Indirect injection:** Malicious content embedded in data the agent processes (web pages fetched by tools, documents read from disk, MCP server responses) that hijacks agent behavior.

**Mitigations:**
- Pattern-based input filtering (14 patterns in OpenCrust)
- System prompt hardening with explicit boundary instructions
- Tool result sandboxing and size limits
- Human-in-the-loop for destructive operations

**References:**
- Greshake et al., "Not what you've signed up for: Compromising Real-World LLM-Integrated Applications with Indirect Prompt Injection" (2023)
- Simon Willison, "Prompt injection explained" (2023)

## Tool Abuse

Agents with tool access can be manipulated into:

- **Shell injection:** Running arbitrary commands via bash tools
- **File exfiltration:** Reading sensitive files and including content in responses
- **SSRF:** Fetching internal URLs via web fetch tools
- **Recursive scheduling:** Creating infinite task loops via scheduling tools

**Mitigations:**
- Tool iteration limits (max 10 round-trips)
- Recursive scheduling prevention (heartbeat context blocks re-scheduling)
- File path validation and sandboxing
- Private IP blocking for outbound requests

## Credential Leakage

AI agents are high-value targets because they aggregate credentials:

- LLM provider API keys (Anthropic, OpenAI)
- Channel bot tokens (Discord, Telegram, Slack)
- Webhook secrets (WhatsApp)
- User pairing codes

**Attack vectors:**
- Plaintext config files accessible to local users or leaked in backups
- Log output containing API keys (accidental logging)
- Prompt injection exfiltrating credentials via tool responses
- Memory/history containing credentials from prior conversations

**Mitigations:**
- Encrypted vault (AES-256-GCM) for all credentials
- Automatic log redaction for known key patterns
- Input sanitization preventing credential injection
- Session history bounded and cleaned up

## Supply Chain

### MCP Servers

MCP servers are external processes that the agent trusts to provide tools. A compromised MCP server can:

- Return malicious tool schemas that trick the LLM into dangerous actions
- Exfiltrate data passed as tool arguments
- Exploit the agent host via process-level access (stdio transport)

### Plugins (WASM)

Despite WASM sandboxing, plugins still present risks:

- Excessive host function imports granting unintended capabilities
- Resource exhaustion (memory, CPU)
- Side-channel attacks (timing)

### Skills

Skills are Markdown files injected into the system prompt. A malicious skill can:

- Override agent behavior via prompt injection in the skill body
- Introduce tool-use patterns that exfiltrate data

## Industry Research

- **Belgium CCB (2024):** Guidelines on securing AI systems, emphasizing input validation and output filtering for LLM-integrated applications.
- **Dutch DPA (2024):** Guidance on AI and GDPR, covering data minimization requirements relevant to agent memory and logging.
- **Sophos (2024):** "The lethal trifecta"  - compromised credentials, tool abuse, and prompt injection as the three converging attack vectors against AI agents.
- **SecurityScorecard (2025):** Supply chain risk analysis showing third-party integrations (MCP servers, plugins) as the fastest-growing attack surface for AI applications.
