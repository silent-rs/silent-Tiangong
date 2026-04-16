# TODO - 天工全栈平台重构任务清单

> 最后更新：2026-04-16
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
- `Phase 16`：架构收口与远程能力补齐已完成，统一入口、Server 信任语义、远程成本可见性和历史文档收敛已落地。

---

## Phase 17：多媒体结果语义收敛 — **进行中**

> 对照 `docs/media-capability-architecture-adjustment.md` 继续推进

### A. 媒体结果模型

- [x] 为统一消息模型增加结构化媒体结果字段
- [x] 定义图片结果最小可用结构（URL / MIME / 来源能力 / 标题）
- [x] 保持文本内容与媒体内容并存，避免强制回退到 Markdown 文本

### B. 本地 GUI 图片链路

- [x] 将本地图片生成最终结果改为结构化媒体消息，而不是工具日志文本
- [x] 区分最终结果图片与中间过程图片，避免一律提升为最终 assistant 回复
- [x] 前端基于结构化媒体消息渲染图片，保留旧 Markdown 图片渲染作为兼容路径

### C. 后续扩展口

- [ ] 为视频结果预留相同的结构化媒体语义
- [ ] 约束 MCP 只作为媒体后端适配来源，不直接定义上层媒体消息语义
