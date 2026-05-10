# Auto Dream 最终实现设计文档

## 目标

Auto Dream 是项目记忆的后台整理机制，不负责从当前对话中抽取新记忆。它在会话结束后以低成本门控判断是否需要整理项目记忆目录，并在手动 `/dream` 时立即运行同一套整理流程。

本实现将旧的 `~/.claude-code/memory.json` 聚类式实现替换为文档定义的 memdir 机制：围绕 `MEMORY.md`、主题文件、日志和 session JSONL 进行整理、剪裁、索引和状态记录。

## 路径模型

Auto Dream 使用项目记忆目录作为唯一写入对象。路径解析顺序如下：

1. 测试或调用方显式传入的 `AutoDreamConfig.memory_dir`。
2. 环境变量 `CLAUDE_COWORK_MEMORY_PATH_OVERRIDE`。
3. 配置项 `settings.memory.auto_memory_directory`。
4. 默认项目路径 `~/.claude/projects/<project-slug>/memory`。

默认路径会优先使用当前工作目录所在 Git 根目录生成稳定 slug；如果没有 Git 根，则使用工作目录本身。这样与 prompt assembly 读取 `MEMORY.md` 和 topic memory 的机制保持一致。

Session 来源默认使用 `~/.claude-code/query-engine/sessions`，也支持测试或嵌入方通过 `AutoDreamConfig.sessions_dir` 覆盖。状态文件默认写入 memdir 下的 `.autodream_state.json`。

## 触发机制

自动触发走三重门控，顺序从便宜到昂贵：

1. 配置开关：`settings.memory.enabled` 和 `AutoDreamConfig.enabled`。
2. 时间门控：距离上次整理至少 `minHours`，默认 24 小时。
3. 会话门控：上次整理后至少 `minSessions` 个 session 记录发生变化，默认 5 个。
4. 锁门控：memdir 中没有活动的 `.consolidate-lock`。

`QueryEngine::submit_message` 在一次运行结束后后台 spawn Auto Dream 检查，不阻塞用户响应。运行失败时也会记录失败 transcript 后触发一次后台检查。

手动 `/dream` 使用 `AutoDreamService::force_consolidation`，绕过时间和会话数量门控，但仍遵守锁，避免多个进程同时改写同一 memdir。

## 锁与状态

锁文件位于 memdir 的 `.consolidate-lock`：

- 运行中内容为 `pid:<pid>`。
- 成功完成后内容改为 `last:<pid>`。
- 文件 mtime 表示最近一次成功整理时间。
- `pid:` 锁超过 1 小时视为过期，可被新进程接管。
- 抢锁后会重新读取内容验证 holder，避免并发写入时误判。
- 整理失败会恢复抢锁前的内容和 mtime；如果抢锁前不存在锁，则删除临时锁。

`.autodream_state.json` 保存最近整理时间、累计 session 数、最后跳过原因和最近一次 run report。状态用于 CLI/status 展示，锁 mtime 仍是跨进程的轻量 truth source。

## 整理流程

本实现使用确定性本地 runner，避免后台整理依赖网络或 LLM 可用性。runner 会生成 `.last-dream-prompt.md`，保留文档定义的四阶段 Dream prompt，后续可以把 runner 替换为受限 forked sub-agent。

四阶段行为：

1. Orient：定位 memdir、读取 `MEMORY.md`、枚举已有主题文件。
2. Gather：收集主题文件、`logs/YYYY/MM/YYYY-MM-DD.md`、QueryEngine session `transcript.jsonl` 和旧版 session JSON。
3. Consolidate：将相对日期如 `yesterday`、`昨天`、`last week`、`上周` 归一化为绝对日期。
4. Prune and Index：重写 `MEMORY.md`，保证最多 200 行、最多 25KB、每行最多 150 字符，并优先保留本次发现的 topic/log/session 指针。

本地 runner 不写入代码架构、Git 历史或可从源码推导的信息；这些规则同时写入 `.last-dream-prompt.md`，方便后续 LLM runner 复用。

## CLI 与服务集成

- `memory dream` 现在调用 Auto Dream 手动整理，而不是旧 `MemoryManager::consolidate()`。
- `memory auto-dream` 保持强制运行 Auto Dream。
- `services auto-dream` 继续展示状态，并新增内部状态字段供 JSON/status 使用。
- `ServiceManager` 初始化仍创建 `AutoDreamService`，后台自动触发由 QueryEngine 在会话运行结束时负责。

## 测试覆盖

新增 `native/claude-code-rust/tests/auto_dream_test.rs`，覆盖：

- 手动 `/dream` 会整理 memdir、生成四阶段 prompt、剪裁 `MEMORY.md`。
- 自动触发按时间、session 数和 PID 锁顺序门控。
- 活动锁阻止运行，过期锁允许接管。
- 整理失败时恢复旧锁内容和 mtime。
- 禁用 Auto Dream 时不创建 memdir，并在 status 中报告 `disabled`。

这些测试先以 RED 方式验证旧实现缺失 API 和行为，再通过新实现转绿。
