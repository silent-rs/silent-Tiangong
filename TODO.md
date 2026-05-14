# TODO - 天工当前开发任务

> 最后更新：2026-05-13
> 当前主线：多智能体协作系统（Phase 19）
> 参考：`PLAN.md`、`docs/rfc/0011-multi-agent-collaboration.md`

---

## Phase A：基础框架

- [x] 定义 AgentDescriptor / AgentLifecycle / AgentStatus 类型（`agent_team/descriptor.rs`）
- [x] 实现 AgentRegistry — 会话级 Agent 注册表（`agent_team/registry.rs`）
- [x] 定义团队工具 Spec 并注册（`agent_team/tools.rs`）
- [x] 在 `inject_enhanced_tools` 中注册团队工具
- [x] 实现 Sub Agent ReactEngine 独立执行循环（`react/engine.rs::drain_sub_agent_inboxes`）
- [x] 实现 Sub Agent 生命周期管理：创建、解散（`agent_team/lifecycle.rs`）
- [x] 新增 StreamEvent：AgentCreated / AgentStatusChanged / AgentNotification / AgentMessage / FileLockChanged
- [x] ReactEngine 拦截团队工具调用（create_agent / dismiss_agent / send_message 等）
- [x] 前端：Agent Tab 栏基础展示（Agent 列表 + 状态指示器）

## Phase B：消息通讯

- [x] 定义 AgentMessage 类型（from / to / content / priority）
- [x] 实现 MessageBus 消息路由（`agent_team/message_bus.rs`）
- [x] 定义 `send_message` / `broadcast_message` 工具 Spec
- [x] 实现 Agent 收件箱：消息注入 ReactEngine 上下文
- [x] 新增 StreamEvent：AgentMessage
- [x] 前端：Agent 间消息流展示（系统消息形式）

## Phase C：文件编辑锁

- [x] 实现 FileLockManager（`agent_team/file_lock.rs`）
- [x] 定义 `lock_file` / `unlock_file` 工具 Spec
- [x] 集成到 `write_file` / `replace_in_file`：执行前检查锁状态
- [x] 锁超时机制（默认 300 秒自动释放）
- [x] Agent 销毁时自动释放所有持有锁
- [x] 主 Agent 拥有锁最高权限（可强制释放）
- [x] 新增 StreamEvent：FileLockChanged
- [x] 锁冲突状态报告（active_locks_summary）

## Phase D：前端交互

- [x] 定义 `notify_user` 工具 Spec，Sub Agent 可直接推送消息到前端
- [x] 新增 StreamEvent：AgentNotification（携带 agent_id + agent_label）
- [x] 前端：Agent Tab 切换 — 用户可查看不同 Agent 的执行细节
- [x] 前端：@提及输入 — 支持 @dev / @test / @all 语法
- [x] 前端：Agent 面板 — 显示团队成员列表、状态、手动关闭按钮
- [x] Tauri commands 层处理 Agent 相关事件转发

## Phase E：完善与优化

- [x] 临时 Agent 任务完成后自动销毁
- [x] Agent 错误恢复：Sub Agent 失败不影响团队其他 Agent
- [x] 并发限制：最大 8 个 Agent，同时运行 4 个 Sub Agent
- [x] Token 预算分配：Sub Agent 共享 200K token 预算，超出后暂停后续 Agent
- [x] system prompt 中补充团队工具使用指引
- [x] Sub Agent 并行执行（使用 futures::join_all 协作并发）

## 临时修复任务

- [x] 修复 LLM 调用失败日志缺少供应商与模型信息、provider 字段混用协议名导致误判的问题
- [x] 修复 Markdown 代码块内部误显示行内代码背景色的问题
- [x] 在前端展示上下文压缩开始和完成状态
- [x] 修复手动上下文压缩未真实触发 LLM 摘要的问题
- [x] 调整上下文压缩完成后的前端痕迹展示

---

## 文档同步要求

- `docs/requirements.md`：补充多智能体协作相关需求
- `docs/rfc/0011-multi-agent-collaboration.md`：实现过程中如有设计变更需同步更新
