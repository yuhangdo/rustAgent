# Token Budget 与上下文压缩最终实现设计

## 目标

Rust 原生运行时已经实现 provider-aware 的 token budget 与上下文压缩管线。它替代了早期固定窗口的粗略裁剪方式，支持动态上下文窗口、粗略与精确 token 计数、阈值驱动的 warning / auto-compact / block、prompt-too-long 恢复、prompt cache metadata、输出 slot retry，以及 transcript 持久化的 compact 状态。

这份文档是最终实现设计，不再是实现前草稿。

## 已实现范围

- 动态解析 context window。
- 根据输出 token 预留计算 effective budget。
- 对所有 provider 使用 rough count。
- 对 Anthropic native provider 使用 `messages/count_tokens` 精确计数。
- 支持 warning、auto-compact、block 阈值和 circuit breaker。
- 支持 micro compact、session-memory compact、full compact、partial compact。
- 支持 prompt-too-long 后 full compact retry。
- 支持 Anthropic native prompt cache breakpoint。
- 支持输出 slot retry。
- 支持通过 transcript 持久化 token-budget 和 compact 状态。
- 支持 mobile bridge 暴露 context warning / compact / block 事件。

## 非目标

- 不实现 Bedrock / Vertex 的精确 token counting。
- 不为 OpenAI-compatible / Gemini-compatible provider 做精确计数。
- 不在这次实现 GUI 手动 partial compact 操作界面。
- 不把所有 provider 请求都改造成 Anthropic content-block 格式。

## 核心模块

### `token_budget`

路径：`native/claude-code-rust/src/token_budget/mod.rs`

职责：

- `ProviderKind`：区分 `AnthropicNative`、`OpenAICompatible`、`GeminiCompatible`。
- `ModelCapability`：描述模型上下文能力。
- `TokenBudgetState`：保存上下文窗口、effective budget、warning 状态、auto-compact 失败次数等。
- `resolve_context_window()`：解析当前模型可用上下文窗口。
- `effective_budget()`：扣除输出预留后的可用输入预算。
- `rough_count_messages()` / `rough_count_tools()`：粗略计数消息和工具定义。
- `evaluate_budget_decision()`：产出 warning、auto-compact、block 等决策。

### `compact`

路径：`native/claude-code-rust/src/compact/mod.rs`

职责：

- `micro_compact_history()`：压缩旧 tool result，把大输出替换为稳定 placeholder。
- `session_memory_compact()`：将较早历史总结为 session memory section。
- `full_compact()`：对指定范围做完整压缩，并注入 compact boundary 和 rehydration section。
- `full_compact_with_summary()`：允许 runtime 使用 LLM side-query 摘要覆盖本地启发式摘要。
- `CompactDirection::UpTo` / `CompactDirection::From`：支持 partial compact。

### `api`

路径：`native/claude-code-rust/src/api/mod.rs`

职责：

- `provider_kind()`：根据 base URL 判断 provider 类型。
- `count_tokens()` / `count_tokens_with_metadata()`：Anthropic native 精确计数。
- `chat_with_slot_strategy()` / `chat_with_slot_strategy_and_metadata()`：输出 slot retry。
- Anthropic native request builder：序列化 system、tools、messages、cache control。
- OpenAI-compatible fallback：保留原 chat-completions 路径。

### `agent_runtime`

路径：`native/claude-code-rust/src/agent_runtime.rs`

职责：

- 每轮执行前构建 prompt 并计算 token pressure。
- 根据 token-budget 决策发出 `TokenBudgetWarning`、`TokenBudgetBlocked`。
- 触发 micro / session-memory / full compact。
- 捕获 prompt-too-long 错误并执行 full compact retry。
- 通过 side-query 生成 full compact summary，失败时回退本地摘要。
- 非流式路径使用 slot retry。

### `query_engine`

路径：

- `native/claude-code-rust/src/query_engine/mod.rs`
- `native/claude-code-rust/src/query_engine/transcript.rs`

职责：

- 将 token-budget 状态从 transcript replay 回灌给 runtime。
- 持久化 token-budget warning、auto compact、compact failure、session compact、block 事件。
- 提供 `compact_session()`，允许外部触发 full / partial compact。
- 在 session resume 时恢复 compact 后的 history 和附加 prompt sections。

### `mobile_bridge`

路径：`native/claude-code-rust/src/mobile_bridge/mod.rs`

职责：

- 向移动端透出 token-budget warning。
- 向移动端透出 auto compact performed / failed。
- 向移动端透出 token-budget blocked。
- 复用已有 event payload trim 逻辑保护大输出。

## Provider 模型

运行时把 provider 分成三类：

- `AnthropicNative`
  - 使用 Anthropic Messages 风格请求。
  - 支持 `messages/count_tokens` 精确计数。
  - 支持 provider-level `cache_control`。
  - 支持 Anthropic native response 解析。

- `OpenAICompatible`
  - 使用原 chat-completions 路径。
  - 使用 rough count。
  - 保留内部 cache metadata，但不向 provider 发送 cache 字段。

- `GeminiCompatible`
  - 当前计数和执行行为与 OpenAI-compatible fallback 类似。
  - 单独保留类型，便于后续加入 Gemini-specific 逻辑。

## Context Window 解析

`resolve_context_window()` 的优先级：

1. `CLAUDE_CODE_MAX_CONTEXT_TOKENS`
2. 模型名后缀，例如 `[1m]`
3. 静态模型能力表
4. Anthropic 1M beta header
5. 默认 `200_000`

最终执行预算为：

```text
effective_budget = context_window - min(max_output_tokens, 20_000)
```

warning、auto-compact、block 都基于 `effective_budget` 判断，而不是直接使用完整 context window。

## Token 计数

### Rough Count

rough count 适用于所有 provider：

- 普通文本和 reasoning 按字符估算。
- JSON、tool call、tool result 使用更重权重。
- 图片、文档类内容使用 sentinel 成本。
- tool definitions 会序列化后加入预算。

### Exact Count

exact count 当前只对 Anthropic native provider 生效：

- 调用 `POST /v1/messages/count_tokens`。
- 请求体包含实际会发送的 tools、system、messages。
- cache metadata 与实际请求保持一致。
- 如果 provider 不支持精确计数，则返回 fallback 并继续使用 rough count。

## 阈值与 Circuit Breaker

`TokenBudgetState` 保存：

- context window
- effective budget
- warning 是否已发出
- 连续 auto-compact 失败次数
- compact baseline
- 输出 token 预留

主要常量：

- `WARNING_THRESHOLD_BUFFER_TOKENS`
- `AUTOCOMPACT_BUFFER_TOKENS`
- `ERROR_THRESHOLD_BUFFER_TOKENS`
- `MANUAL_COMPACT_BUFFER_TOKENS`
- `MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES`

运行时行为：

- 低于 warning threshold：正常执行。
- 达到 warning threshold：发出一次 `TokenBudgetWarning`。
- 达到 auto-compact threshold：尝试自动压缩。
- 达到 block threshold：发出 `TokenBudgetBlocked` 并阻止继续发送超长请求。
- auto-compact 连续失败过多：打开 circuit breaker，停止继续自动压缩。

## 压缩管线

### Micro Compact

`micro_compact_history()` 会压缩较早的 tool output：

- 保留 tool call / tool result 结构。
- 替换大输出为稳定 placeholder。
- 保护最近若干消息，避免破坏正在进行的工具上下文。

它是最便宜、最小损耗的一层压缩。

### Session-Memory Compact

`session_memory_compact()` 会保留最近消息，把更早的 user / assistant / tool 内容提炼为 session memory user-context section。

它比 full compact 更轻，适合先尝试降低上下文压力。

### Full Compact

`full_compact()` 会生成：

- compact 后的 history
- compact boundary system section
- compact summary user-context section
- rehydration / reinjection sections

`AgentRuntime` 在 full compact 时优先走 LLM side-query 生成 summary；如果 side-query 失败，回退到本地启发式摘要。

### Partial Compact

partial compact 支持：

- `UpTo`：压缩 anchor 之前的历史，保留近期对话。
- `From`：保留前缀，压缩 anchor 之后的尾部。

`From` 更利于保持 prompt cache 前缀稳定。

## Prompt Rebuild 与 Prompt Cache

`PromptBuilder` 支持额外 compact sections：

- additional system sections
- additional user-context sections

compact 后的 prompt 会携带：

- compact boundary marker
- compact summary
- post-compact reinjection context
- preserved recent turns

Anthropic native 请求会把 cache breakpoint 序列化到：

- tool definitions
- system blocks
- message content blocks

OpenAI-compatible / Gemini-compatible provider 保留内部 metadata，但不发送 provider cache 字段。

## Output Slot Retry

正常请求先使用默认或配置的输出 slot。若 provider 返回 length finish reason，则同一请求会 retry 一次，并使用 `SLOT_RETRY_MAX_TOKENS`。

限制：

- 只 retry 一次。
- 只有 length 类 finish reason 触发。
- 非 length 错误不会触发 slot retry。
- retry 复用相同 prompt 和 compact 状态。

## Prompt-Too-Long Recovery

如果 provider 返回 prompt-too-long 类错误，`AgentRuntime` 会：

1. 识别错误文本。
2. 执行 full compact。
3. 发出 `AutoCompactPerformed`，策略为 `prompt_too_long_retry`。
4. 发出 `SessionCompacted`。
5. 用 compact 后的历史重试当前轮。

如果 compact 仍无法恢复预算，则按正常错误路径返回。

## Transcript 持久化

transcript 记录以下事件：

- `TokenBudgetWarning`
- `AutoCompactPerformed`
- `AutoCompactFailed`
- `SessionCompacted`
- `TokenBudgetBlocked`

replay 时恢复：

- compact 后的 history
- additional system sections
- additional user-context sections
- warning 状态
- auto-compact failure count
- block 状态

这保证 session resume 后不会丢失 compact baseline。

## 测试覆盖

已覆盖的测试类型：

- provider classification
- Anthropic count-tokens request serialization
- Anthropic prompt-cache metadata serialization
- output slot retry 条件
- context-window 优先级
- effective budget
- rough count 权重
- warning / auto-compact / block 阈值
- auto-compact circuit breaker
- micro compact
- session-memory compact
- full compact
- partial compact
- prompt builder compact section 渲染
- transcript replay 的 token-budget 和 compact 状态
- mobile bridge event payload trim

常用验证命令：

```powershell
cargo test --manifest-path D:\work\rustAgent\native\claude-code-rust\Cargo.toml --lib
cargo test --manifest-path D:\work\rustAgent\native\claude-code-rust\Cargo.toml --test prompting_test
cargo test --manifest-path D:\work\rustAgent\native\claude-code-rust\Cargo.toml --features mobile-bridge mobile_bridge::tests::trim_for_event_caps_large_payloads --lib
rustfmt --edition 2021 --check --config skip_children=true ...
```

## 已知边界

- Bedrock 和 Vertex exact count 尚未实现。
- OpenAI-compatible / Gemini-compatible provider 仍使用 rough count。
- GUI 手动 partial compact 入口尚未实现。
- 部分 upstream hook 概念在当前实现中以 runtime event 和 transcript event 表达，还没有完整外部 hook runner。

这些边界是当前实现范围内的有意取舍；核心 runtime、provider、compaction、transcript 和 bridge 接线已经完成。
