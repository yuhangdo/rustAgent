# 子 Agent 机制最终实现设计文档

日期：2026-05-08

## 背景与目标

本次实现对齐 `https://ccb.agent-aura.top/docs/agent/sub-agents` 中的核心机制，把此前 coordinator 内部的简化 worker 路径升级为统一的通用子 Agent 运行时。目标不是只保留一个 coordinator 专用工具，而是让普通 Agent、Coordinator、SDK/headless 场景都复用同一套任务状态、后台队列、sidechain transcript、TaskOutput、SendMessage 和 TaskStop 语义。

最终实现集中在 `native/claude-code-rust/src/sub_agents/mod.rs`，并由 `AgentRuntime` 默认注册 `agent`、`send_message`、`task_output`、`task_stop` 四个工具。`orchestration` 中旧的 coordinator worker 工具已不再作为独立任务系统存在，coordinator 只额外保留 `subscribe_pr_activity`。

## 已实现范围

已实现的核心能力：

- 命名子 Agent：内置 `general-purpose`、`worker`、`explore`、`plan`、`verification`、`claude-code-guide`，并支持从工作区 `.claude/agents/*.md` 加载自定义 Agent。
- Agent definition frontmatter：支持 `name`、`description`、`when_to_use`、`tools`、`model`、`permissionMode`、`background`、`isolation`、`maxTurns`、`memory`、`requiredMcpServers`、`hooks`、`skills`、`initialPrompt`。
- AgentTool：支持同步执行和 `run_in_background` 后台执行，返回 `task_id`、状态、输出文件和 sidechain transcript 路径。
- 后台生命周期：后台任务通过 Tokio task 运行，终态写入输出文件和 transcript，并进入通知队列。
- 通知队列：`AgentRuntime` 每轮开始会 drain 后台子 Agent 通知，并以 `<sub-agent-notifications>` 注入下一轮上下文。
- SendMessage：支持通过 `task_id`、唯一 Agent 名称或唯一任务描述寻址；running 状态消息排队，terminal 状态可恢复执行。
- TaskOutput：支持非阻塞读取状态，也支持 `block=true` 等待后台任务进入终态。
- TaskStop：停止后台任务、标记为 `killed`、阻止后续消息，并写入通知。
- Sidechain transcript：每个子 Agent 任务写入 `.claude/agents-runtime/transcripts/<task_id>.jsonl`，记录任务启动、排队消息、完成或失败。
- Output file：默认写入 `.claude/agents-runtime/outputs/<task_id>.md`，也允许 AgentTool 指定 `output_file`。
- Worktree isolation：`isolation: worktree` 会在 `.claude/agents-runtime/worktrees/<task_id>` 下准备隔离工作区；如果工作区是 git 仓库，会通过 `git worktree add --detach` 创建真正 worktree。
- 安全互斥：`cwd` 与 `isolation: worktree` 明确互斥，避免写入根目录不确定。
- 权限策略：子 Agent 工具集会剔除内部编排工具；`permissionMode: plan` 会降级为只读工具集合；`permissionMode: auto` 会启用 Auto Mode 判断。
- Fork gate：`fork` 请求受 `FORK_SUBAGENT` / `CLAUDE_CODE_FORK_SUBAGENT` 开关和上下文限制保护，后台任务和 coordinator 场景不会启用 fork。

## 运行架构

核心类型：

- `SubAgentDefinition`：描述命名 Agent 的角色、工具、模型、权限、隔离和 prompt。
- `SubAgentRegistry`：加载内置 Agent 与 `.claude/agents/*.md` 自定义 Agent。
- `SubAgentManager`：管理任务状态、后台 JoinHandle、通知队列、transcript 和输出文件。
- `SubAgentRunner`：抽象真正的执行器，生产环境由 `RuntimeSubAgentRunner` 调用 `AgentRuntime`，测试中可注入 fake runner。
- `SubAgentTaskSnapshot`：所有工具返回的统一任务快照。

主流程：

1. 模型调用 `agent`，指定 `subagent_type`、`description`、`prompt`，可选 `run_in_background`、`cwd`、`isolation`、`output_file`。
2. `SubAgentManager` 从 registry 解析 Agent 定义，准备 workspace、output file 和 sidechain transcript。
3. 同步任务直接等待 `SubAgentRunner::run` 完成；后台任务返回 running 快照，并在 Tokio task 中继续执行。
4. 子 Agent 完成后写输出文件、追加 transcript，并将 `<task-notification>` 推入通知队列。
5. `AgentRuntime` 下一轮开始 drain 通知队列，把通知注入主模型上下文。
6. 主模型可用 `task_output` 查看或等待结果，用 `send_message` 追加指令，用 `task_stop` 停止任务。

## 安全与降级

工具权限按 Agent 定义和运行模式共同收敛：

- 自定义 Agent 的 `tools` 是允许工具上限，运行时不会自动扩大。
- 内部工具 `agent`、`send_message`、`task_output`、`task_stop` 默认不传给子 Agent，避免无限嵌套和任务系统互相操作。
- `permissionMode: plan` 强制只保留 `file_read`、`search`、`list_files`、`task_output`。
- `permissionMode: auto` 在子 Agent 内启用 Auto Mode 工具分类器。
- `isolation: remote` 当前没有远端运行器配置时直接失败，而不是静默降级到本地执行。
- `isolation: worktree` 与 `cwd` 冲突时直接拒绝，避免调用方以为在 worktree 中执行但实际落到另一个目录。
- fork 默认关闭，只在显式 feature gate 打开且请求不是后台/coordinator 场景时允许。

## 与 Coordinator / Swarm 的关系

Coordinator 现在使用同一套通用子 Agent 工具：

- `coordinator_allowed_tools()` 包含 `agent`、`send_message`、`task_output`、`task_stop`、`subscribe_pr_activity`。
- coordinator 的旧 `WorkerRecord`、旧 `SendMessage` 和旧 `TaskStop` 任务语义已从实际工具注册路径中移除。
- `worker_allowed_tools()` 仍作为 worker 策略辅助函数存在，用于内置 worker 定义和测试。
- Swarm 的 team/task/mailbox 状态机保持独立，它解决的是多人/多 teammate 的任务分配，不替代单个子 Agent 生命周期。

## 验证覆盖

新增 `native/claude-code-rust/tests/sub_agents_test.rs` 覆盖：

- Markdown frontmatter 自定义 Agent 定义解析。
- 后台 Agent 输出文件、sidechain transcript 和通知队列。
- running Agent 的 SendMessage 排队和 terminal Agent 恢复。
- TaskStop kill 语义和后续消息拒绝。
- worktree isolation 与 cwd 的安全互斥。

更新 `native/claude-code-rust/tests/orchestration_test.rs` 覆盖 coordinator 新增 `task_output` 工具和提示词变化。

## 已知边界

本实现已经补齐核心机制，但仍保留几个明确边界：

- `requiredMcpServers`、`hooks`、`skills` 已解析并注入 Agent prompt，但尚未实现 agent-scoped MCP/hook/skill 独立装载器。
- `remote` isolation 需要远端执行配置，本地运行时会显式失败。
- fork 当前实现了 feature gate 和请求路由保护；完整“继承父上下文精确 prompt cache”的 UI/交互层还需要上层调用方提供父上下文快照。
- worktree 清理策略当前保守保留输出，避免后台任务完成后丢失可审计文件。
