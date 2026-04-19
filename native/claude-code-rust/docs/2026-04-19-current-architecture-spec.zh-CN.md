# Claude Code Rust 当前架构说明

## 1. 目的

本文描述 `native/claude-code-rust` 在当前 Android 项目中的实际形态。
在清理掉无关分发资产之后，这份文档用于给维护者说明当前目录结构、
`src` 下各模块的职责、主要实现方式，以及它们与 Android 集成链路的关系。

这是一份“现状说明文档”，不是重构计划。

## 2. 范围与清理边界

- `src/` 有意保持不动。
- `locales/` 被保留，因为 `src/i18n/loader.rs` 会在构建时嵌入该目录。
- `tests/` 被保留，因为它仍然用于 Rust 侧验证。
- 独立安装脚本、Docker 资产、发布 workflow、上游发布/迁移类文档已移除，
  因为当前项目是以 vendored dependency 的方式使用这个 crate，而不是把它当作独立产品分发。

## 3. 当前构建模式

### 3.1 本仓库实际使用的 Android 构建方式

当前 App 会用下面的方式构建 Rust Agent：

```bash
cargo build --lib --target aarch64-linux-android --no-default-features --features mobile-bridge
```

这意味着：

- 启用了 `mobile_bridge`。
- Android 库构建时禁用了默认特性，例如 `gui-egui` 和 `i18n`。
- 但仍有很多模块会一起参与编译，因为它们在 `src/lib.rs` 中是无条件声明的。

### 3.2 这个构建方式带来的直接结果

当前 crate 的能力范围明显大于 Android App 实际所需。
虽然真正的运行路径比较窄，但库的暴露面仍包含 CLI、memory、services、plugins、
MCP、terminal 等大量代码。因此这次清理只删除仓库外围资产，不去收缩 `src/`。

## 4. 当前 Android 执行链路

今天真正对 APK 生效的链路如下：

1. Kotlin 侧通过 `RustEmbeddedRuntimeBridge` 加载 `libclaude_code_rs.so`。
2. Native 库暴露 `mobile_bridge` 对应入口。
3. `src/mobile_bridge/mod.rs` 在本地进程内启动一个基于 Axum 的 localhost 服务。
4. Android App 调用 `/api/runs`、`/api/runs/{id}`、`/api/runs/{id}/cancel`、`/api/health`。
5. `MobileBridgeServer` 把请求载荷转换成 `AgentExecutionRequest`。
6. `src/agent_runtime.rs` 执行带工具调用能力的 agent loop。
7. `src/api/mod.rs` 向配置好的 provider 发起聊天请求。
8. `src/tools/` 在模型请求 tool call 时执行本地工具。
9. Bridge 再把 reasoning、工具事件、answer、失败状态、取消状态回传给 UI。

## 5. 根级源码文件

| 路径 | 角色 | 当前说明 |
| --- | --- | --- |
| `src/lib.rs` | crate 根与模块注册中心 | 大多数模块都在这里无条件声明；只对 `mobile_bridge`、`wasm`、`gui`、`web`、`i18n` 做了 feature gate。 |
| `src/main.rs` | CLI 可执行入口 | 负责加载 settings、创建 `AppState`、再交给 CLI runtime。Android App 不会走这个入口。 |
| `src/agent_runtime.rs` | 共享 agent loop | 当前 Android 链路中的核心运行时，负责模型调用、工具执行、reasoning 捕获、取消与最终答案组装。 |

## 6. 顶层模块清单

| 模块 | 主要文件 | 主要职责 | 当前实现情况 | 是否在 Android 主路径上 |
| --- | --- | --- | --- | --- |
| `advanced` | `mod.rs`, `ssh.rs`, `remote.rs`, `project_init.rs` | SSH、远程执行、项目初始化 | 上游偏“高级能力”的通用实现 | 否 |
| `api` | `mod.rs` | Provider client 与响应模型 | OpenAI 兼容的 HTTP client，支持 tool call 和 reasoning 字段透传 | 是 |
| `cli` | `mod.rs`, `args.rs`, `commands.rs`, `repl.rs`, `ui.rs` | CLI 命令解析、REPL、文本交互 | 保留了比较完整的 agent 产品化命令面 | 否 |
| `config` | `mod.rs`, `api_config.rs`, `mcp_config.rs`, `settings.rs` | 配置模型与配置持久化 | CLI 和 runtime 共用的 settings 层 | 部分相关 |
| `gui` | `mod.rs`, `app.rs`, `chat.rs`, `sidebar.rs`, `settings.rs`, `theme.rs`, `syntax_highlight.rs`, `tool_calls.rs`, `main.rs` | egui 桌面 GUI | feature-gated 的桌面 UI 能力 | 否 |
| `i18n` | `mod.rs`, `loader.rs`, `locales.rs`, `translator.rs` | 多语言与翻译加载 | feature-gated，依赖 `locales/` 资源 | 当前 Android 构建不走 |
| `mcp` | `mod.rs`, `tools.rs`, `resources.rs`, `prompts.rs`, `sampling.rs`, `server.rs`, `transport.rs` | MCP 协议原语与服务能力 | 结构较完整的 MCP scaffold | 否 |
| `memory` | `mod.rs`, `session.rs`, `history.rs`, `context.rs`, `storage.rs`, `consolidation.rs` | 记忆/会话持久化体系 | 独立且较完整的 memory 子系统 | 否 |
| `mobile_bridge` | `mod.rs`, `main.rs` | Android 嵌入使用的本地 HTTP bridge | 用 Axum 暴露 run 生命周期接口和 snapshot | 是 |
| `plugins` | `mod.rs`, `commands.rs`, `hooks.rs`, `loader.rs`, `isolation.rs`, `registry.rs` | 插件加载与隔离模型 | 保留了上游插件系统 | 否 |
| `services` | `mod.rs`, `agents.rs`, `auto_dream.rs`, `voice.rs`, `magic_docs.rs`, `team_memory_sync.rs`, `plugin_marketplace.rs`, `stress_tests.rs` | 更上层的后台/产品服务 | 非 Android 当前路径所需，但仍在 crate 中 | 否 |
| `session` | `mod.rs` | 会话级抽象 | `memory/` 之外的一层轻量 session 支撑 | 否 |
| `skills` | `mod.rs`, `registry.rs`, `executor.rs`, `builtin.rs` | 技能注册、执行、内建技能 | 更像 CLI/agent 产品工作流的能力层 | 否 |
| `state` | `mod.rs` | 共享内存态 | CLI 取向的 runtime state，对话和 tool registry 状态都在这里 | 否 |
| `terminal` | `mod.rs` | ratatui 终端壳层 | 简单的 terminal app wrapper | 否 |
| `tools` | `mod.rs`, `file_read.rs`, `file_edit.rs`, `file_write.rs`, `execute_command.rs`, `search.rs`, `list_files.rs`, `git_operations.rs`, `task_management.rs`, `note_edit.rs` | 本地工具执行层 | `agent_runtime` 直接依赖的核心工具注册表 | 是 |
| `utils` | `mod.rs`, `project.rs` | 通用工具函数 | 路径、目录、项目相关的辅助逻辑 | 间接相关 |
| `voice` | `mod.rs` | 语音输入外观层 | 目前还是占位实现，没有真实语音识别后端 | 否 |
| `wasm` | `mod.rs`, `client.rs`, `storage.rs`, `bridge.rs` | 浏览器/WebAssembly 能力 | feature-gated 的浏览器运行时与 JS bridge | 否 |
| `web` | `mod.rs`, `server.rs`, `routes.rs`, `handlers.rs`, `models.rs`, `templates.rs`, `main.rs` | 插件市场 Web 应用 | feature-gated 的 Axum Web 服务与模板层 | 否 |

## 7. 分模块说明

### 7.1 `advanced`

- 聚合了 SSH、远程执行、项目模板初始化等高级能力。
- 这部分实现更偏“通用 agent 产品能力”，与 Android 集成无直接关系。
- 当前 bridge 不会调用到这里。

### 7.2 `api`

- 提供 `ApiClient`、`ChatMessage`、`ToolDefinition` 以及响应模型。
- 请求形状兼容 OpenAI 风格的 `/v1/chat/completions`。
- 同时支持非流式和流式请求。
- 保留了 `reasoning_content`，这对当前 Android 的“深度思考”展示非常重要。

### 7.3 `cli`

- `args.rs` 定义了很大的命令树，包含 config、MCP、plugins、memory、
  services、agents、skills、stress-test 等命令。
- `mod.rs` 负责把这些命令接入 runtime。
- `repl.rs` 和 `ui.rs` 负责交互式命令行体验。
- Android App 虽然不使用这部分，但由于 `lib.rs` 无条件导出，它仍然属于 crate 表面的一部分。

### 7.4 `config`

- `Settings` 是 CLI 和 runtime 共用的配置对象。
- `api_config.rs` 和 `mcp_config.rs` 提供更细分的配置模型。
- 在 Android 路径上，`mobile_bridge` 会先根据请求载荷构造一个内存中的 `Settings`，再交给 `AgentRuntime`。

### 7.5 `gui`

- 基于 `egui` 和 `eframe` 的桌面 GUI。
- 包含完整的桌面聊天壳、设置页、侧边栏、主题、语法高亮、工具调用渲染等。
- 由于 Android native library 构建关闭了默认 feature，这部分当前不会参与 Android 嵌入构建。

### 7.6 `i18n`

- 使用 `rust-embed` 从 `locales/` 加载 Fluent 语言包。
- 提供翻译加载、fallback locale、翻译器切换能力。
- 当前 Android library 构建没有启用它，但如果后续仍需保留桌面/默认构建，这个资源目录必须在。

### 7.7 `mcp`

- 是一个体量较大的 MCP scaffold，覆盖 tools、resources、prompts、
  sampling、server、transport 等方向。
- `transport` 已经表达了 stdio 和 websocket 等传输抽象。
- 当前 Android App 不走 MCP，而是直接通过 `tools` 做本地工具执行。

### 7.8 `memory`

- 实现了 session memory、history、context、storage、consolidation 等能力。
- 整体比 Android 当前使用的状态管理要丰富得多。
- 当前 bridge endpoint 没有直接把这套 memory 暴露给 App。

### 7.9 `mobile_bridge`

- 当前 Android native library 最关键的入口模块。
- 定义了 `BridgeRunRequest`、`BridgeRunSnapshot`、`BridgeRunEvent`、
  `BridgeRunStatus` 等请求与快照 DTO。
- 会启动 Axum server、用 `DashMap` 管理活跃 run、支持取消，并把 runtime 事件转换成适合 UI 拉取的 snapshot。
- 这是 APK 集成链路中最重要的模块。

### 7.10 `plugins`

- 保留了插件注册、hook、loader、isolation、metadata 等能力。
- 整体更像“上游 agent 平台愿景”的一部分，而不是当前移动端运行时需求。
- Android 现在既不暴露插件管理，也不走插件加载链路。

### 7.11 `services`

- 提供 agents、auto-dream、voice、magic docs、team memory sync、
  plugin marketplace、stress tests 等更上层服务。
- 这些服务都不在当前 Android 嵌入路径上。
- 由于它们是无条件模块，所以仍然扩大了 crate 的编译面和暴露面。

### 7.12 `session`

- 是 `memory/` 之外的一层轻量 session 抽象。
- 目前不是 Android 集成中的关键部分。

### 7.13 `skills`

- 实现了 skill 抽象、registry、executor 以及内建 skill。
- 更偏向 CLI/agent 产品工作流，而不是当前 APK 的运行时主链路。
- `mobile_bridge` 和 `agent_runtime` 当前都不会直接调用它。

### 7.14 `state`

- 定义了 `AppState`、conversation state、tool registry state、memory bookkeeping 等。
- `src/main.rs` 的 CLI 入口会用到它。
- Android App 当前不会直接依赖这个 state 对象。

### 7.15 `terminal`

- 对 Ratatui 做了一个极简终端壳封装。
- 负责 alternate-screen、按键事件和一个占位 UI。
- 不在 Android 嵌入链路里。

### 7.16 `tools`

- 是当前 agent loop 直接依赖的本地工具层。
- `ToolRegistry` 注册了 file read/edit/write、shell command、search、
  list files、git operations、task management、note editing 等工具。
- `agent_runtime` 会把模型给出的 tool call 转成 registry 执行，再把结果回喂到对话循环。
- 在 Android runtime 中，它的重要性仅次于 `mobile_bridge` 和 `agent_runtime`。

### 7.17 `utils`

- 提供 home/config/data 目录、目录创建、格式化函数等小型工具。
- `project.rs` 则补充了项目级路径辅助逻辑。
- 主要是间接支撑代码。

### 7.18 `voice`

- 是一个很薄的语音输入外观层。
- 当前只输出模式信息，没有真实的语音识别能力。
- 对 Android 当前路径来说基本处于休眠状态。

### 7.19 `wasm`

- 面向浏览器环境的特性集合，提供 API 调用、浏览器存储、JS bridge。
- 当前 Android embedded library 不会构建这一层。

### 7.20 `web`

- 一个完整的 Axum Web 应用，面向插件市场等场景。
- 包含 routes、handlers、models，以及很大的模板文件。
- 当前与移动端运行时集成无关。

## 8. 当前实现观察

### 8.1 今天真正重要的模块

当前 Android 主路径主要依赖：

- `agent_runtime`
- `mobile_bridge`
- `api`
- `config`
- `tools`
- 少量 `utils` 中的共享辅助逻辑

### 8.2 仍然存在但不在 Android 热路径上的模块

下面这些模块现在更多是“继承自上游能力宽度”，而不是当前 App 的直接需求：

- `cli`
- `terminal`
- `gui`
- `web`
- `wasm`
- `plugins`
- `services`
- `memory`
- `skills`
- `advanced`
- `voice`
- `mcp` 的大部分能力

### 8.3 当前架构上的折中

这个 crate 现在更像是“一个大而全的产品型单 crate”，而不是“最小 Android runtime crate”。
它目前还能工作，但代价是 Android embedded build 仍会编译不少 APK 实际不会调用到的能力。

### 8.4 为什么这次只停在仓库资产清理

这次需求明确要求不改 `src/`。因此安全边界只能是：

- 删除不用的仓库外围/分发文件
- 保持 runtime 代码完整
- 用文档把真实模块设计讲清楚，为后续真正瘦身提供基线

## 9. `src` 之外保留的文件

目前仍然有意保留的非 `src` 内容包括：

- `Cargo.toml` 与 `Cargo.lock`：Rust 依赖和 feature 定义
- `locales/`：i18n 嵌入资源
- `tests/`：Rust 侧验证
- `LICENSE`：上游许可证信息
- `.env.example`：虽然 Android App 运行时注入 settings，但 vendored runtime 仍然保留 provider 配置形态
- `RUST_AGENT_INTEGRATION.md`：项目自己的 bridge 集成说明

## 10. 建议阅读顺序

如果只想理解 Android 相关主链路，建议按这个顺序读：

1. `src/mobile_bridge/mod.rs`
2. `src/agent_runtime.rs`
3. `src/api/mod.rs`
4. `src/tools/mod.rs`
5. `src/config/mod.rs`
6. `RUST_AGENT_INTEGRATION.md`

如果想理解整个 crate 的全貌，则从 `src/lib.rs` 开始，再结合本文里的模块表阅读即可。
