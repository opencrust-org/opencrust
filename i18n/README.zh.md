<p align="center">
  <img src="../assets/logo.png" alt="OpenCrust" width="280" />
</p>

<h1 align="center">OpenCrust</h1>

<p align="center">
  <strong>安全、轻量的开源 AI 代理框架。</strong>
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
  <a href="#快速开始">快速开始</a> &middot;
  <a href="#为什么选择-opencrust">为什么选择 OpenCrust?</a> &middot;
  <a href="#功能特性">功能特性</a> &middot;
  <a href="#安全">安全</a> &middot;
  <a href="#架构">架构</a> &middot;
  <a href="#从-openclaw-迁移">从 OpenClaw 迁移</a> &middot;
  <a href="#贡献">贡献</a>
</p>

<p align="center">
  <a href="../README.md">🇺🇸 English</a> &middot;
  <a href="README.th.md">🇹🇭 ไทย</a> &middot;
  🇨🇳 <strong>简体中文</strong> &middot;
  <a href="README.hi.md">🇮🇳 हिन्दी</a>
</p>

---

一个仅 16 MB 的独立二进制文件，支持在 Telegram、Discord、Slack、WhatsApp、WhatsApp Web、LINE、微信、iMessage 和 MQTT 上运行您的 AI 代理。拥有加密的凭据存储、配置热重载，空闲时仅占用 13 MB 内存。基于 Rust 构建，旨在为 AI 代理提供所需的安全性与可靠性。

## 快速开始

```bash
# 安装 (Linux, macOS)
curl -fsSL https://raw.githubusercontent.com/opencrust-org/opencrust/main/install.sh | sh

# 交互式初始化 - 选择您的 LLM 供应商和渠道
opencrust init

# 启动 - 在收到第一条消息时，代理将进行自我介绍并学习您的偏好
opencrust start

# 诊断配置、连接性和数据库状态
opencrust doctor
```

<details>
<summary>源码编译</summary>

```bash
# 需要 Rust 1.85+
cargo build --release
./target/release/opencrust init
./target/release/opencrust start

# 可选：包含 WASM 插件支持
cargo build --release --features plugins
```
</details>

### 终端聊天

直接在终端与 agent 对话，无需打开浏览器。

> **需要先启动 gateway。** 首次使用请运行 `opencrust init`，然后运行 `opencrust start`，再使用 `opencrust chat`。

```bash
# 首次设置
opencrust init
opencrust start           # 或：opencrust start -d  (后台模式)

# 启动终端聊天
opencrust chat
opencrust chat --agent coder           # 使用指定 agent
opencrust chat --url http://host:3888  # 连接远程 gateway
```

```
╭─── OpenCrust Chat v0.2.9 ──────────────────────╮
│                                                │
│         _~^~^~_                                │
│     \) /  o o  \ (/                            │
│       '_   -   _'                              │
│       / '-----' \                              │
│                                                │
│   Gateway  http://127.0.0.1:3888               │
│   Agent    default                             │
│                                                │
│   Type /help for commands                      │
│                                                │
╰────────────────────────────────────────────────╯

you › Go channel 和 Rust async 有什么区别？
bot › Go channel 是语言内置功能 — goroutine 通过类型化 channel
      (make(chan int, 5)) 传递数据。Rust async 使用
      Future + tokio::sync::mpsc 实现类似模式，但所有权系统
      在编译期就能防止数据竞争，无需 unsafe。

you › /agent coder
Switched to agent: coder.

you › 给我一个 rust mpsc 示例
bot › use tokio::sync::mpsc;

      #[tokio::main]
      async fn main() {
          let (tx, mut rx) = mpsc::channel(8);
          tokio::spawn(async move { tx.send(42).await.unwrap(); });
          println!("{}", rx.recv().await.unwrap()); // 42
      }

you › /exit
Goodbye!
```

**聊天命令：** `/help` · `/new`（新建会话）· `/agent <id>` · `/clear` · `/exit`

### 网页聊天

Gateway 启动后，在浏览器中打开：

```
http://127.0.0.1:3888
```

内置 Web UI 让你与 agent 对话、实时切换 LLM provider、管理 MCP server 并监控已连接的 channel — 无需重启。

> **身份验证** — 若 `config.yml` 中设置了 `api_key`，UI 将在连接前提示输入 gateway key。

适用于 Linux (x86_64, aarch64)、macOS (Intel, Apple Silicon) 和 Windows (x86_64) 的预编译二进制文件。可在 [GitHub Releases](https://github.com/opencrust-org/opencrust/releases) 下载。

## 为什么选择 OpenCrust?

### 与 OpenClaw、ZeroClaw 等 AI 代理框架对比

| | **OpenCrust** | **OpenClaw** (Node.js) | **ZeroClaw** (Rust) |
|---|---|---|---|
| **二进制文件大小** | 16 MB | ~1.2 GB (包含 node_modules) | ~25 MB |
| **空闲状态内存** | 13 MB | ~388 MB | ~20 MB |
| **冷启动速度** | 3 ms | 13.9 s | ~50 ms |
| **凭据存储方式** | AES-256-GCM 加密库 | 明文配置文件 | 明文配置文件 |
| **默认身份验证** | 已启用 (WebSocket 配对) | 默认禁用 | 默认禁用 |
| **任务调度** | Cron, 间隔, 单次执行 | 是 | 否 |
| **多代理路由** | 是 (命名代理) | 是 (agentId) | 否 |
| **会话编排** | 是 | 是 | 否 |
| **MCP 支持** | Stdio + HTTP | Stdio + HTTP | Stdio |
| **渠道数量** | 9 | 6+ | 4 |
| **LLM 供应商数量** | 15 | 10+ | 22+ |
| **预编译二进制文件** | 是 | 无 (Node.js) | 源码编译 |
| **配置热重载** | 是 | 否 | 否 |
| **WASM 插件系统** | 可选 (沙盒隔离) | 否 | 否 |
| **自动更新** | 是 (`opencrust update`) | npm | 源码编译 |

*性能基准测试在 1 vCPU, 1 GB RAM 的 DigitalOcean Droplet 上进行。*

## 安全

OpenCrust 专门为需要访问私有数据并进行外部通信的“全天候” AI 代理设计。

- **加密凭据库** - API 密钥和令牌使用 AES-256-GCM 加密存储在 `~/.opencrust/credentials/vault.json`。磁盘上从不存储明文。
- **默认开启身份验证** - WebSocket 网关需要配对码。开箱即用，无未授权访问。
- **基于渠道的授权策略** - 支持私信策略（开放、配对、白名单）和群组策略（开放、仅提及时回复、禁用）。未经授权的消息将被静默丢弃。
- **提示词注入检测** - 在内容到达 LLM 之前进行输入验证和清洗。
- **频率限制** - 基于滑动窗口的用户级消息频率限制，配合可配置冷却时间，防止滥用。
- **Token 预算** - 按 session、每日和每月设置 token 上限，控制每位用户的 LLM 成本。
- **工具白名单** - 限制 agent 在每个 session 中可调用的工具，并设置调用次数上限。
- **敏感信息屏蔽** - 自动从日志输出中隐藏 API 密钥和令牌。
- **WASM 沙盒隔离** - 可选的 WebAssembly 插件沙盒，严格控制主机访问权限（通过 `--features plugins` 编译）。
- **默认仅绑定 localhost** - 网关默认绑定到 `127.0.0.1`，而非 `0.0.0.0`。

## 功能特性

### LLM 供应商

**原生支持：**

- **Anthropic Claude** - 流式输出 (SSE), 工具调用
- **OpenAI** - GPT-4o, Azure, 以及任何兼容 OpenAI 的 `base_url` 端点
- **Ollama** - 本地模型支持流式输出

**OpenAI 兼容供应商：**

- **Sansa** - 通过 [sansaml.com](https://sansaml.com) 访问地区级 LLM
- **DeepSeek** - DeepSeek Chat
- **Mistral** - Mistral Large
- **Gemini** - 通过 OpenAI 兼容 API 访问 Google Gemini
- **Falcon** - TII Falcon 180B (AI71)
- **Jais** - Core42 Jais 70B
- **Qwen** - 阿里巴巴通义千问 Qwen Plus
- **Yi** - 零一万物 Yi Large
- **Cohere** - Command R Plus
- **MiniMax** - MiniMax Text 01
- **Moonshot** - 月之暗面 Kimi K2
- **vLLM** - 通过 vLLM 的 OpenAI 兼容服务器自托管模型

### 语音 I/O

- **TTS（文本转语音）** — Kokoro（通过 kokoro-fastapi 自托管）、OpenAI TTS（`tts-1`、`tts-1-hd`）、任何兼容 OpenAI 的端点
- **STT（语音转文本）** — 本地 Whisper（faster-whisper-server）、OpenAI Whisper API
- `auto_reply_voice: true` 自动将每条文本回复合成为语音
- `tts_max_chars` 限制合成长度，超出时截断并记录警告
- 按渠道分发：Discord（文件附件）、微信（客服语音 API）、Telegram/LINE（原生音频）、Slack（回退为文本）

### 渠道

- **Telegram** - 流式响应、MarkdownV2、机器人指令、正在输入提示、带配对码的用户白名单、图片/多模态支持、语音消息 (Whisper STT)、TTS 自动语音回复、文档/文件处理
- **Discord** - 斜杠指令、事件驱动消息处理、会话管理、语音回复（TTS 文件附件）
- **Slack** - Socket Mode、流式响应、白名单/配对
- **WhatsApp** - Meta Cloud API Webhooks、白名单/配对
- **WhatsApp Web** - 通过 Baileys Node.js 端的 QR 码配对，无需 Meta 商业账号，身份验证状态持久化
- **iMessage** - 通过 chat.db 轮询的原生 macOS 支持、群聊支持、AppleScript 发送 ([设置指南](../docs/src/channels/imessage.md))
- **LINE** - Messaging API Webhooks、回复/推送回退、群组/通话支持、白名单/配对、语音回复（TTS，回退为文本）
- **WeChat (微信)** - 公众号平台 Webhooks、SHA-1 签名验证、同步 XML 回复、图片/语音/视频/位置消息分发、客服 API 推送、语音消息（TTS）、白名单/配对
- **MQTT** - 原生 MQTT Broker 客户端（Mosquitto、EMQX、HiveMQ），Mode A（纯文本，每频道一个会话）与 Mode B（JSON `{"user_id","text"}`，每设备独立会话），自动检测、指数退避重连、QoS 0/1/2、支持 TLS（`mqtts://`）

### MCP (模型上下文协议)
- 连接任何兼容 MCP 的服务器 (文件系统、GitHub、数据库、网络搜索)
- 工具以命名空间形式显示为原生代理工具 (`server_tool`)
- LLM 可以按需列出和读取 MCP 服务器资源
- 从握手中捕获服务器指令并附加到系统提示词中
- 具备 30 秒心跳检测和自动重连的监控
- 在 `config.yml` 或 `~/.opencrust/mcp.json` (兼容 Claude Desktop) 中配置
- CLI 指令: `opencrust mcp list`, `opencrust mcp inspect <name>`, `opencrust mcp resources <name>`

### 代理个性化 (DNA)
- 在收到第一条消息时，代理会进行自我介绍并询问几个问题以学习您的偏好
- 自动生成 `~/.opencrust/dna.md`，包含您的姓名、沟通风格、准则以及机器人的自我身份
- 无需手动编辑配置文件，无需填写初始化向导 —— 仅需通过对话完成
- 编辑后即时热重载 —— 修改 `dna.md`，代理会即刻适应
- 从 OpenClaw 迁移？`opencrust migrate openclaw` 将导入您现有的 `SOUL.md`

### 代理运行时
- 工具执行循环 —— 支持 bash, file_read, file_write, web_fetch, web_search (Brave 或 Google Custom Search), doc_search, handoff, schedule_heartbeat, cancel_heartbeat, list_heartbeats, mcp_resources 等 (最高 10 次迭代)
- 基于 SQLite 的对话记忆，支持向量搜索 (sqlite-vec + Cohere embeddings)
- 上下文窗口管理 —— 在上下文达到 75% 时自动进行滚动摘要
- 调度任务 —— 支持 Cron、间隔和单次任务调度

### 技能
- 以带有 YAML 元数据的 Markdown 文件 (SKILL.md) 定义代理技能
- 从 `~/.opencrust/skills/` 自动发现并注入系统提示词
- 热重载 —— `create_skill` 或 `skill install` 后技能立即生效，无需重启
- CLI 指令: `opencrust skill list`, `opencrust skill install <url|path>`, `opencrust skill remove <name>`
- **自主学习** —— agent 在完成 3 次以上工具调用后，会主动考虑保存可复用的工作流；提示信息显示在回复末尾
- 在 `config.yml` 中设置 `agent.self_learning: false` 可禁用此功能
- 三层质量控制：提示词引导、机械限制（最多 30 个技能、最小正文长度、重复检测）以及必填的 `rationale` 字段（存储于技能文件中以供审计）

### 多智能体编排

在 `config.yml` 中定义命名 agent，并通过内置 `handoff` 工具在 agent 之间路由任务：

```yaml
agents:
  router:
    provider: main                    # 使用哪个 llm: 配置项
    system_prompt: |
      分析用户请求并使用 handoff 工具委派：
      - handoff(agent_id='coder')     用于代码、脚本、编程
      - handoff(agent_id='assistant') 用于一般问题
      始终使用 handoff — 不要直接回答。

  coder:
    provider: main
    system_prompt: 你是一名专业编程 agent，回答简洁。
    tools: [bash, file_read, file_write]  # 限制此 agent 可调用的工具
    dna_file: dna-coder.md               # 可选：agent 专属人格文件
    skills_dir: skills/coder/            # 可选：agent 专属技能目录

  assistant:
    provider: main
    system_prompt: 你是一名有帮助的通用助手。
    max_tokens: 2048
    max_context_tokens: 32000
```

**`agents:` 配置字段：**

| 字段 | 类型 | 说明 |
|---|---|---|
| `provider` | string | 使用哪个 `llm:` 配置键（如 `main`、`claude`）。默认为首个注册的 provider。 |
| `model` | string | 仅限此 agent 的模型覆盖。 |
| `system_prompt` | string | 此 agent 专属系统提示词（替换全局设置）。 |
| `max_tokens` | int | 此 agent 的最大响应 token 数。 |
| `max_context_tokens` | int | 此 agent 的上下文窗口上限。 |
| `tools` | list | 工具白名单。空列表 = 允许所有工具。 |
| `dna_file` | path | agent 专属 DNA/人格文件路径（覆盖全局 `dna.md`）。 |
| `skills_dir` | path | agent 专属技能目录路径（覆盖全局 `skills/`）。 |

**Handoff 工具：**

`handoff` 工具对所有 agent 自动可用。调用时，它会在独立的临时 session 中运行目标 agent 并返回结果：

```
handoff(agent_id="coder", message="用 Python 写一个 fibonacci 函数")
# → "[coder]: 以下是实现代码…"
```

- 深度限制为 3 层，防止 A→B→A 无限循环
- 每次 handoff 使用独立 session，agent 之间无历史记录泄漏
- 目标 agent 继承其自身的 tools、DNA 和 skills 覆盖配置

**通过 API 使用：**

创建绑定到特定 agent 的 session，后续消息自动使用该 agent：

```bash
# 创建绑定到 "router" agent 的 session
SESSION=$(curl -s -X POST http://localhost:3888/api/sessions \
  -H "X-API-Key: your-key" \
  -H "Content-Type: application/json" \
  -d '{"agent_id": "router"}' | jq -r '.session_id')

# 此 session 中的所有消息自动经过 router
curl -X POST "http://localhost:3888/api/sessions/$SESSION/messages" \
  -H "X-API-Key: your-key" \
  -H "Content-Type: application/json" \
  -d '{"content": "用 Python 写 hello world"}'
```

### 基础设施
- **配置热重载** - 编辑 `config.yml` 变更即时生效，无需重启
- **守护进程管理** - `opencrust start --daemon` 自动管理 PID
- **自动更新** - `opencrust update` 下载最新发布版本并进行 SHA-256 验证，`opencrust rollback` 即可回档
- **重启** - `opencrust restart` 优雅停用并重启守护进程
- **运行时动态切换供应商** - 无需重启，通过 WebChat UI 或 REST API 添加或切换 LLM 供应商
- **迁移工具** - `opencrust migrate openclaw` 导入技能、渠道和凭据
- **对话摘要** - 75% 上下文自动总结，会话摘要跨重启持久化
- **诊断指令** - `opencrust doctor` 检查配置、数据目录、凭据库、供应商连通性、渠道凭据、MCP 连接及数据库完整性

## 从 OpenClaw 迁移?

只需一个指令即可导入您的技能、渠道配置、凭据（加密存入库）和个性化设置 (`SOUL.md` 转为 `dna.md`):

```bash
opencrust migrate openclaw
```

使用 `--dry-run` 可以在执行前预览变更。使用 `--source /path/to/openclaw` 指定自定义的 OpenClaw 配置路径。

## 配置

OpenCrust 默认搜索路径为 `~/.opencrust/config.yml`:

```yaml
gateway:
  host: "127.0.0.1"
  port: 3888
  # api_key: "your-secret-key"  # 可选：公开部署时保护 /api/* 端点
                                 # 生成命令：openssl rand -hex 32

llm:
  claude:
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    # api_key 解析顺序: vault > config > ANTHROPIC_API_KEY env var

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
    channel_access_token: "your-access-token"  # 或使用 LINE_CHANNEL_ACCESS_TOKEN 环境变量
    channel_secret: "your-secret"              # 或使用 LINE_CHANNEL_SECRET 环境变量
    dm_policy: pairing     # open | pairing | allowlist（默认：pairing）
    group_policy: mention  # open | mention | disabled（默认：open）

agent:
  # 个性化设置通过 ~/.opencrust/dna.md 配置 (首条消息后自动创建)
  max_tokens: 4096
  max_context_tokens: 100000

guardrails:
  max_input_chars: 16000            # 超过此长度的消息将被拒绝（默认: 16000）
  max_output_chars: 32000           # 超过此长度的回复将被截断（默认: 32000）
  token_budget_session: 10000       # 每 session 最大 token 数
  token_budget_user_daily: 100000   # 每用户每天最大 token 数
  token_budget_user_monthly: 500000 # 每用户每月最大 token 数
  allowed_tools:                    # null = 允许所有工具；[] = 禁止所有工具
    - web_search
    - file_read
  session_tool_call_budget: 15      # 每 session 最大工具调用次数

gateway:
  rate_limit:
    max_messages_per_minute: 10     # 每用户每分钟消息数限制
    cooldown_seconds: 30            # 超限后的冷却时间

memory:
  enabled: true

# 用于外部工具的 MCP 服务器
mcp:
  filesystem:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
  remote-server:
    transport: http
    url: "https://mcp.example.com/sse"
```

查看 [完整配置参考](../docs/) 获取更多选项，包括 Discord, Slack, WhatsApp, iMessage, Embeddings 和 MCP 服务器设置。

## 架构

```
crates/
  opencrust-cli/        # CLI, 初始化向导, 守护进程管理
  opencrust-gateway/    # WebSocket 网关, HTTP API, 会话管理
  opencrust-config/     # YAML/TOML 加载, 热重载, MCP 配置
  opencrust-channels/   # Discord, Telegram, Slack, WhatsApp, WhatsApp Web, iMessage, LINE, WeChat, MQTT
  opencrust-agents/     # LLM 供应商, 工具, MCP 客户端, 代理运行时
  opencrust-db/         # SQLite 记忆, 向量搜索 (sqlite-vec)
  opencrust-plugins/    # WASM 插件沙盒 (wasmtime)
  opencrust-media/      # TTS (Kokoro, OpenAI)、STT (Whisper)、媒体处理
  opencrust-security/   # 凭据库, 白名单, 配对, 验证
  opencrust-skills/     # SKILL.md 解析器, 扫描器, 安装器
  opencrust-common/     # 共享类型, 错误映射, 工具集
```

| 组件 | 状态 |
|-----------|--------|
| 网关 (WebSocket, HTTP, 会话) | 正常工作 |
| Telegram (流式响应, 指令, 视觉/语音支持) | 正常工作 |
| Discord (斜杠指令, 会话) | 正常工作 |
| Slack (Socket Mode, 流式响应) | 正常工作 |
| WhatsApp (Webhooks) | 正常工作 |
| WhatsApp Web (Baileys 客户端) | 正常工作 |
| iMessage (macOS, 群聊支持) | 正常工作 |
| LINE (回复/推送回退) | 正常工作 |
| WeChat 微信 (公众号 Webhooks, 媒体消息分发) | 正常工作 |
| MQTT（Broker 客户端，Mode A/B 自动检测，重连，QoS 0/1/2） | 正常工作 |
| LLM 供应商 (15 种) | 正常工作 |
| 代理工具 (bash, file_read, file_write, web_fetch, web_search, doc_search, schedule_heartbeat 等) | 正常工作 |
| MCP 客户端 (stdio, HTTP, 资源及指令) | 正常工作 |
| A2A 协议 (Agent-to-Agent) | 正常工作 |
| 多代理路由 | 正常工作 |
| 技能 (SKILL.md 自动发现) | 正常工作 |
| 配置 (YAML/TOML 热重载) | 正常工作 |
| 代理 DNA (即时更新) | 正常工作 |
| 记忆管理 (SQLite, 向量化, 摘要) | 正常工作 |
| 安全 (库加密, 白名单, 策略管理) | 正常工作 |
| 调度 (Cron, 间隔) | 正常工作 |
| CLI (全套管理指令) | 正常工作 |
| 插件系统 (WASM 沙盒) | 已搭建脚手架 |
| TTS (Kokoro, OpenAI) + STT (Whisper, OpenAI) | 正常工作 |

## 贡献

OpenCrust 遵循 MIT 开源协议。加入 [Discord](https://discord.gg/97jTJEUz4) 进行交流、提问或分享您的作品。查看 [CONTRIBUTING.md](../CONTRIBUTING.md) 获取开发环境设置、代码准则和模块概述。

## 贡献者

<a href="https://github.com/opencrust-org/opencrust/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=opencrust-org/opencrust" />
</a>

## 许可证

MIT
