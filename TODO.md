# TODO - 天工全栈平台重构任务清单

> 最后更新：2026-04-14
> 当前主线 RFC：`docs/rfc/0004-full-stack-agent-platform.md`
> 参考：`PLAN.md`、`docs/requirements.md`

---

## 已完成阶段摘要

- `Phase 2`：Skill 管理 MVP 已完成，包含安装、启停、卸载、锁文件、托管 MCP 清理、事务回滚与审计。
- `Phase 3`：Workspace 拆分与核心抽离已完成，`tiangong-core` / `tiangong-cli` / `tiangong-entry` / `src-tauri` 已稳定运行。
- `Phase 4`：Server 模式已完成，REST、WebSocket、Token 认证、后台运行与停止命令已落地。
- `Phase 5`：Gateway 与 EventBus 已完成，统一消息模型和消息路由已投入使用。
- `Phase 6`：Connector 框架已完成，Webhook、Telegram、Discord、Lark 已接入。
- `Phase 7`：多媒体框架已完成，图片生成、STT、TTS 与媒体任务模型已接入主链路。
- `Phase 8`：生产化增强已完成，日志分级、错误恢复、配置脱敏、配置热重载已上线。
- `Phase 9`：模型配置重构与多媒体集成已完成，`ModelsConfig` 三层架构与多媒体 routing 已稳定。
- `Phase 10`：GUI/CLI 友好交互改造已完成，解释文本和工具调用支持实时展示。
- `Phase 11`：运行时基础设施补全已完成，统一任务模型、查询编排、恢复、权限和成本治理已落地。
- `Phase 12`：事件驱动循环运行时已完成，EventLoopRunner 已替代旧的主执行链。
- `Phase 13`：`TiangongCore` 纯粹化与统一类型已完成，CLI/GUI/Server 已统一到核心流事件接口。
- `Phase 13.5`：LLM 请求容错已完成，重试、错误展示、审批流程和工具展示优化已接入。

---

## Phase 14：CoreConfig 配置注入 — **当前阶段**

> RFC：`docs/rfc/0006-core-config-provider.md`

### Phase C：验证

- [x] `cargo clippy --workspace` 通过
- [x] `cargo nextest run --workspace` 通过
- [ ] GUI 验证：切换模型/MCP/Skill 后下一轮对话生效
- [x] CLI 验证：`/model deepseek-chat` 后发送 `你好`，成功按新 routing 完成下一轮回复

---

## Phase 15：LLM 协议抽象与 Anthropic 支持 — **进行中**

### A. Provider 协议建模

- [x] `ModelsConfig::ProviderConfig` 增加协议类型字段，支持 `openai_compatible` / `anthropic`
- [x] 保持旧配置兼容：未配置协议类型时默认视为 `openai_compatible`
- [x] GUI / CLI 配置读写链路透传协议类型

### B. Core 协议抽象

- [x] 新增独立 crate `tiangong-llm`
- [x] 将 Anthropic / OpenAI provider 抽到 `tiangong-llm`
- [ ] 清理 `tiangong-core/src/model.rs` 中剩余兼容桥接和旧 helper
- [x] 保持 `ModelClient` 统一接口不变，避免上层 Runtime / Core / CLI / GUI 感知协议差异
- [x] 保留现有重试、错误提示与 `StreamEvent::Retry` 语义

### C. Anthropic Messages 适配

- [x] 在 `tiangong-core` 内部新增统一 LLM Provider 抽象、领域模型和错误边界
- [x] 新增独立 crate `tiangong-anthropic`
- [x] `tiangong-anthropic` 内部使用 `reqwest + serde` 实现 Anthropic Messages 与 SSE
- [x] `tiangong-llm` 的 Anthropic provider 切换到 `tiangong-anthropic`，第三方类型不泄漏到上层
- [x] 删除 Anthropic 路径对 `async-anthropic` / `anthropic-async` 的依赖
- [x] 支持 Anthropic 普通对话请求（`complete`）
- [x] 支持 Anthropic 流式对话请求（`complete_stream`）
- [x] 支持 Anthropic 工具调用请求（`complete_with_functions` / `complete_with_functions_stream`）
- [x] 支持 `lite` 模型调用与 `chat` 路由分离

### D. 集成与验证

- [x] `cargo check --workspace` 通过
- [x] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [x] `cargo nextest run --workspace` 通过
- [x] `yarn --cwd frontend build` 通过
