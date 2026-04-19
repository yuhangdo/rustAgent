# Android UI 严格 MVI 重构设计 Spec

## 1. 文档目的

本文定义 Android UI 的最终态重构方案，用于将当前应用演进为一个基于 Jetpack Compose 的、严格意义上的 MVI 应用架构。

这份文档聚焦于设计，不包含实现代码，也不描述长期新旧双轨兼容策略。它定义的是一次性重构完成后的目标状态，供后续实现、拆任务和评审使用。

目标约束如下：

- `Compose-only`
- 严格 MVI
- 每个 feature 拥有独立的 `ViewModel` 类
- UI 主轴为对话产品，但前期内建运维控制台能力
- 允许为 UI 目标进行必要的跨层数据流重整

## 2. 当前问题诊断

当前 Android UI 已经具备可工作的 MVP，但并不符合严格 MVI 的定义，主要问题如下：

### 2.1 顶层导航状态不在统一状态容器中

`MainActivity` 当前通过本地 `rememberSaveable` 保存顶层 `destination`。这意味着路由切换属于 Compose 本地状态，而不是统一的应用状态源，无法纳入 reducer、effect、测试和恢复策略。

### 2.2 Feature 文件职责混杂

`Chat`、`Sessions`、`Settings` 目前都把 `UiState`、`Action`、`ViewModel`、`Screen`、局部组件放在单个 `*Feature.kt` 文件中。问题包括：

- 状态契约与 UI 呈现耦合
- ViewModel 不再是独立可替换单元
- 文件过大，不利于维护和测试
- feature 的边界不清晰，后续扩展会继续恶化

### 2.3 UI 决策状态散落在 repository flow 中

`SelectedSessionRepository` 目前承载“当前选中会话”这类明显属于 UI 决策的状态。它不是业务域模型，却又脱离了 UI 状态容器，导致：

- UI 状态来源不统一
- 页面切换与会话切换耦合在隐式共享流里
- 顶层导航、弹层、会话选择等跨页面行为难以形成一致的单向数据流

### 2.4 一次性事件与持久状态边界不清

当前页面中的错误提示、弹窗开关、查看 run、重试、取消等操作，部分通过状态字段表达，部分直接由 Compose 本地状态和回调驱动。缺少统一的 `Effect` 通道后：

- 单次事件和持久状态容易混在一起
- ViewModel 的职责容易膨胀
- UI 事件回放和测试粒度不稳定

### 2.5 Run Inspector 仍是“调试附属能力”

当前 run inspector 是一个附加诊断面板，尚未成为产品体验的一部分。聊天消息、运行状态、工具操作、推理信息和诊断时间线仍然是“并排存在”，而不是“同一条消息体验的一部分”。

### 2.6 UI 不是“聊天主轴 + 运维内建”的产品形态

当前 UI 更接近“聊天页面 + 调试信息补充”，还不是一个以消息为主轴、同时能自然查看深度思考链路和运行轨迹的 agent 产品。

## 3. 重构目标

本次 UI 重构的目标不是单纯拆文件，而是同时完成架构升级与产品体验重设计。

### 3.1 目标产品形态

新的 UI 采用“对话产品优先，但内建运维能力”的方向：

- 默认入口是聊天工作区
- 用户首先看到连续、流畅、可流式更新的对话体验
- 每条 assistant 回复都可展开查看深度思考与运行轨迹
- 诊断能力不再被藏进二级页面，而是成为消息体验的组成部分

### 3.2 目标架构形态

架构采用 `Feature Store + App Shell` 模式，而不是单一 root store：

- 顶层 `App Shell` 负责全局路由、全局反馈、全局弹层与恢复语义
- 每个 feature 各自拥有严格 MVI store 契约
- 每个 feature 必须有独立 ViewModel 类
- Reducer 纯函数化
- 业务状态不再依赖散落的 Compose 本地状态或裸露 repository flow

### 3.3 非目标

以下内容不在本次设计范围内：

- 重做 Room 数据库核心 schema，除非某项修改是 strict MVI 所必需
- 改写 provider 协议或底层 runtime 协议
- 引入长期兼容层或新旧 UI 双轨维护方案
- 讨论多平台 UI 共享

## 4. 产品设计原则

### 4.1 聊天优先

消息流、输入、流式回答、回复可操作性是第一层体验。页面初看应是一个强对话产品，而不是一个监控后台。

### 4.2 运维能力内建

运维能力不再是“开发者专属隐藏层”。运行状态、工具操作、失败诊断、推理摘要、响应来源应能自然嵌入聊天体验。

### 4.3 深度思考采用“混合结构化”语义

`Deep Thinking` 不等同于“原始 chain-of-thought 文本回显”。它的目标是提供一个稳定、跨 provider 可工作的思考视图：

- 优先展示结构化轨迹
- 在 provider 能提供 reasoning 文本时，再附加展示原文
- 不承诺所有 provider 都有完整原始推理文本

### 4.4 状态必须可解释、可追踪、可测试

所有业务型 UI 状态都必须来自 reducer 驱动的状态树。每一个重要状态变化都应能被测试明确验证。

### 4.5 UI 层只消费状态，不拥有业务事实

Compose 可以保留少量纯展示状态，但 UI 不应成为业务事实的真实来源。

## 5. 顶层信息架构

应用顶层仍保留 3 个入口：

- `Chat`
- `Sessions`
- `Settings`

这是为了维持现有产品理解成本，并避免过度引入复杂导航。

### 5.1 App Shell

`App Shell` 是新的顶层 UI 状态容器，职责包括：

- 当前 route 管理
- 顶层导航切换
- 全局 snackbar/banner
- 全局 modal/sheet/dialog 协调
- 返回语义与状态恢复
- 跨页面跳转 effect，如“从 Sessions 打开 Chat 并聚焦某个 session”

`MainActivity` 只保留 Android 容器职责，不再保留顶层 destination 本地状态。

## 6. Feature 功能设计

本节描述 UI 层每个功能的简单职责和目标形态。

### 6.1 Chat Workspace

`Chat Workspace` 是应用主界面和默认入口，承担以下职责：

- 显示当前会话的完整消息流
- 管理用户输入与发送行为
- 支持 assistant 回复的流式输出
- 显示发送中、运行中、失败、取消、可重试等消息级状态
- 将“查看 run”升级为消息级 `Deep Thinking` 入口
- 支持对具体 run 的取消与重试

UI 目标：

- 首屏一眼看上去是聊天产品
- 每条 assistant 回复都能清楚看见状态、模型来源和可操作项
- 不打断消息主轴地暴露调试能力

### 6.2 Deep Thinking / Run Trace

这是新的核心能力，而不是独立页面附属品。

它负责展示单条 assistant 回复背后的执行轨迹，包括：

- 推理阶段摘要
- 工具调用意图
- 工具调用结果
- 状态流转，如启动、请求构建、provider 选择、完成、失败、取消
- reasoning 原文（如 provider 返回）

UI 形态要求：

- 作为消息的展开区域、抽屉、底部面板或内嵌次级内容显示
- 与聊天主页面联动
- 对普通用户可读，对运维调试也足够有信息量

### 6.3 Sessions Hub

`Sessions Hub` 从简单列表升级为会话中心，职责包括：

- 浏览全部会话
- 切换当前会话
- 删除会话
- 识别会话最近一次运行状态
- 从会话中心快速返回聊天上下文

UI 重点：

- 弱化“只是一个列表”
- 强化会话健康度和最近响应情况
- 让用户快速知道哪个会话还在运行、哪个失败、哪个值得继续

### 6.4 Settings Studio

`Settings Studio` 从原始表单页升级为配置工作台。

职责包括：

- provider 类型切换
- 模型连接配置编辑
- 运行模式配置编辑
- 工作目录和环境配置
- 调试和实验相关配置
- 保存、校验反馈、保存结果提示

UI 重点：

- 分组展示
- 不再是一长串平铺输入框
- 明确区分“连接配置”和“运行配置”

## 7. 共享状态模式

以下 UI 模式在整个应用中需要统一表达，不允许各 feature 自行发明不同语义：

- `Loading`
- `Empty`
- `Error`
- `Retryable`
- `Cancelled`
- `StreamInterrupted`
- `Banner / Snackbar`
- `Dialog / Sheet / Modal`

统一策略如下：

- 持久状态进入 `State`
- 单次反馈进入 `Effect`
- 全局展示由 `App Shell` 协调
- 消息级反馈仍由 feature state 控制，但不绕开统一状态模型

## 8. 严格 MVI 架构设计

### 8.1 核心原则

严格 MVI 在本项目中的定义如下：

1. `Intent` 是 UI 进入系统的唯一入口
2. `ViewModel` 不直接拼接最终 `State`
3. `Mutation` 是状态变化的唯一中间表示
4. `Reducer` 是纯函数
5. `Effect` 只承载一次性事件
6. Screen 只消费 state 和发送 intent
7. 业务型 UI 状态不得由 `mutableStateOf` 或 `rememberSaveable` 直接持有

### 8.2 App 架构模式

采用 `Feature Store + App Shell`：

- `App Shell` 持有应用级状态
- `Chat`、`Sessions`、`Settings` 各自拥有 feature contract
- feature 内可以拥有自己的 reducer 和 ViewModel
- Shell 只负责全局协调，不吞并所有 feature 内部状态

不采用单一 root store 的原因：

- 当前 app 规模尚不足以支撑一个巨型全局状态树
- Chat、Sessions、Settings 的边界天然存在
- 单一 root store 会提升复杂度，并让日常演进成本上升

### 8.3 单向数据流

统一数据流如下：

`UI -> Intent -> ViewModel -> UseCase / Repository -> Result -> Mutation -> Reducer -> State -> Compose`

一次性事件流如下：

`UI -> Intent -> ViewModel -> Effect`

示例：

- 发送消息
- 切换 session
- 保存设置
- 打开某条消息的 Deep Thinking 面板
- 显示保存成功 snackbar

## 9. 公共接口与基础设施

`core/ui/mvi` 目录需要新增统一基础设施，并在 spec 中标准化如下类型。

### 9.1 基础 marker types

- `UiIntent`
- `UiState`
- `UiEffect`

这些类型用于统一 feature contract 的边界定义。

### 9.2 Reducer

新增统一 reducer 抽象：

`Reducer<State, Mutation>`

约束：

- 纯函数
- 无副作用
- 无 repository 调用
- 无 coroutine 调用
- 输入相同必须输出相同

### 9.3 MviViewModel

新增统一抽象：

`MviViewModel<Intent, State, Effect, Mutation>`

职责：

- 暴露 `StateFlow<State>`
- 暴露 effect 流
- 接收 intent
- 执行 use case / repository 协调
- 把结果映射为 mutation
- 把 one-off 行为映射为 effect

它不应：

- 直接在多个地方手工修改 state 字段
- 直接暴露可写 `MutableStateFlow` 给 UI
- 让 Compose 层感知 repository

### 9.4 App 级公共类型

spec 中需要明确以下公共契约：

- `AppShellContract`
- `ChatContract`
- `SessionsContract`
- `SettingsContract`
- `AppRoute`
- `DeepThinkingPanelState`
- `RunTraceItem`
- `ChatMessagePresentation`

## 10. Feature Contract 设计

每个 feature 都必须定义自己的 contract 文件，至少包含：

- `State`
- `Intent`
- `Effect`
- `Mutation`

### 10.1 AppShellContract

#### State

- 当前 route
- 全局 snackbar/banner 内容
- 当前全局 modal/sheet 状态
- 可能的 route 恢复信息

#### Intent

- 切换 route
- 消费全局反馈
- 打开/关闭全局 modal
- 处理来自 feature 的跨页跳转请求

#### Effect

- 导航副作用
- 系统级一次性提示

### 10.2 ChatContract

#### State 边界

- 当前 session 标识
- 当前 session 标题
- 消息展示列表
- composer 内容
- 发送状态
- 活动 run 数量
- 当前展开的 `DeepThinkingPanelState`
- 当前错误表现态

#### Intent

- 输入变更
- 发送消息
- 打开某条消息的 deep thinking
- 关闭 deep thinking
- 重试 run
- 取消 run
- 消费错误反馈

#### Effect

- 请求切换到某个全局 modal/sheet
- 请求顶层显示 snackbar
- 必要时请求 shell 级跳转

#### Presentation 模型

`ChatMessagePresentation` 负责把 transcript、run 状态、工具操作摘要、provider 信息拼接成稳定的 UI 输入模型，而不是让 Screen 自己拼装 domain 对象。

### 10.3 SessionsContract

#### State 边界

- 会话列表
- 当前选中会话
- 会话摘要与健康状态
- 空态/加载态/错误态

#### Intent

- 创建会话
- 选择会话
- 删除会话
- 刷新摘要

#### Effect

- 打开 Chat 并切换到指定 session
- 显示删除确认或全局反馈

### 10.4 SettingsContract

#### State 边界

- 当前表单草稿
- 分组后的配置项
- 保存中状态
- 字段级校验结果
- 表单级反馈信息

#### Intent

- provider 切换
- 字段编辑
- 保存
- 消费保存结果反馈

#### Effect

- 保存成功提示
- 保存失败提示
- 必要时请求全局 banner/snackbar

## 11. Deep Thinking 设计

### 11.1 设计目标

`Deep Thinking` 需要同时满足两类用户：

- 希望理解模型“做了什么”的普通用户
- 希望定位请求问题、工具调用和失败原因的运维/开发用户

### 11.2 展示语义

采用“混合结构化”设计：

- 第一层：结构化轨迹
- 第二层：工具调用与结果
- 第三层：provider 原始 reasoning 文本（如果存在）

### 11.3 RunTraceItem

`RunTraceItem` 是 deep thinking 的统一展示单元，类型可包括：

- 推理摘要项
- 工具调用项
- 工具结果项
- 状态流转项
- 失败诊断项
- 原始 reasoning 项

它不是数据库实体，而是 UI 层的表现模型。

### 11.4 UI 约束

- Deep Thinking 不应成为和聊天平级的大页面主导航
- 默认收起
- 打开后应保持上下文关联，明确属于哪条消息/哪次 run
- 不得直接把数据库实体原样暴露给 Compose

## 12. 目录蓝图

目标结构至少覆盖如下目录：

```text
app/src/main/java/com/yuhangdo/rustagent/
  core/ui/mvi/
  ui/appshell/
  feature/chat/
  feature/sessions/
  feature/settings/
```

### 12.1 core/ui/mvi

职责：

- MVI 基础接口
- ViewModel 基类
- reducer 抽象
- effect 分发与测试辅助
- 可复用 UI 状态工具

典型文件：

- `MviViewModel.kt`
- `Reducer.kt`
- `MviTypes.kt`
- `MviTestHelpers.kt`

### 12.2 ui/appshell

职责：

- App shell contract
- shell reducer
- shell ViewModel
- shell route 容器
- 顶层 scaffold、snackbar host、global sheet host

典型文件：

- `AppShellContract.kt`
- `AppShellReducer.kt`
- `AppShellViewModel.kt`
- `AppShellRoute.kt`
- `AppShellScreen.kt`

### 12.3 feature/chat

职责：

- Chat feature contract
- reducer
- ViewModel
- screen
- message list / composer / deep thinking 等组件
- UI 展现模型映射

典型文件：

- `ChatContract.kt`
- `ChatReducer.kt`
- `ChatViewModel.kt`
- `ChatRoute.kt`
- `ChatScreen.kt`
- `components/MessageCard.kt`
- `components/ComposerBar.kt`
- `components/DeepThinkingPanel.kt`
- `presentation/ChatMessagePresentation.kt`

### 12.4 feature/sessions

职责：

- 会话中心 contract
- reducer
- ViewModel
- screen
- session card / empty state / summary 组件

典型文件：

- `SessionsContract.kt`
- `SessionsReducer.kt`
- `SessionsViewModel.kt`
- `SessionsRoute.kt`
- `SessionsScreen.kt`
- `components/SessionCard.kt`

### 12.5 feature/settings

职责：

- 设置工作台 contract
- reducer
- ViewModel
- screen
- provider 分组面板 / 表单组件 / 保存反馈组件

典型文件：

- `SettingsContract.kt`
- `SettingsReducer.kt`
- `SettingsViewModel.kt`
- `SettingsRoute.kt`
- `SettingsScreen.kt`
- `components/ProviderSection.kt`
- `components/RuntimeSection.kt`

### 12.6 结构约束

重构完成后不得继续保留“单个 `*Feature.kt` 巨文件”作为 feature 的主要组织方式。

## 13. Compose 层约束

### 13.1 Route 和 Screen 分离

每个 feature 推荐采用：

- `Route`：负责拿 ViewModel、收集 `state/effect`
- `Screen`：纯渲染层，只接收 `state` 和 `onIntent`

这样可以把 Compose 与依赖装配分开。

### 13.2 本地状态使用边界

Compose 本地状态只允许用于纯展示目的，例如：

- 列表滚动位置
- 动画开关
- 文本展开/收起
- 局部控件的视觉展开状态

以下内容禁止使用本地状态持有：

- 当前 route
- 当前 session 选择
- 当前消息流业务状态
- 是否发送中
- 是否保存中
- 当前错误业务状态

### 13.3 effect 消费

一次性事件如 snackbar、导航、打开全局弹层，必须从 effect 流消费，而不是通过在 state 中塞入临时字段再手工清空。

## 14. UI 支撑层重整边界

为了达成严格 MVI，本设计允许对 UI 上游支撑层做有限重整。

### 14.1 允许的改动

- 抽出 UI-facing use case / observer 层
- 重新定义 shell 与 feature 的状态来源
- 回收 `selectedSessionId` 这类 UI 决策状态
- 为 presentation 层补充映射模型
- 统一 ViewModel 注入方式

### 14.2 不在本轮主动重定义的内容

- Room 底层 schema 的业务意义
- provider 协议和 API 形状
- runtime 层对外协议

### 14.3 建议的 UI-facing 协调层

建议在 repository 之上补充协调层，例如：

- `ObserveChatTimelineUseCase`
- `SendMessageUseCase`
- `ObserveSessionSummariesUseCase`
- `SelectSessionUseCase`
- `UpdateProviderSettingsUseCase`

这些协调层的职责是为 ViewModel 提供稳定、面向 UI 的输入输出，而不是让 ViewModel 直接组装复杂业务流程。

## 15. 导航与共享状态重整

### 15.1 导航进入 Shell Contract

当前顶层 destination 不再保留在 `MainActivity` 的 `rememberSaveable` 中，而是进入 `AppShellContract.State`。

### 15.2 Session 选择不再裸露

`selectedSessionId` 不再作为裸 repository 共享流被 UI 各处直接写入。它需要被纳入 shell 或 feature 协调层统一管理。

### 15.3 全局反馈收口

全局 snackbar、banner、modal、sheet 的展示权收口到 `App Shell`，各 feature 通过 effect 请求，而不是自行在局部“偷偷”持有。

## 16. 测试设计

本次 spec 必须把测试策略作为目标架构的一部分，而不是实现结束后的补充项。

### 16.1 Reducer 单元测试

验证内容：

- reducer 为纯函数
- 状态不可变
- 每种 mutation 的状态变化正确
- 边界状态转换清晰

重点场景：

- route 切换
- 发送中/完成/失败/取消
- settings 保存中/成功/失败
- session 删除后的选择切换

### 16.2 ViewModel Intent / Effect 测试

使用 `Turbine` 验证：

- intent 到 mutation 的顺序
- intent 到 effect 的触发条件
- 单次事件不会混入 state
- 异步任务结束后状态稳定可预测

### 16.3 Chat 场景测试

至少覆盖：

- 流式输出状态更新
- provider 失败
- run 取消
- run 重试
- deep thinking 展开
- provider 缺少 reasoning 时的回退策略

### 16.4 Sessions 场景测试

至少覆盖：

- 创建会话
- 切换会话
- 删除当前会话
- 会话最近运行状态摘要刷新

### 16.5 Settings 场景测试

至少覆盖：

- provider 切换
- 字段编辑
- 保存成功
- 保存失败
- Embedded Rust Runtime 相关配置显隐

### 16.6 App Shell 测试

至少覆盖：

- 顶层导航切换
- 全局反馈展示
- 返回语义
- 状态恢复语义

## 17. 验收标准

重构完成后应满足以下验收标准：

1. UI 层不存在业务型 `mutableStateOf` 或散落的共享状态
2. 每个 feature 均有独立 `ViewModel` 类
3. 每个 feature 都定义 `State / Intent / Effect / Mutation`
4. 所有状态变化都通过 reducer 完成
5. 所有一次性事件都通过 effect 流发出
6. `MainActivity` 不再持有顶层业务状态
7. `Chat` 主界面支持流式回答
8. 每条 assistant 回复支持消息级 `Deep Thinking / Run Trace`
9. `Sessions` 能呈现会话健康度和最近运行摘要
10. `Settings` 以工作台方式分组展示配置

## 18. 风险与设计防线

### 18.1 风险：重新退化为“MVVM + 零散状态”

防线：

- 严格要求 mutation + reducer
- 禁止 ViewModel 直接手工拼 state
- 禁止导航状态继续保存在 Activity 本地状态里

### 18.2 风险：Deep Thinking 变成原始文本堆砌

防线：

- 结构化轨迹优先
- 原始 reasoning 只作为附加内容
- 明确 UI 层使用 `RunTraceItem` 展现模型

### 18.3 风险：聊天主轴被运维信息压制

防线：

- Chat 仍为默认入口
- 运维能力以消息级嵌入方式出现
- 不把 run trace 升级为顶层主导航

## 19. 实施建议

虽然本文定义的是一次性重写后的最终态，但实现阶段建议按如下顺序推进，以降低集成风险：

1. 先建立 `core/ui/mvi` 与 `ui/appshell`
2. 再拆分 `Chat`
3. 然后迁移 `Sessions`
4. 最后迁移 `Settings`
5. 收口共享状态与全局反馈

注意：这只是推荐实施顺序，不代表设计上保留长期过渡态。

## 20. 假设与默认选择

- 文档为中文
- 文档路径固定为 `app/docs/2026-04-19-strict-mvi-ui-redesign-spec.md`
- 视觉方向采用“对话产品优先，但早期内建运维控制台能力”
- `Deep Thinking` 采用混合结构化语义
- 目标架构采用 `Feature Store + App Shell`
- 文档定义的是最终态，不设计长期兼容层
