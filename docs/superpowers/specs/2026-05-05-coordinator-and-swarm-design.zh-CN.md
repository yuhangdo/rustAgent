# 协调者与蜂群模式设计

## 目标

实现项目 Agent 的多 Agent 高级编排能力，对齐 `coordinator-and-swarm` 文档里的两种模式：

- `Coordinator Mode`：中心化编排。Coordinator 只负责理解任务、分派 Worker、跟进 Worker、停止错误方向的 Worker、综合最终结果。
- `Agent Teams / Swarm`：蜂群式协作。Team Lead 创建团队，共享任务列表，Teammate 自主认领任务，并通过 mailbox 直接通信。

这次实现优先保证主链路能安全接入，并把最核心的编排原语做成可测试、可持久化、可继续扩展的 Rust 模块。

## 非目标

- 不实现嵌套团队。Worker 和 Teammate 不能再创建团队或派生新的团队层级。
- 不实现远程 Agent、CCR 分布式 Agent、后台 shell task、workflow task、MCP monitor task。
- 不把 Coordinator 做成能直接写代码的普通 agent。
- 不替换现有 `AgentRuntime`、`QueryEngine`、`AgentsService`，只在它们上面增加模式化编排能力。
- 不在这次实现 GUI 队伍管理页面；移动桥接和 UI 可在后续读取公开数据结构继续补。

## 总体架构

新增 `native/claude-code-rust/src/orchestration/mod.rs` 作为多 Agent 编排层。它承担三类职责：

- 模式策略：环境门控、Coordinator/Worker prompt、工具白名单、XML 通知渲染。
- Coordinator 工具：`agent`、`send_message`、`task_stop`、`subscribe_pr_activity`。
- Swarm 状态机：team config、task list、mailbox、hook event、任务认领和 teammate 生命周期。

现有运行链路保持不变：

- `AgentRuntime` 增加 `allowed_tool_names`，负责限制某个 agent mode 可见和可执行的工具。
- `QueryEngine` 在 `CLAUDE_CODE_COORDINATOR_MODE=1` 时切换到 Coordinator prompt 和 coordinator-only tool registry。
- `AgentsService` 新增内置 `Coordinator` 和 `Worker` agent，CLI 可以直接运行这两类 agent。

## Coordinator Mode

### 激活

Coordinator Mode 使用环境变量门控：

- `CLAUDE_CODE_COORDINATOR_MODE=1|true|yes|on`

当该变量开启时，`QueryEngine` 会：

- 将原始 system prompt 包装成 Coordinator system prompt。
- 只注册 coordinator 工具。
- 将 `allowed_tool_names` 设置为 coordinator 工具白名单。
- 如果 `CLAUDE_CODE_SCRATCHPAD` 开启，则在 workspace 下创建 `.claude-scratchpad` 并注入 prompt。

### Coordinator 工具权限

Coordinator 只能看到和调用这些工具：

- `agent`
- `send_message`
- `task_stop`
- `subscribe_pr_activity`

它不能直接读取文件、编辑文件、写文件、执行命令或搜索代码。这样可以把“理解、分配、综合”和“实际执行”分开，避免 Coordinator 跳过 Worker 直接动手。

### Worker 权限

Worker 的工具集由 `worker_allowed_tools(simple_mode)` 生成。

简化模式下只允许：

- `execute_command`
- `file_read`
- `file_edit`

普通模式下允许常规执行工具，但排除内部编排工具：

- 不允许 `agent`
- 不允许 `team_create`
- 不允许 `team_delete`
- 不允许 `send_message`
- 不允许 `task_stop`
- 不允许 `synthetic_output`

这保证 Worker 不能递归创建团队，也不能绕过 Coordinator 直接控制其他 Worker。

### Worker 通信

Coordinator 的 `agent` 工具会启动一个受限 Worker，并在 Worker 完成后返回 XML 格式的通知：

```xml
<task-notification>
  <task-id>agent-...</task-id>
  <status>completed|failed|killed</status>
  <summary>...</summary>
  <result>...</result>
  <usage>
    <total_tokens>...</total_tokens>
    <tool_uses>...</tool_uses>
    <duration_ms>...</duration_ms>
  </usage>
</task-notification>
```

Coordinator 可以使用 `send_message` 对指定 `<task-id>` 继续发送后续指令。`task_stop` 会把 Worker 标记为 `killed`，让 Coordinator 可以停止错误方向的工作。

## Swarm Mode

### 激活

Swarm Mode 使用环境变量门控：

- `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1|true|yes|on`

当前实现先提供底层状态机和持久化能力，供服务层、CLI、移动桥接或后续 UI 接入。

### 持久化布局

团队状态使用文件系统持久化。默认根目录是 `~/.claude`，测试和嵌入式场景可以通过 `AgentTeamStore::new(root)` 指定根目录。

目录结构：

```text
~/.claude/teams/{team-name}/config.json
~/.claude/tasks/{team-name}/tasks.json
~/.claude/tasks/{team-name}/mailbox.json
~/.claude/tasks/{team-name}/hooks.json
~/.claude/tasks/{team-name}/.lock
```

写入 task、mailbox、hook 时使用 `.lock` 文件做互斥，确保任务认领是原子的。

### 核心对象

- `AgentTeam`：团队配置，包含 `team_name`、`lead_session_id`、`task_list_id` 和 teammate 列表。
- `Teammate`：队友状态，包含角色、状态、创建时间和更新时间。
- `SwarmTask`：共享任务，包含状态、优先级、owner、依赖和结果。
- `MailboxMessage`：队友间消息，支持定向 `message` 和全员 `broadcast`。
- `SwarmHookEvent`：生命周期事件，覆盖任务创建、任务认领、任务完成、队友空闲和消息发送。

### 任务认领

`claim_task(team, task_id, teammate)` 是蜂群模式的核心并发原语。

流程：

1. 读取任务列表。
2. 根据已完成依赖解锁 blocked task。
3. 确认目标任务仍是 `pending` 且没有 owner。
4. 原子写入 `status=in_progress` 和 `owner=teammate`。
5. 竞争失败的一方收到 `already_claimed` 错误。

任务完成后，`complete_task` 会写入结果、标记完成，并自动解锁依赖它的任务。

### Mailbox

Mailbox 支持两种消息：

- `send_message`：发给指定 teammate。
- `broadcast`：发给所有 teammate，发送者本人不会在 inbox 中看到自己的广播。

`inbox(team, teammate)` 会返回该 teammate 的定向消息和可见广播。当前实现保留 `read_by` 字段，为后续已读状态提供兼容空间。

### Teammate 生命周期

当 teammate 异常退出或进入不可用状态时，`unassign_teammate_tasks(team, teammate)` 会：

- 找到该 teammate 拥有的未完成 `in_progress` 任务。
- 重置为 `pending`。
- 清空 owner。
- 记录 `TeammateIdle` hook。

这样 Team Lead 可以通过任务列表或 hook 事件感知到可重新分配的工作。

## Runtime 接线

`AgentExecutionRequest` 新增：

- `allowed_tool_names: Option<Vec<String>>`

`AgentRuntime` 在两个地方使用它：

- 构造 tool definitions 时只把允许的工具暴露给模型。
- 执行 tool call 前再次检查工具名，不允许的工具会作为 `tool_not_allowed` 失败结果写回对话。

这使工具权限成为运行时强约束，而不是只靠 prompt 约束。

## 服务层接线

`AgentsService` 新增两个内置 agent：

- `Coordinator`
- `Worker`

运行 Coordinator agent 时，服务层会创建一个空 `ToolRegistry`，只注册 coordinator 工具。运行 Worker 或其他内置 agent 时继续使用默认工具注册表，但通过 `allowed_tool_names` 做白名单限制。

CLI 的 agent 类型解析增加：

- `coordinator`
- `coord`
- `worker`

## 安全边界

当前实现的安全边界：

- Coordinator 不能直接调用文件、shell、搜索、编辑等动手工具。
- Worker 不能调用内部团队工具。
- `AgentRuntime` 对工具白名单做运行时检查。
- Team name 和 teammate name 禁止路径分隔符、空字符串、`.` 和 `..`，避免路径逃逸。
- Swarm task 写入用锁文件保护，避免并发认领同一任务。
- 不支持嵌套团队，降低递归编排风险。

## 测试覆盖

新增 `native/claude-code-rust/tests/orchestration_test.rs`，覆盖：

- Coordinator 工具白名单和 system prompt 渲染。
- Worker 工具权限过滤。
- `CLAUDE_CODE_COORDINATOR_MODE` 与 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` 环境门控。
- `<task-notification>` XML 结构。
- Swarm task 原子认领和 `already_claimed` 错误。
- Team config 和 task list 文件持久化。
- Mailbox 定向消息与广播。
- Teammate 异常释放任务。

同时回归运行：

- `--lib`
- `--test orchestration_test`
- `--test fast_path_test`
- `--test prompting_test`
- `--features mobile-bridge mobile_bridge::tests::trim_for_event_caps_large_payloads --lib`
- `rustfmt --check`

## 已知边界

- Coordinator 工具里的 Worker 当前是同进程执行，不是独立子进程或 git worktree 隔离。
- `subscribe_pr_activity` 当前记录订阅意图，尚未接真实 GitHub 事件流。
- Swarm 状态机已持久化，但还没有独立 CLI 子命令或 GUI 管理面。
- Mailbox 已保存消息和 `read_by` 字段，但未实现显式已读 API。
- `duration_ms` 和 `tool_uses` 在 coordinator Worker 通知中保留字段，后续可以从 runtime event handler 统计得更精确。

## 后续演进

后续可以自然扩展几块：

- 给 Swarm 增加 CLI 和 mobile bridge 操作入口。
- 给 Coordinator Worker 增加独立 worktree 或子进程隔离。
- 把 PR activity subscription 接到 GitHub app 或本地 `gh` 事件轮询。
- 增加 mailbox 已读、ack 和空闲通知的 UI 表达。
- 将 hook event 暴露给插件或用户自定义脚本。
