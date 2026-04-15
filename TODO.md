# TODO - 天工全栈平台重构任务清单

> 最后更新：2026-04-15
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
- `Phase 14`：CoreConfig 配置注入已完成，CLI/GUI/Server 的配置同步与即时生效链路已打通。
- `Phase 15`：LLM 协议抽象与 Anthropic 支持已完成，统一 Provider 抽象、Anthropic transport、协议配置透传与兼容性处理已接入。

---

## Phase 16：架构收口与远程能力补齐 — **进行中**

> 对照 `docs/desktop-agent-technical-architecture.md` 的完成度盘点继续推进

### A. 统一入口与事件模型

- [ ] 梳理 GUI / Server / Gateway 当前入口差异，确认统一事件模型的目标边界
- [x] 收敛远程消息入口到统一事件流，减少 `TiangongState` 直连路径
- [x] 为系统通知、后台任务回流、远程输入补齐统一事件接入方式

### B. 远程角色与权限边界

- [x] 将 `control / approve / observe` 角色模型从结构定义推进到实际权限控制
- [x] 区分远程控制、审批、旁观的接口能力与会话可见范围
- [x] 让远程端共享本地权限审批链路，不允许绕过本地安全边界

### C. Core 与协议层收尾

- [x] 清理 `tiangong-core/src/model.rs` 中剩余兼容桥接和旧 helper
- [x] 继续限制 `tiangong-llm` 只承载抽象、映射和错误边界
- [ ] 将 OpenAI 兼容 transport 拆分保留为后续子任务，不阻塞当前主线功能

### D. 安全、审计与观测补齐

- [ ] 盘点路径级、网络目标级和外部作用域控制点的现状缺口
- [ ] 补齐高风险动作审计字段，覆盖会话、任务、代理、决策和结果摘要
- [ ] 对齐请求级 / 任务级 / 会话级成本统计与用户可见状态
