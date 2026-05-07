# Auto Mode 最终实现设计文档

## 背景

Auto Mode 的目标是让 Agent 在用户明确开启后进入连续自主执行状态：安全操作自动执行，危险操作直接拒绝，不确定操作降级为需要人工确认。它不同于 `bypassPermissions`，不会无条件放行；也不同于默认模式，不会对每个敏感操作都打断用户，而是在工具执行前运行分类器。

本实现面向当前 Rust Agent 架构落地，核心位于 `native/claude-code-rust/src/auto_mode/mod.rs`，并接入 `AgentRuntime`、Transcript、Mobile Bridge、配置和公开导出。

## 状态模型

Auto Mode 使用 `AutoModeSession` 保存运行期状态：

- `active`：是否正在使用 Auto Mode。
- `previous_mode`：进入 Auto Mode 前的权限模式，用于退出时恢复语义。
- `model` / `model_supported`：记录分类器模型和支持状态。
- `circuit_broken`：紧急熔断开关，开启后禁止 Auto Mode 激活。
- `stripped_dangerous_rules`：进入 Auto Mode 时被剥离的危险 allow 规则。

激活来源包括配置 `settings.safety.auto_mode` 和环境变量 `CLAUDE_CODE_ENABLE_AUTO_MODE=true`。`CLAUDE_CODE_DISABLE_AUTO_MODE=true` 会显式关闭。配置里的 `safety.auto_mode_circuit_breaker` 是本地熔断开关。

## 分类器设计

`AutoModeSession::classify_tool_call()` 是工具调用前的唯一安全入口。分类结果为：

- `Allow`：直接执行工具。
- `Deny`：拒绝执行，并向模型返回 `auto_mode_tool_denied`。
- `Ask`：不执行工具，向模型返回 `auto_mode_permission_required`，表示应降级到人工确认。

分类器按两阶段语义建模：

- `Fast`：确定性安全或危险的快速判断，如只读工具、危险命令、路径逃逸。
- `Thinking`：需要结合上下文的判断，如工作区内编辑、测试/构建命令、远端 git 操作是否有明确用户意图。

当前 Rust 实现采用确定性规则分类器作为稳定 fallback：它不依赖在线模型，因此测试和本地运行稳定。接口和事件字段保留 `stage`、`unavailable`、`transcript_too_long`、`model`，后续可以在相同边界内替换为真正的 LLM transcript classifier。

## 安全规则

Auto Mode 激活后不会隐藏普通工具，而是在执行前分类。Runtime 在 Auto Mode 激活时禁用 quick path，避免 fast path 并发执行绕过分类器。

默认规则：

- 只读工具 `file_read`、`search`、`list_files` 自动 allow。
- `file_edit` / `file_write` 仅允许写入 workspace 内路径，路径逃逸直接 deny。
- 测试、lint、构建命令如 `cargo test`、`cargo check`、`npm test`、`pytest` 自动 allow。
- 只读 git 命令如 `git status`、`git diff`、`git log` 自动 allow。
- 远端 git 操作如 `push` / `pull` 只有在最近用户消息明确提到对应意图时 allow，否则 ask。
- 高危命令如 `rm -rf`、`sudo`、shell wrapper、inline interpreter、网络传输工具、权限削弱命令直接 deny。
- 未知写操作默认 ask，不会自动执行。

危险 allow 规则剥离由 `strip_dangerous_permissions_for_auto_mode()` 完成。它会移除可能绕过分类器的规则，例如 `Bash(python:*)`、`Bash(node:*)`、`Bash(sh:*)`、`Agent(*)`、`sudo`、`eval` 等。退出时可用 `restore_dangerous_permissions()` 恢复。

## Runtime 接入

`AgentRuntime` 新增 `auto_mode_session`：

1. 构造 Runtime 时从 `Settings` 和环境变量构建 `AutoModeConfig`。
2. 每次执行开始同步 workspace root，并在 active 时发出 `AgentEvent::AutoModeEntered`。
3. 构造系统提示词时先注入 Auto Mode 指令，再叠加 Plan Mode 指令。
4. 工具执行前，先走 Plan Mode 可见性检查，再走 Auto Mode 分类。
5. `Allow` 后才触发 tool hook、文件快照和真实工具执行。
6. `Deny` / `Ask` 不执行工具，只向 transcript 追加工具失败消息。

这种顺序保证了 Plan Mode 的只读约束优先，Auto Mode 负责默认/自主执行阶段的细粒度安全分类。

## Transcript 与移动端事件

Transcript 新增：

- `AutoModeEntered`
- `AutoModeExited`
- `AutoModeDecisionRecorded`

`TranscriptReplay` 会恢复 `auto_mode_status` 和 `auto_mode_decisions`，便于后续恢复会话、调试分类器决策和审计安全行为。

Mobile Bridge 新增事件：

- `AUTO_MODE_ENTERED`
- `AUTO_MODE_EXITED`
- `AUTO_MODE_DECISION`

移动端可以展示进入 Auto Mode、危险规则剥离数量、每次工具调用的分类结果、降级原因和不可用状态。

## 配置

`Settings` 新增 `SafetySettings`：

- `safety.auto_mode`
- `safety.auto_mode_circuit_breaker`
- `safety.auto_mode_stage`
- `safety.auto_mode_allow_rules`
- `safety.auto_mode_deny_rules`
- `safety.auto_mode_environment`

Mobile Bridge 请求也支持 `settings.autoMode`，用于移动端运行时开启 Auto Mode。

## 测试覆盖

新增 `native/claude-code-rust/tests/auto_mode_test.rs` 覆盖：

- 危险 allow 规则剥离与恢复。
- 只读工具、测试命令、危险命令、未知命令的分类行为。
- workspace 内编辑 allow 与路径逃逸 deny。
- Auto Mode full / sparse / exit 系统提示词。
- 不支持模型和 circuit breaker 阻止激活。
- Transcript replay 恢复 Auto Mode 状态和分类器决策。

这些测试先以缺失模块/事件的方式红测失败，再实现到绿测，确保核心行为不是事后补测。
