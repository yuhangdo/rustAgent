# 项目 Agent 快慢路径设计

## 目标

给项目级 Agent 增加两条执行路径：

- 一条严格只读的快路径，用来处理简单检索、读取、汇总类任务
- 一条现有的多轮 Agent 慢路径，用来处理其余复杂任务

快路径要更快、更安全、也更容易在不确定时回退；慢路径继续作为默认兜底，只要置信度或安全性下降，就自动切回慢路径。

## 为什么要做

当前 runtime 不区分任务复杂度，所有请求都会直接进入标准 Agent loop。这样一来，即使用户只是想做少量只读检索，也会付出完整多轮 Agent 的延迟和成本。

新的设计不会替换现有 runtime，而是在最前面加一层快路径。它负责：

1. 识别是否是安全的只读任务
2. 规划一个很小的工具执行计划
3. 以“批内并发、批间串行”的方式执行最多 3 个只读工具
4. 用一次 LLM 收尾输出最终答案
5. 只要任何环节不稳妥，就自动降级到标准慢路径

## 非目标

- 不做可写的快路径
- 不提供 `force_fast`
- 不让快路径演化成开放式多轮循环
- 不做通用 DAG 工具执行器
- 不替换现有 `AgentRuntime`

## 架构

### 执行层

现在 runtime 内部有两层执行能力：

- `QuickPathExecutor`
- `AgentRuntime` 慢路径循环

快路径挂在 `AgentRuntime` 的最前面，先尝试一次保守的只读快速执行；如果不合适，再进入现有慢路径。这种接法能保持对外接口稳定，同时把新逻辑控制在一个清晰边界里。

### 核心模块

- `native/claude-code-rust/src/fast_path/mod.rs`
  - 路由 hint
  - 硬规则路由
  - 可选 LLM 分类
  - 只读工具规划
  - 只读命令校验
  - 批次规划与并发执行
  - 最终收尾与自动降级
- `native/claude-code-rust/src/agent_runtime.rs`
  - 在现有多轮 loop 前接入快路径
  - 快路径跳过时复用已准备好的 prompt
  - 快路径执行过工具后若降级，会把只读证据回灌给慢路径
- `native/claude-code-rust/src/query_engine/*`
  - 把执行 hint 贯穿到 session submit 链路
  - 持久化快路径相关事件
- `native/claude-code-rust/src/mobile_bridge/mod.rs`
  - 接收上层传入的 hint
  - 向移动桥接层暴露快路径选择与降级事件

## 请求面

以下请求结构现在都带有执行模式 hint：

- `AgentExecutionRequest.execution_mode_hint`
- `QuerySubmitRequest.execution_mode_hint`
- `BridgeRunRequest.execution_mode_hint`

当前支持的 hint 为：

- `auto`
- `prefer_fast`
- `prefer_slow`
- `force_slow`

刻意不提供 `force_fast`，因为安全性必须高于提速。

## 路由策略

### 先走硬规则

只要满足任一条件，直接拒绝快路径，进入慢路径：

- 调用方显式传了 `prefer_slow` 或 `force_slow`
- 当前会话里已经存在工具回合或 reasoning 回合
- 当前 prompt 中带有额外 compact / replay section
- 可见历史已经太长
- 最新用户请求明显带有写操作意图
- 最新用户请求明显是深度、多步、开放式任务

典型例子：

- “编辑这个文件”
- “实现这个功能”
- “规划一次重构”
- “做一次根因分析”

### 直接命中快路径候选

当请求明显是下面这些类型时，可直接视为快路径候选：

- find / search
- list / show
- read / inspect
- summarize / explain
- git status / diff / log 这类只读检查

### LLM 作为补充分类器

如果硬规则无法明确判断，就调用一次轻量分类器。分类器输出严格 JSON，并且必须在不确定时选择 `slow`。

分类器输出包括：

- `route`
- `confidence`
- `reason`
- `candidate_tools`
- `has_dependencies`

runtime 只有在下面条件同时满足时，才会真正放行到快路径：

- `route == quick`
- `confidence` 高于阈值
- 推荐工具全部在白名单内
- 依赖关系仍落在快路径允许的复杂度内

## 快路径执行模型

### 第一步：规划

规划器返回严格 JSON：

- `goal`
- `steps`

每个 step 必须包含：

- `id`
- `tool`
- `input`
- `depends_on`
- `read_only`
- `reason`

如果这个请求不适合快路径，规划器必须直接返回慢路径交接结果，而不是勉强给出计划。

### 第二步：校验

runtime 会拒绝任何违反下列规则的计划：

- step 数为 0
- step 数超过 3
- step id 不唯一
- step 不是只读
- 使用了不支持的工具
- 工具输入结构不合法
- 依赖关系引用错误
- 需要的执行批次过多

### 第三步：执行

执行策略是“批内并发，批间串行”：

- 同一批里没有依赖关系的 step 并发执行
- 有依赖的 step 放到下一批串行推进
- 当前最大只支持两批

这样可以保持快路径足够快，同时避免它膨胀成第二个完整 Agent loop。

### 第四步：收尾

工具执行完后，再调用一次 finalizer，返回二选一：

- `{"status":"answer","answer":"..."}`
- `{"status":"slow","reason":"..."}`

如果证据不够、结果冲突、或者不适合直接回答，finalizer 必须选择慢路径。

## 安全模型

### 只读工具白名单

快路径只允许：

- `search`
- `list_files`
- `file_read`
- `execute_command`

其他所有工具都会在计划校验阶段被拒绝。

### 只读命令白名单

`execute_command` 只在严格白名单下可用，并且会叠加 shell 控制符过滤。

主要保护包括：

- 拒绝命令拼接
- 拒绝重定向
- 拒绝不成对引号
- 只允许明确列出的只读命令

按平台区分：

- Windows
  - `git ...`
  - `rg ...`
  - `dir ...`
  - `type ...`
  - `where ...`
- 非 Windows
  - `git ...`
  - `rg ...`
  - `ls ...`
  - `cat ...`
  - `pwd`
  - `which ...`

其中 `git` 也只允许只读子命令，例如：

- `status`
- `diff`
- `log`
- `show`
- `rev-parse`
- `grep`
- 安全的 `branch` 查看

任何变更型子命令或危险 flag 都会被拒绝。

## 降级规则

快路径在以下任一情况都会自动降级到慢路径：

- 硬规则拒绝
- 分类器拒绝
- 规划器拒绝
- 计划校验失败
- 批次构建失败
- 任一工具执行失败
- 工具结果为空
- finalizer 认为证据不足
- 任一快路径旁路 LLM 调用报错

这是刻意设计的保守行为。快路径失败不应该让整次请求失败，而应该无损回退。

## 与慢路径的衔接

如果快路径在真正执行工具之前就被跳过：

- 会直接复用那次已经准备好的 prompt，进入慢路径

如果快路径已经执行了只读工具，但后面决定降级：

- 会把一组合成的 assistant tool-call 历史和 tool output 追加进 history
- 慢路径会基于这份扩展后的 history 重新构造上下文

这样就能保留快路径拿到的只读证据，而不是白做一遍。

## 事件与可观测性

新增两个 runtime 事件：

- `QuickPathSelected`
- `QuickPathDowngraded`

这些事件会：

- 通过 `AgentEvent` 暴露
- 被持久化进 transcript
- 通过 mobile bridge 事件流继续向上透出

同时，快路径工具执行仍然复用现有工具事件，所以 transcript replay 仍然能看到完整的只读证据链。

## 测试策略

第一批自动化测试先覆盖确定性最强的部分：

- 硬规则路由
- 只读命令校验
- 计划校验
- 执行批次构建

测试文件位于：

- `native/claude-code-rust/tests/fast_path_test.rs`

这批测试先把风险最高的边界锁住，不依赖在线 provider。

## 设计权衡

### 为什么不把快路径做成通用执行器

如果把快路径做成通用 DAG 执行器，或者允许它多轮自循环，它很快就会退化成另一个 AgentRuntime，失去“低延迟、强约束、可预测”的核心价值。

### 为什么坚持保守降级

这里宁可误杀，不可误放：

- 一个简单任务被错判成慢路径，代价只是慢一点
- 一个复杂或不安全任务被误放进快路径，代价会更高

所以这个设计优先接受 false negative，而不是冒险追求更高快路径命中率。

## 后续扩展

如果当前设计跑得稳定，后面比较合理的增强方向有：

- 在更多前端暴露用户可见的模式控制
- 更细的快路径观测指标
- 按 provider 选择更合适的辅助模型
- 对“快路径执行后再降级”的 transcript 做更好的摘要

当前版本会刻意停在“只读快路径”，不会继续扩展到写操作快路径。
