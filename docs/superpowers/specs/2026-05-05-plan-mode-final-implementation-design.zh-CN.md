# Plan Mode 最终实现设计文档

## 背景与目标

本实现对齐 `https://ccb.agent-aura.top/docs/safety/plan-mode.md` 描述的 Claude Code Plan Mode：在高风险或不明确任务开始前，Agent 先进入只读探索阶段，形成可审阅、可编辑的实施计划，用户批准后再继续执行。目标不是只追加提示词，而是在 Rust Agent 中落成可执行的安全状态机。

本次实现覆盖四个核心目标：

- 进入计划模式后，工具列表和实际工具执行双重收窄，只允许只读工具与 `exit_plan_mode`。
- 退出计划模式时，将计划持久化为可编辑 Markdown 文件，并记录 prompt-based 权限请求。
- Runtime、QueryEngine transcript、Mobile Bridge 都能观测并持久化计划模式生命周期事件。
- 单测覆盖工具权限、状态迁移、计划文件持久化、transcript 回放恢复。

## 状态机

计划模式状态由 `PlanModeSession` 独立维护，避免把安全状态散落在 runtime 分支里。

状态包括：

- `default`：正常执行状态。模型可以看到 `enter_plan_mode`，用于主动进入计划模式。
- `plan`：只读探索状态。runtime 每轮重新计算工具定义，只暴露 `ReadOnly` 工具和 `exit_plan_mode`。
- `awaiting_approval`：计划已提交并持久化，当前 run 立即停止，等待用户批准后再继续。

状态迁移：

```mermaid
stateDiagram-v2
    [*] --> default
    default --> plan: enter_plan_mode(previous_mode)
    plan --> awaiting_approval: exit_plan_mode(plan, allowed_prompts)
    awaiting_approval --> default: 后续用户批准后开启新执行轮
```

## 工具权限模型

新增 `ToolAccess`：

- `ReadOnly`：不会修改项目状态，例如 `file_read`、`search`、`list_files`。
- `Write`：默认级别，包含文件写入、编辑、命令执行、任务管理等可能改变状态的工具。
- `Internal`：运行时内部安全工具，例如 `enter_plan_mode` 和 `exit_plan_mode`。

计划模式启用时使用同一策略做两层保护：

- 工具定义过滤：模型在请求中只看到只读工具和 `exit_plan_mode`。
- 执行前拦截：即使模型手写或复用历史中的写工具调用，runtime 也会返回 `plan_mode_tool_not_allowed`，不会执行写工具。

`execute_command` 在计划模式下按 `Write` 处理。虽然快路径已有只读命令白名单，但计划模式的第一版安全边界更保守：shell 执行不直接暴露，避免平台差异和命令副作用绕过只读语义。

## Runtime 集成

`AgentRuntime` 在初始化时为注册表绑定一个共享 `PlanModeSession` 并注册两个内部工具：

- `enter_plan_mode`：记录进入前权限模式并切换为 `plan`。
- `exit_plan_mode`：接收计划文本和 `allowed_prompts`，写入计划文件，进入 `awaiting_approval`。

每个 LLM turn 前，runtime 都会读取当前状态并重建工具定义。这样 `enter_plan_mode` 执行成功后的下一轮会立即只展示只读工具，`exit_plan_mode` 执行成功后会立即停止本次执行，防止计划提交后同一轮继续改代码。

系统提示也随状态动态增强：

- 默认状态提示模型在高影响、多文件或模糊任务中先调用 `enter_plan_mode`。
- 计划状态提示模型只能只读探索，形成计划后调用 `exit_plan_mode` 并等待审批。

## 计划持久化与 Prompt-based 权限

`exit_plan_mode` 将计划写入：

`docs/superpowers/plans/<timestamp>-plan-mode-<short-id>.md`

计划文件包含：

- 可编辑说明。
- `## Plan` 下的实施计划正文。
- `## Allowed prompts` 下的 prompt-based 权限请求。

`allowed_prompts` 当前支持：

```json
[{ "tool": "Bash", "prompt": "run tests" }]
```

Rust Agent 当前没有独立的人机权限确认 UI，因此本实现先完整记录并透传 prompt-based 权限请求：tool output metadata、`AgentEvent::PlanModeExited`、transcript replay 都保留该信息。后续接入真正的审批 UI 或 shell 权限分类器时，可以直接消费这份结构化数据。

## Transcript 与移动端观测

新增 transcript 事件：

- `plan_mode_entered`
- `plan_mode_exited`

`TranscriptReplay` 会恢复 `PlanModeStatus`，包括计划文件路径、等待审批状态、是否编辑、allowed prompts。这样 session 恢复、预算回放、移动端状态展示都能拿到一致的计划模式状态。

Mobile Bridge 新增事件类型：

- `PLAN_MODE_ENTERED`
- `PLAN_MODE_AWAITING_APPROVAL`

移动端可以在事件流中展示“已进入计划模式”和“计划等待审批”的关键节点。

## 安全边界

本实现采用保守失败策略：

- 未显式标记 `ReadOnly` 的工具默认是 `Write`。
- 计划模式下不暴露 `execute_command`，即使某些命令理论上只读。
- `exit_plan_mode` 只能在 `plan` 状态调用，否则返回错误。
- 提交计划后 runtime 立即结束当前执行，不允许同一 run 继续执行写工具。
- 计划文件写入固定在 workspace 下的 `docs/superpowers/plans`，不会使用模型传入路径。

## 测试覆盖

新增 `tests/plan_mode_test.rs` 覆盖：

- `ToolAccess` 标记和计划模式工具过滤。
- 计划模式下写工具请求被策略拒绝。
- `PlanModeSession` 进入和退出状态迁移。
- `exit_plan_mode` 持久化可编辑计划文件和 allowed prompts。
- `ToolRegistry::register_plan_mode_tools` 驱动状态。
- `TranscriptStore::replay` 恢复计划模式状态。

后续如果接入审批 UI，需要补充端到端测试：用户批准计划后恢复原权限模式，并将 allowed prompts 注入命令权限判断。
