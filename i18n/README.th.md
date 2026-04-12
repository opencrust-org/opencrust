<p align="center">
  <img src="../assets/logo.png" alt="OpenCrust" width="280" />
</p>

<h1 align="center">OpenCrust</h1>

<p align="center">
  <strong>เฟรมเวิร์ค AI Agent แบบ open-source ที่ปลอดภัยและเบาที่สุด</strong>
</p>

<p align="center">
  <a href="https://github.com/opencrust-org/opencrust/actions"><img src="https://github.com/opencrust-org/opencrust/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/opencrust-org/opencrust/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://github.com/opencrust-org/opencrust/stargazers"><img src="https://img.shields.io/github/stars/opencrust-org/opencrust?style=flat" alt="Stars"></a>
  <a href="https://github.com/opencrust-org/opencrust/issues"><img src="https://img.shields.io/github/issues/opencrust-org/opencrust" alt="Issues"></a>
  <a href="https://github.com/opencrust-org/opencrust/issues?q=label%3Agood-first-issue+is%3Aopen"><img src="https://img.shields.io/github/issues/opencrust-org/opencrust/good-first-issue?color=7057ff&label=good%20first%20issues" alt="Good First Issues"></a>
  <a href="https://discord.gg/97jTJEUz4"><img src="https://img.shields.io/badge/discord-join-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
</p>

<p align="center">
  <a href="#เริ่มต้นใช้งาน">เริ่มต้นใช้งาน</a> &middot;
  <a href="#ทำไมต้อง-opencrust">ทำไมต้อง OpenCrust?</a> &middot;
  <a href="#ฟีเจอร์">ฟีเจอร์</a> &middot;
  <a href="#ความปลอดภัย">ความปลอดภัย</a> &middot;
  <a href="#สถาปัตยกรรม">สถาปัตยกรรม</a> &middot;
  <a href="#ย้ายจาก-openclaw">ย้ายจาก OpenClaw</a> &middot;
  <a href="#การมีส่วนร่วม">การมีส่วนร่วม</a>
</p>

<p align="center">
  <a href="../README.md">🇺🇸 English</a> &middot;
  🇹🇭 <strong>ไทย</strong> &middot;
  <a href="README.zh.md">🇨🇳 简体中文</a> &middot;
  <a href="README.hi.md">🇮🇳 हिन्दी</a>
</p>

---

binary ขนาด 16 MB ที่รัน AI agent ของคุณผ่าน Telegram, Discord, Slack, WhatsApp, WhatsApp Web, LINE, WeChat และ iMessage — พร้อมการจัดเก็บ credential แบบเข้ารหัส, hot-reload config และใช้ RAM เพียง 13 MB ขณะ idle สร้างด้วย Rust เพื่อความปลอดภัยและความเสถียรที่ AI agent ต้องการ

## เริ่มต้นใช้งาน

```bash
# ติดตั้ง (Linux, macOS)
curl -fsSL https://raw.githubusercontent.com/opencrust-org/opencrust/main/install.sh | sh

# ตั้งค่าแบบ interactive - เลือก LLM provider และช่องทาง
opencrust init

# เริ่มใช้งาน - เมื่อได้รับข้อความแรก agent จะแนะนำตัวและเรียนรู้ความชอบของคุณ
opencrust start

# ตรวจสอบ config, connectivity และ database health
opencrust doctor
```

<details>
<summary>Build จาก source</summary>

```bash
# ต้องใช้ Rust 1.85+
cargo build --release
./target/release/opencrust init
./target/release/opencrust start

# รองรับ WASM plugin (optional)
cargo build --release --features plugins
```
</details>

binary สำหรับ Linux (x86_64, aarch64), macOS (Intel, Apple Silicon) และ Windows (x86_64) ดาวน์โหลดได้ที่ [GitHub Releases](https://github.com/opencrust-org/opencrust/releases)

## ทำไมต้อง OpenCrust?

### เทียบกับ OpenClaw, ZeroClaw และเฟรมเวิร์คอื่น

| | **OpenCrust** | **OpenClaw** (Node.js) | **ZeroClaw** (Rust) |
|---|---|---|---|
| **ขนาด binary** | 16 MB | ~1.2 GB (รวม node_modules) | ~25 MB |
| **RAM ขณะ idle** | 13 MB | ~388 MB | ~20 MB |
| **Cold start** | 3 ms | 13.9 s | ~50 ms |
| **เก็บ credential** | vault เข้ารหัส AES-256-GCM | plaintext config file | plaintext config file |
| **Auth ค่าเริ่มต้น** | เปิดใช้ (WebSocket pairing) | ปิดใช้ | ปิดใช้ |
| **Scheduling** | Cron, interval, one-shot | ใช่ | ไม่ |
| **Multi-agent routing** | ใช่ (named agents) | ใช่ (agentId) | ไม่ |
| **Session orchestration** | ใช่ | ใช่ | ไม่ |
| **MCP support** | Stdio + HTTP | Stdio + HTTP | Stdio |
| **ช่องทาง** | 9 | 6+ | 4 |
| **LLM provider** | 15 | 10+ | 22+ |
| **Pre-compiled binary** | ใช่ | N/A (Node.js) | Build จาก source |
| **Config hot-reload** | ใช่ | ไม่ | ไม่ |
| **WASM plugin system** | Optional (sandboxed) | ไม่ | ไม่ |
| **Self-update** | ใช่ (`opencrust update`) | npm | Build จาก source |

*วัดผลบน DigitalOcean droplet 1 vCPU, 1 GB RAM [ทดสอบเองได้](../bench/)*

## ความปลอดภัย

OpenCrust ถูกออกแบบสำหรับ AI agent ที่ทำงานตลอดเวลาและเข้าถึงข้อมูลส่วนตัว

- **Encrypted credential vault** - API key และ token จัดเก็บด้วย AES-256-GCM ที่ `~/.opencrust/credentials/vault.json` ไม่มี plaintext บนดิสก์
- **Authentication ค่าเริ่มต้น** - WebSocket gateway ต้องใช้ pairing code ไม่มีการเข้าถึงโดยไม่ผ่านการยืนยันตัวตน
- **User allowlist** - allowlist แยกตามช่องทางควบคุมว่าใครสามารถโต้ตอบกับ agent ได้ ข้อความที่ไม่ได้รับอนุญาตจะถูกละทิ้งโดยไม่แจ้ง
- **Per-channel authorization policies** - DM policy (open, pairing, allowlist) และ group policy (open, mention-only, disabled) แยกตามช่องทาง ข้อความที่ไม่ได้รับอนุญาตจะถูกละทิ้งโดยไม่แจ้ง
- **ตรวจจับ prompt injection** - ตรวจสอบและ sanitize input ก่อนส่งถึง LLM
- **Rate limiting** - จำกัดจำนวน message ต่อผู้ใช้แบบ sliding-window พร้อม cooldown ป้องกันการใช้งานเกินขีดจำกัด
- **Token budgets** - กำหนด token สูงสุดต่อ session, ต่อวัน และต่อเดือน เพื่อควบคุมต้นทุน LLM ต่อผู้ใช้
- **Tool allowlists** - จำกัด tool ที่ agent เรียกใช้ได้ต่อ session พร้อมกำหนดจำนวนครั้งสูงสุด
- **Log secret redaction** - API key และ token ถูก redact จาก log output อัตโนมัติ
- **WASM sandboxing** - sandbox plugin แบบ optional ผ่าน WebAssembly runtime (compile ด้วย `--features plugins`)
- **Bind เฉพาะ localhost** - gateway bind กับ `127.0.0.1` ค่าเริ่มต้น ไม่ใช่ `0.0.0.0`

## ฟีเจอร์

### LLM Provider

**Native providers:**

- **Anthropic Claude** - streaming (SSE), tool use
- **OpenAI** - GPT-4o, Azure, รองรับ endpoint แบบ OpenAI-compatible ผ่าน `base_url`
- **Ollama** - โมเดลในเครื่องพร้อม streaming

**OpenAI-compatible providers:**

- **Sansa** - regional LLM ผ่าน [sansaml.com](https://sansaml.com)
- **DeepSeek** - DeepSeek Chat
- **Mistral** - Mistral Large
- **Gemini** - Google Gemini ผ่าน OpenAI-compatible API
- **Falcon** - TII Falcon 180B (AI71)
- **Jais** - Core42 Jais 70B
- **Qwen** - Alibaba Qwen Plus
- **Yi** - 01.AI Yi Large
- **Cohere** - Command R Plus
- **MiniMax** - MiniMax Text 01
- **Moonshot** - Kimi K2
- **vLLM** - โมเดล self-hosted ผ่าน vLLM's OpenAI-compatible server

### เสียง I/O
- **TTS (Text-to-Speech)** — Kokoro (self-hosted ผ่าน kokoro-fastapi), OpenAI TTS (`tts-1`, `tts-1-hd`), รองรับ endpoint แบบ OpenAI-compatible
- **STT (Speech-to-Text)** — Whisper ในเครื่อง (faster-whisper-server), OpenAI Whisper API
- `auto_reply_voice: true` แปลงทุกข้อความตอบกลับเป็นเสียงอัตโนมัติ
- `tts_max_chars` จำกัดความยาวข้อความที่ส่งสังเคราะห์เสียง ตัดและแจ้งเตือนเมื่อเกินกำหนด
- ส่งแยกตามช่องทาง: Discord (ไฟล์แนบ), WeChat (Customer Service voice API), Telegram/LINE (เสียงแบบ native), Slack (ตกสำรองเป็นข้อความ)

### ช่องทาง
- **Telegram** - streaming responses, MarkdownV2, bot commands, typing indicators, user allowlist พร้อม pairing code, รองรับรูปภาพ/vision, voice message (Whisper STT), TTS ตอบกลับอัตโนมัติ, จัดการไฟล์/เอกสาร
- **Discord** - slash commands, event-driven message handling, session management, voice response (ไฟล์แนบ TTS)
- **Slack** - Socket Mode, streaming responses, allowlist/pairing
- **WhatsApp** - Meta Cloud API webhooks, allowlist/pairing
- **WhatsApp Web** - QR code pairing ผ่าน Baileys Node.js sidecar, ไม่ต้องมี Meta Business account, บันทึกสถานะ auth
- **LINE** - Messaging API webhooks, reply/push fallback, รองรับกลุ่ม/ห้องแชท, allowlist/pairing, voice response (TTS ตกสำรองเป็นข้อความ)
- **WeChat** - Official Account Platform webhooks, ตรวจสอบลายเซ็น SHA-1, ตอบกลับ XML แบบ synchronous, รองรับรูปภาพ/เสียง/วิดีโอ/ตำแหน่ง, Customer Service API push, voice message (TTS), allowlist/pairing
- **iMessage** - macOS native ผ่าน chat.db polling, group chat, AppleScript sending ([คู่มือตั้งค่า](../docs/imessage-setup.md))

### MCP (Model Context Protocol)
- เชื่อมต่อ MCP server ใดก็ได้ (filesystem, GitHub, databases, web search)
- รองรับทั้ง stdio และ HTTP transport
- tool ปรากฏเป็น native agent tool พร้อม namespace (`server.tool`)
- รองรับ resource tool และ server instructions
- ตั้งค่าใน `config.yml` หรือ `~/.opencrust/mcp.json` (รองรับ Claude Desktop)
- CLI: `opencrust mcp list`, `opencrust mcp inspect <name>`

### Personality (DNA)
- เมื่อได้รับข้อความแรก agent จะแนะนำตัวและถามคำถามเพื่อเรียนรู้ความชอบ
- เขียน `~/.opencrust/dna.md` พร้อมชื่อ รูปแบบการสื่อสาร แนวทาง และ identity ของ bot
- ไม่ต้องแก้ config file หรือกรอก wizard — แค่พูดคุย
- Hot-reload เมื่อแก้ไข — เปลี่ยน `dna.md` แล้ว agent ปรับตัวทันที
- ย้ายจาก OpenClaw? `opencrust migrate openclaw` นำเข้า `SOUL.md` ที่มีอยู่

### Agent Runtime
- Tool execution loop — bash, file_read, file_write, web_fetch, web_search (Brave หรือ Google Custom Search), doc_search, schedule_heartbeat, cancel_heartbeat, list_heartbeats, mcp_resources (สูงสุด 10 รอบ)
- หน่วยความจำบทสนทนาบน SQLite พร้อม vector search (sqlite-vec + Cohere embeddings)
- จัดการ context window — สรุปบทสนทนาแบบ rolling ที่ 75% ของ context window
- Scheduled task — cron, interval และ one-shot scheduling

### Skills
- กำหนด agent skill เป็นไฟล์ Markdown (SKILL.md) พร้อม YAML frontmatter
- auto-discovery จาก `~/.opencrust/skills/` — inject เข้า system prompt อัตโนมัติ
- CLI: `opencrust skill list`, `opencrust skill install <url>`, `opencrust skill remove <name>`

### Infrastructure
- **Config hot-reload** - แก้ `config.yml` แล้วการเปลี่ยนแปลงมีผลทันทีโดยไม่ต้อง restart
- **Daemonization** - `opencrust start --daemon` พร้อม PID management
- **Self-update** - `opencrust update` ดาวน์โหลด release ล่าสุดพร้อมตรวจสอบ SHA-256, `opencrust rollback` เพื่อย้อนกลับ
- **Restart** - `opencrust restart` หยุดและเริ่มใหม่แบบ graceful
- **Runtime provider switching** - เพิ่มหรือเปลี่ยน LLM provider ผ่าน webchat UI หรือ REST API โดยไม่ต้อง restart
- **Migration tool** - `opencrust migrate openclaw` นำเข้า skills, channels และ credentials
- **Conversation summarization** - rolling summary ที่ 75% context window, บันทึก session summary ข้ามการ restart
- **Interactive setup** - wizard `opencrust init` สำหรับตั้งค่า provider และช่องทาง
- **Diagnostics** - `opencrust doctor` ตรวจสอบ config, data directory, credential vault, ความสามารถเข้าถึง LLM provider, credentials ของ channel, ความสามารถเชื่อมต่อ MCP server และ database integrity

## ย้ายจาก OpenClaw?

คำสั่งเดียวนำเข้า skills, channel config, credentials (เข้ารหัสเข้า vault) และ personality (`SOUL.md` เป็น `dna.md`):

```bash
opencrust migrate openclaw
```

ใช้ `--dry-run` เพื่อดูตัวอย่างก่อน commit ใช้ `--source /path/to/openclaw` เพื่อระบุ directory config ของ OpenClaw

## การตั้งค่า

OpenCrust อ่าน config จาก `~/.opencrust/config.yml`:

```yaml
gateway:
  host: "127.0.0.1"
  port: 3888
  # api_key: "your-secret-key"  # ไม่บังคับ: ป้องกัน /api/* เมื่อเปิดให้เข้าถึงสาธารณะ
                                 # สร้างด้วย: openssl rand -hex 32

llm:
  claude:
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    # api_key อ่านจาก: vault > config > ANTHROPIC_API_KEY env var

  ollama-local:
    provider: ollama
    model: llama3.1
    base_url: "http://localhost:11434"

channels:
  telegram:
    type: telegram
    enabled: true
    bot_token: "your-bot-token"  # หรือ TELEGRAM_BOT_TOKEN env var

  line:
    type: line
    enabled: true
    channel_access_token: "your-access-token"  # หรือ LINE_CHANNEL_ACCESS_TOKEN env var
    channel_secret: "your-secret"              # หรือ LINE_CHANNEL_SECRET env var
    dm_policy: pairing     # open | pairing | allowlist (default: pairing)
    group_policy: mention  # open | mention | disabled (default: open)

agent:
  # Personality ตั้งค่าผ่าน ~/.opencrust/dna.md (สร้างอัตโนมัติเมื่อได้รับข้อความแรก)
  max_tokens: 4096
  max_context_tokens: 100000

guardrails:
  max_input_chars: 16000            # ปฏิเสธ message ที่ยาวเกิน (default: 16000)
  max_output_chars: 32000           # ตัด response ที่ยาวเกิน (default: 32000)
  token_budget_session: 10000       # tokens สูงสุดต่อ session
  token_budget_user_daily: 100000   # tokens สูงสุดต่อผู้ใช้ต่อวัน
  token_budget_user_monthly: 500000 # tokens สูงสุดต่อผู้ใช้ต่อเดือน
  allowed_tools:                    # null = ทุก tool; [] = ห้ามใช้ tool
    - web_search
    - file_read
  session_tool_call_budget: 15      # จำนวนครั้ง tool call สูงสุดต่อ session

gateway:
  rate_limit:
    max_messages_per_minute: 10     # จำกัด message ต่อผู้ใช้ต่อนาที
    cooldown_seconds: 30            # cooldown หลังเกินขีดจำกัด

memory:
  enabled: true

# MCP server สำหรับ external tools
mcp:
  filesystem:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
```

ดู [full configuration reference](../docs/) สำหรับ options ทั้งหมด รวมถึง Discord, Slack, WhatsApp, iMessage, embeddings และ MCP server

## สถาปัตยกรรม

```
crates/
  opencrust-cli/        # CLI, init wizard, daemon management
  opencrust-gateway/    # WebSocket gateway, HTTP API, sessions
  opencrust-config/     # YAML/TOML loading, hot-reload, MCP config
  opencrust-channels/   # Discord, Telegram, Slack, WhatsApp, WhatsApp Web, iMessage, LINE, WeChat
  opencrust-agents/     # LLM providers, tools, MCP client, agent runtime
  opencrust-db/         # SQLite memory, vector search (sqlite-vec)
  opencrust-plugins/    # WASM plugin sandbox (wasmtime)
  opencrust-media/      # TTS (Kokoro, OpenAI), STT (Whisper), การประมวลผลสื่อ
  opencrust-security/   # Credential vault, allowlists, pairing, validation
  opencrust-skills/     # SKILL.md parser, scanner, installer
  opencrust-common/     # Shared types, errors, utilities
```

| Component | สถานะ |
|-----------|--------|
| Gateway (WebSocket, HTTP, sessions) | ใช้งานได้ |
| Telegram (streaming, commands, pairing, photos, voice, documents) | ใช้งานได้ |
| Discord (slash commands, sessions) | ใช้งานได้ |
| Slack (Socket Mode, streaming) | ใช้งานได้ |
| WhatsApp (webhooks) | ใช้งานได้ |
| WhatsApp Web (QR code, Baileys sidecar) | ใช้งานได้ |
| LINE (webhooks, reply/push fallback) | ใช้งานได้ |
| WeChat (Official Account webhooks, media dispatch) | ใช้งานได้ |
| iMessage (macOS, group chats) | ใช้งานได้ |
| LLM providers (15: Anthropic, OpenAI, Ollama + 12 OpenAI-compatible) | ใช้งานได้ |
| Agent tools (bash, file_read, file_write, web_fetch, web_search, doc_search, schedule_heartbeat, cancel_heartbeat, list_heartbeats, mcp_resources) | ใช้งานได้ |
| MCP client (stdio, HTTP, tool bridging, resources, instructions) | ใช้งานได้ |
| A2A protocol (Agent-to-Agent) | ใช้งานได้ |
| Multi-agent routing (named agents) | ใช้งานได้ |
| Skills (SKILL.md, auto-discovery) | ใช้งานได้ |
| Config (YAML/TOML, hot-reload) | ใช้งานได้ |
| Personality (DNA bootstrap, hot-reload) | ใช้งานได้ |
| Memory (SQLite, vector search, summarization) | ใช้งานได้ |
| Security (vault, allowlist, pairing, per-channel policies, log redaction) | ใช้งานได้ |
| Scheduling (cron, interval, one-shot) | ใช้งานได้ |
| CLI (init, start/stop/restart, update, migrate, mcp, skills, doctor) | ใช้งานได้ |
| Plugin system (WASM sandbox) | Scaffolded |
| TTS (Kokoro, OpenAI) + STT (Whisper, OpenAI) | ใช้งานได้ |

## การมีส่วนร่วม

OpenCrust เป็น open source ภายใต้ MIT license เข้าร่วม [Discord](https://discord.gg/97jTJEUz4) เพื่อพูดคุยกับผู้ร่วมพัฒนา ถามคำถาม หรือแชร์สิ่งที่คุณกำลังสร้าง ดู [CONTRIBUTING.md](../CONTRIBUTING.md) สำหรับคำแนะนำการตั้งค่า แนวทางการเขียนโค้ด และ crate overview

### ลำดับความสำคัญปัจจุบัน

| ลำดับ | Issue | คำอธิบาย |
|-------|-------|-----------|
| **P0** | [#99](https://github.com/opencrust-org/opencrust/issues/99) | Brand facelift: logo, images, visual identity |
| **P1** | [#150](https://github.com/opencrust-org/opencrust/issues/150) | Fallback model chain: auto-retry with backup providers |
| **P1** | [#152](https://github.com/opencrust-org/opencrust/issues/152) | Token usage tracking and cost reporting |
| **P1** | [#153](https://github.com/opencrust-org/opencrust/issues/153) | `opencrust doctor` diagnostic command |
| **P1** | [#146](https://github.com/opencrust-org/opencrust/issues/146) | Guardrails: safety, rate limits, and cost controls |
| **P2** | [#185](https://github.com/opencrust-org/opencrust/issues/185) | MCP: Apps support (interactive HTML interfaces) |
| **P2** | [#158](https://github.com/opencrust-org/opencrust/issues/158) | Auto-backup config files before changes |
| **P2** | [#142](https://github.com/opencrust-org/opencrust/issues/142) | Web-based setup wizard at /setup |

ดู [issues ทั้งหมด](https://github.com/opencrust-org/opencrust/issues) หรือกรองด้วย [`good-first-issue`](https://github.com/opencrust-org/opencrust/issues?q=label%3Agood-first-issue+is%3Aopen) เพื่อหาจุดเริ่มต้น

## ผู้มีส่วนร่วม

<a href="https://github.com/opencrust-org/opencrust/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=opencrust-org/opencrust" />
</a>

## License

MIT
