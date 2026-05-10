# Ultraplan 最终实现设计文档

## 目标

Ultraplan 是增强规划入口。启用 `FEATURE_ULTRAPLAN=1` 后，普通用户输入中的有效 `ultraplan` 关键字会被重定向到增强 Plan Mode；显式 `/ultraplan` 命令可直接进入该模式。相比普通 Plan Mode，Ultraplan 会给模型注入更强的规划提示，要求先只读探索，再输出包含文件级步骤、测试策略、风险、回滚和审批边界的深度实现计划。

## 输入路由

新增 `native/claude-code-rust/src/ultraplan/mod.rs` 作为统一入口，职责包括：

- `find_ultraplan_trigger_positions(text)`：定位真实触发关键字。
- `replace_ultraplan_keyword(text)`：从普通输入中移除触发词，保留用户原始意图。
- `process_ultraplan_input(text)`：统一处理 feature flag、关键字触发和 `/ultraplan` 命令。
- `highlight_ultraplan_keyword(text)`：返回 UI 可用的彩虹高亮 span。
- `render_rainbow_ultraplan_highlight(text)`：为终端输出生成 ANSI 彩虹高亮文本。

关键字检测会过滤以下误触发：

- 引号、单引号和反引号中的 `ultraplan`。
- 路径中的 `ultraplan`，例如 `/tmp/ultraplan/spec.md`。
- 其它斜杠命令中的 `ultraplan`，例如 `/help ultraplan`。
- 与字母、数字、下划线或连字符连在一起的非独立词。

## 命令模式

新增 `UltraplanCommandHandler`，它接受 `UltraplanRoute` 并进入增强 Plan Mode：

- `UltraplanLaunchMode::Local`：本地进入 Plan Mode。
- `UltraplanLaunchMode::Remote`：生成 CCR remote session id，并进入远程等待状态。

CLI 新增 `claude-code ultraplan --prompt <text> [--remote]`，REPL 和 `query` 路径也通过同一个 `process_ultraplan_input` 路由。普通关键字触发需要 `FEATURE_ULTRAPLAN=1`，显式 `/ultraplan` 命令用于直接执行。

## Plan Mode 集成

`PlanModeStatus` 新增 `ultraplan: Option<UltraplanPlanStatus>` 字段，记录：

- 是否处于 Ultraplan active 状态。
- 本地或远程启动模式。
- 原始输入和清理后的 prompt。
- 可选 CCR remote session id。

`PlanModeSession::enter_ultraplan` 会将会话切入 `PlanMode::Plan`，并保留 Ultraplan 上下文。`plan_mode_system_prompt` 检测到 active Ultraplan 后，会调用 `ultraplan_system_prompt` 注入增强规划约束。

增强提示要求：

- 保持只读直到计划被批准。
- 先深入探索，再输出计划。
- 计划必须包含具体步骤、测试策略、风险、回滚和审批边界。
- 远程 CCR 模式必须等待 `exit_plan_mode` 审批结果返回。

## CCR 远程会话

新增轻量 CCR 状态机：

- `CcrSession`：记录 session id、状态、teleport target、轮询次数和时间戳。
- `CcrSessionState`：`Created`、`Teleported`、`Polling`、`Approved`、`TimedOut`。
- `ExitPlanModeScanner`：默认每 3 秒轮询一次 `PlanModeSession`，直到进入 `AwaitingApproval` 或超时。

该实现不依赖真实远程网络，先把状态机、轮询、超时和审批结果统一抽象出来。未来可把 `CcrSession::teleport_to_remote` 接到真实 teleport/CCR transport。

## UI 信号

Rust CLI 侧提供两个 UI 友好的接口：

- `highlight_ultraplan_keyword` 返回结构化 spans，适合图形输入框渲染彩虹动画。
- `render_rainbow_ultraplan_highlight` 返回 ANSI 彩色字符串，适合终端渲染。

这样既满足当前 CLI，又给后续 GUI/PromptInput 组件留出稳定接口。

## 测试覆盖

新增 `native/claude-code-rust/tests/ultraplan_test.rs`，覆盖：

- 关键字触发与引号、路径、斜杠命令误触发过滤。
- `replaceUltraplanKeyword` 等价清理行为。
- feature flag 与 `/ultraplan` 命令路由。
- 本地命令进入增强 Plan Mode 并生成 Ultraplan system prompt。
- CCR scanner 轮询到 `exit_plan_mode` 审批结果。
- 彩虹高亮只标记真实触发关键字。

这些测试先 RED，确认旧实现没有 Ultraplan 模块，再通过新增实现转绿。
