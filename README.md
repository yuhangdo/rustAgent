# Rust Agent Lite

一个独立的 Rust agent 子项目，目标是用尽量少的代码把下面几块真正接起来：

- OpenAI-compatible Chat Completions provider
- CLI 和 Telegram 两种入口
- session / memory / skills
- 基础工具：`bash`、`read_file`、`write_file`、`web_fetch`

它不是 OpenClaw 全量功能的 Rust 重写版，而是一个可以单独运行、便于本机实验和集成验证的最小内核。

## 目录

- `src/main.rs`: 程序入口，支持 `cli` / `once` / `telegram`
- `src/agent.rs`: agent 主循环，负责 tool calling
- `src/openai.rs`: OpenAI-compatible `/chat/completions` 客户端
- `src/memory.rs`: session、文件记忆、SQLite 向量记忆、历史压缩
- `src/tools.rs`: `bash` / `read_file` / `write_file` / `web_fetch`
- `src/skills.rs`: skills 加载和匹配
- `src/telegram.rs`: Telegram polling adapter
- `run-kimi.sh`: 本机快速启动脚本

## 运行要求

- Rust stable toolchain
- 一个可用的 OpenAI-compatible API key
- 可选：`TELEGRAM_BOT_TOKEN`

当前已验证可直接运行的提供方组合：

- Moonshot / Kimi
- `OPENAI_BASE_URL=https://api.moonshot.cn/v1`
- `OPENAI_MODEL=moonshot-v1-auto`

说明：

- 这个项目通过 OpenAI-compatible `/chat/completions` 接口调用模型
- 并不是只支持 OpenAI 官方
- 只要你的提供方兼容这套接口，通常都可以接

## 快速开始

### 1. 编译

在 `rust-agent/` 目录下运行：

```bash
cargo build
```

编译完成后，二进制会在：

```text
target/debug/rust-agent-lite
```

### 2. 配置本地环境

推荐在 `rust-agent/` 目录下创建一个本地文件 `.env.local`：

```bash
OPENAI_API_KEY=your_api_key
OPENAI_BASE_URL=https://api.moonshot.cn/v1
OPENAI_MODEL=moonshot-v1-auto
```

如果你需要 Telegram，再额外加上：

```bash
TELEGRAM_BOT_TOKEN=your_telegram_bot_token
```

可选配置：

```bash
AGENT_WORKDIR=/path/to/workspace
AGENT_DATA_DIR=/path/to/data
AGENT_SKILLS_DIRS="/path/to/skills:/another/skills"
OPENAI_TIMEOUT_SECS=60
AGENT_CONTEXT_BUDGET=18000
AGENT_MAX_TOOL_ROUNDTRIPS=6
AGENT_MAX_FILE_CHARS=8000
AGENT_MAX_WEB_CHARS=8000
AGENT_MEMORY_TOP_K=4
```

默认行为：

- `AGENT_WORKDIR` 默认是当前仓库根目录
- `AGENT_DATA_DIR` 默认是 `rust-agent/.agent-data`
- `AGENT_SKILLS_DIRS` 默认扫描：
  - 仓库根 `skills/`
  - `rust-agent/examples/skills/`

### 3. 启动 CLI

如果你已经写好了 `.env.local`，最方便的方式是：

```bash
./run-kimi.sh
```

这个脚本会：

- 自动加载 `rust-agent/.env.local`
- 检查 `OPENAI_API_KEY`
- 启动：

```bash
./target/debug/rust-agent-lite cli --session agent:main:main
```

如果你不想用脚本，也可以手动运行：

```bash
export OPENAI_API_KEY=your_api_key
export OPENAI_BASE_URL=https://api.moonshot.cn/v1
export OPENAI_MODEL=moonshot-v1-auto
./target/debug/rust-agent-lite cli --session agent:main:main
```

### 4. 单次执行

适合 smoke test：

```bash
./target/debug/rust-agent-lite once "请只回复 ok"
```

### 5. 启动 Telegram

在 `.env.local` 或 shell 里设置好 `TELEGRAM_BOT_TOKEN` 后运行：

```bash
./target/debug/rust-agent-lite telegram
```

当前 Telegram 模式使用 polling，不是 webhook。

## CLI 命令

启动 CLI 后，内置命令有：

- `/help`
- `/reset`
- `/session`
- `/sessions`
- `/skills`
- `/skills <query>`
- `/memory <query>`
- `/exit`

这些命令的作用分别是：

- `/help`: 查看帮助
- `/reset`: 清空当前 session
- `/session`: 查看当前 session 最近消息
- `/sessions`: 列出本地 session
- `/skills`: 列出已加载 skills
- `/skills <query>`: 查看命中的 skills
- `/memory <query>`: 查看 memory 命中
- `/exit`: 退出 CLI

## Telegram 命令

Telegram 模式下内置：

- `/help`
- `/reset`
- `/session`
- `/skills`
- `/skills <query>`
- `/memory <query>`

## 设计说明

### 1. Context 组装

每次请求会组合这些上下文：

- 最近 session tail
- 较早历史的压缩摘要
- 文件记忆命中结果
- SQLite 向量记忆命中结果
- skills 命中结果

### 2. Context 压缩

这里不是再额外调用一个摘要模型，而是把较早轮次压缩成结构化摘要，只保留最近若干条完整对话。这样实现更简单，也更稳定。

### 3. 双层 Memory

文件层：

- 每轮 user / assistant 对话写入 `memory/files/YYYY-MM-DD.md`
- 用简单关键词做检索

SQLite 层：

- 每轮对话写入 SQLite
- 用本地哈希 embedding 建向量
- 在 Rust 侧做 cosine similarity 检索

这样不依赖额外 embedding provider，也能单独运行。

### 4. tools

当前内置工具：

- `bash`
- `read_file`
- `write_file`
- `web_fetch`

文件路径默认限制在 `AGENT_WORKDIR` 下。

### 5. skills

skills 从 markdown 文件加载。默认扫描：

- 仓库根 `skills/`
- `rust-agent/examples/skills/`

匹配规则：

- 如果 markdown 前几行有 `keywords: foo,bar`
- 就按关键词匹配
- 否则退化成按文件名分词匹配

## 常见问题

### 1. `OPENAI_API_KEY is required for the Rust agent`

说明没有设置 API key。  
检查 `.env.local` 或 shell 环境变量。

### 2. 可以用 Kimi 吗

可以，但建议使用：

```bash
OPENAI_BASE_URL=https://api.moonshot.cn/v1
OPENAI_MODEL=moonshot-v1-auto
```

原因：

- 这个项目当前会固定发送 `temperature=0.2`
- 某些 Kimi 模型会限制 temperature
- `moonshot-v1-auto` 已验证可用

### 3. 为什么不用 `kimi-k2.5`

当前 `src/openai.rs` 里请求体固定带：

```json
{
  "temperature": 0.2
}
```

而某些 Kimi 2.5 模型要求 temperature 必须是 `1`，所以会返回 400。

### 4. `.env.local` 会不会进 git

不会。  
`rust-agent/.gitignore` 已忽略 `.env.local`。

## 已知限制

- Telegram 只做 polling，不做 webhook
- SQLite vec 是“SQLite 持久化 + Rust 侧向量检索”，不是外部向量数据库
- 没有 OpenClaw 主项目那套完整的多租户权限和复杂沙箱
- 当前 provider 接口是 OpenAI-compatible chat completions，不是 Responses API
