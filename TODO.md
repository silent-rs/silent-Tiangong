# TODO - 天工当前开发任务

> 最后更新：2026-05-12
> 当前主线：多智能体协作系统（Phase 19）
> 参考：`PLAN.md`、`docs/rfc/0011-multi-agent-collaboration.md`

---

## Phase A：基础框架

- [ ] 定义 AgentDescriptor / AgentLifecycle / AgentStatus 类型（`agent_team/descriptor.rs`）
- [ ] 实现 AgentRegistry — 会话级 Agent 注册表（`agent_team/registry.rs`）
- [ ] 定义 `create_agent` / `dismiss_agent` 工具 Spec（`agent_team/tools.rs`）
- [ ] 在 `inject_enhanced_tools` 中注册团队工具
- [ ] 实现 Sub Agent ReactEngine 启动：独立 Session + 独立工具集 + 专属 system prompt
- [ ] 实现 Sub Agent 生命周期管理：启动、停止、销毁（`agent_team/lifecycle.rs`）
- [ ] 新增 StreamEvent：AgentCreated / AgentStatusChanged
- [ ] ReactEngine 拦截 `create_agent` / `dismiss_agent` 工具调用
- [ ] 前端：Agent Tab 栏基础展示（Agent 列表 + 状态指示器）

## Phase B：消息通讯

- [ ] 定义 AgentMessage 类型（from / to / content / priority）
- [ ] 实现 MessageBus 消息路由（`agent_team/message_bus.rs`）
- [ ] 定义 `send_message` / `broadcast_message` 工具 Spec
- [ ] 实现 Agent 收件箱：收到的消息作为用户消息注入 ReactEngine 上下文
- [ ] 新增 StreamEvent：AgentMessage
- [ ] 前端：Agent 间消息流展示

## Phase C：文件编辑锁

- [ ] 实现 FileLockManager（`agent_team/file_lock.rs`）
- [ ] 定义 `lock_file` / `unlock_file` 工具 Spec
- [ ] 集成到 `write_file` / `replace_in_file`：执行前检查锁状态
- [ ] 锁超时机制（默认 300 秒）
- [ ] Agent 销毁时自动释放所有持有锁
- [ ] 主 Agent 拥有锁最高权限（可强制释放）
- [ ] 新增 StreamEvent：FileLockChanged

## Phase D：前端交互

- [ ] 定义 `notify_user` 工具 Spec，Sub Agent 可直接推送消息到前端
- [ ] 新增 StreamEvent：AgentNotification（携带 agent_id + agent_label）
- [ ] 前端：Agent Tab 切换 — 用户可查看不同 Agent 的执行细节
- [ ] 前端：@提及输入 — 支持 @dev / @test / @all 语法
- [ ] 前端：Agent 面板 — 显示团队成员列表、状态、手动关闭按钮
- [ ] Tauri commands 层处理 Worker/Agent 事件转发

## Phase E：完善与优化

- [ ] 临时 Agent 任务完成后自动销毁
- [ ] 死锁检测：定期扫描锁等待图
- [ ] Agent 错误恢复：单个 Agent 失败不影响团队
- [ ] 并发限制：最大 8 个 Agent，同时运行 4 个 Sub Agent
- [ ] Token 预算分配：主 Agent 60%，Sub Agent 共享 40%
- [ ] system prompt 中补充团队工具使用指引

---

## 文档同步要求

- `docs/requirements.md`：补充多智能体协作相关需求
- `docs/rfc/0011-multi-agent-collaboration.md`：实现过程中如有设计变更需同步更新
