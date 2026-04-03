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

一个仅 16 MB 的独立二进制文件，支持在 Telegram、Discord、Slack、WhatsApp、WhatsApp Web、LINE 和 iMessage 上运行您的 AI 代理。拥有加密的凭据存储、配置热重载，空闲时仅占用 13 MB 内存。基于 Rust 构建，旨在为 AI 代理提供所需的安全性与可靠性。

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
| **渠道数量** | 6 | 6+ | 4 |
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

### 渠道

- **Telegram** - 流式响应、MarkdownV2、机器人指令、正在输入提示、带配对码的用户白名单、图片/多模态支持、语音消息 (Whisper STT)、文档/文件处理
- **Discord** - 斜杠指令、事件驱动消息处理、会话管理
- **Slack** - Socket Mode、流式响应、白名单/配对
- **WhatsApp** - Meta Cloud API Webhooks、白名单/配对
- **WhatsApp Web** - 通过 Baileys Node.js 端的 QR 码配对，无需 Meta 商业账号，身份验证状态持久化
- **iMessage** - 通过 chat.db 轮询的原生 macOS 支持、群聊支持、AppleScript 发送 ([设置指南](../docs/src/channels/imessage.md))
- **LINE** - Messaging API Webhooks、回复/推送回退、群组/通话支持、白名单/配对

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
- 工具执行循环 —— 支持 bash, file_read, file_write, web_fetch, web_search, schedule_heartbeat 等 (最高 10 次迭代)
- 基于 SQLite 的对话记忆，支持向量搜索 (sqlite-vec + Cohere embeddings)
- 上下文窗口管理 —— 在上下文达到 75% 时自动进行滚动摘要
- 调度任务 —— 支持 Cron、间隔和单次任务调度

### 技能
- 以带有 YAML 元数据的 Markdown 文件 (SKILL.md) 定义代理技能
- 从 `~/.opencrust/skills/` 自动发现并注入系统提示词
- CLI 指令: `opencrust skill list`, `opencrust skill install <url>`, `opencrust skill remove <name>`

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
    channel_access_token: "your-access-token"
    channel_secret: "your-secret"

agent:
  # 个性化设置通过 ~/.opencrust/dna.md 配置 (首条消息后自动创建)
  max_tokens: 4096
  max_context_tokens: 100000

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
  opencrust-channels/   # Discord, Telegram, Slack, WhatsApp, iMessage, LINE
  opencrust-agents/     # LLM 供应商, 工具, MCP 客户端, 代理运行时
  opencrust-db/         # SQLite 记忆, 向量搜索 (sqlite-vec)
  opencrust-plugins/    # WASM 插件沙盒 (wasmtime)
  opencrust-media/      # 媒体处理 (脚手架)
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
| LLM 供应商 (15 种) | 正常工作 |
| 代理工具 (bash, 文件读写, 网络搜索等) | 正常工作 |
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
| 媒体处理 | 已搭建脚手架 |

## 贡献

OpenCrust 遵循 MIT 开源协议。加入 [Discord](https://discord.gg/97jTJEUz4) 进行交流、提问或分享您的作品。查看 [CONTRIBUTING.md](../CONTRIBUTING.md) 获取开发环境设置、代码准则和模块概述。

## 许可证

MIT
