# RFC 0013: Sub Agent 架构重构

- **状态**: Draft
- **日期**: 2025-07-11
- **作者**: Hubert Shelley
- **前置**: RFC 0011 (多智能体协作系统)

## 1. 概述

### 1.1 背景

RFC 0011 定义了多智能体协作系统并已落地实现。经过实际使用和代码 review，发现以下问题：

1. **生命周期复杂度高**：`Temporary` / `Persistent` 双生命周期增加了调度和销毁逻辑的复杂度，LLM 也难以准确判断何时该用 Temporary
2. **Sub Agent 缺少团队感知**：删除 `sub_agent_team_context_message` 后，Sub Agent 不知道当前团队中有哪些成员，无法有效协作
3. **能力隔离过度**：Sub Agent 无法使用 `index_search`，导致独立会话中无法搜索代码；`recall_memory` 完全不可用
4. **System Prompt 构建路径不清晰**：Sub Agent 的 `system_prompt` 参数作为首轮用户消息传入，而非融入 system prompt 主体
5. **会话恢复丢失历史**：重启后 Sub Agent 的 child_session 被重建为空，丢失全部对话上下文
6. **消息注入可能丢失**：`dispatch_agent_message` 中 `tx.send` 失败时静默丢弃

### 1.2 目标

在保持 RFC 0011 核心设计（统一 ReactEngine、独立 Session、共享 TeamContext）的基础上：

- 简化生命周期模型
- 建立 Sub Agent 的团队感知机制
- 按需赋予 Sub Agent 索引和记忆能力
- 明确 System Prompt 的分层构建
- 提升消息投递可靠性

### 1.3 非目标

- 不改变前端 AgentPanel 的交互模式
- 不改变 StreamEvent 的事件定义
- 不引入 Sub Agent 嵌套创建（Sub Agent 仍不能创建 Sub Agent）

---

## 2. 生命周期简化

### 2.1 移除 Temporary 生命周期

**现状**：`AgentLifecycle` 有 `Persistent` 和 `Temporary` 两个变体。Temporary Agent 在 `drain_sub_agent_inboxes` 完成后自动销毁（释放锁 + unregister + 发事件）。

**问题**：
- LLM 难以准确判断一个 Agent 是否"临时"
- 自动销毁逻辑增加了 `drain_sub_agent_inboxes` 的复杂度
- Temporary Agent 执行完一轮后如果用户还想追问，Agent 已不存在

**方案**：移除 `AgentLifecycle` 枚举，所有 Sub Agent 统一为 Persistent，只能通过 `dismiss_agent` 主动解散。

### 2.2 改动清单

```
descriptor.rs
  - 删除 AgentLifecycle 枚举
  - AgentDescriptor 移除 lifecycle 字段

tools.rs
  - create_agent 工具移除 lifecycle 参数

lifecycle.rs
  - execute_create_agent: 移除 lifecycle 解析和校验
  - restore_agents_from_session_history: 移除 lifecycle 解析
  - parse_agent_created_message: 移除 lifecycle 解析

engine.rs (drain_sub_agent_inboxes)
  - 移除 Temporary Agent 自动销毁分支
  - 所有 Agent 执行完毕后统一保存 child_session + 恢复 Idle
```

### 2.3 create_agent 工具新签名

```json
{
  "name": "create_agent",
  "description": "创建一个 Sub Agent 加入团队。Agent 拥有独立的执行上下文和指定角色，持续存在直到被解散。",
  "input_schema": {
    "type": "object",
    "properties": {
      "role": {
        "type": "string",
        "description": "Agent 角色标识，用于 @提及（如 'pm'、'dev'、'test'）"
      },
      "label": {
        "type": "string",
        "description": "Agent 显示名称（如 'Project Manager'、'Developer'）"
      },
      "system_prompt": {
        "type": "string",
        "description": "Agent 的角色系统提示，定义其职责和行为规范"
      },
      "tools": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Agent 可用的工具列表。不指定时继承你的全部工具（不含 create_agent/dismiss_agent）。建议根据任务需要精确授权。"
      }
    },
    "required": ["role", "label", "system_prompt"]
  }
}
```

---

## 3. System Prompt 分层构建

### 3.1 设计原则

- `SystemPromptConfig` 保持纯配置快照语义，不混入运行时状态
- Sub Agent 的 prompt 构建通过独立类型 `SubAgentPromptContext` 完成
- 公共部分（规则、环境、动态段、摘要）提取为共享函数，消除重复

### 3.2 类型设计

```rust
/// Sub Agent 的 system prompt 构建上下文
///
/// 将基础 SystemPromptConfig（纯配置快照）与运行时上下文
/// （角色指令、团队成员列表）组合，构建 Sub Agent 专属的 system prompt。
///
/// 不修改 SystemPromptConfig，避免配置加载逻辑被运行时状态污染。
pub struct SubAgentPromptContext<'a> {
    /// 基础配置快照（引用，不持有）
    base: &'a SystemPromptConfig,
    /// Main Agent 生成的角色特化指令
    role_prompt: &'a str,
    /// 当前团队成员列表文本
    team_roster: &'a str,
}
```

### 3.3 Prompt 结构对比

```
Main Agent System Prompt:
  ┌─────────────────────────────┐
  │ 身份块 (identity_block)      │
  │ 规则块 (rules_block)         │
  │ 用户自定义指令               │
  │ 环境段 (工作目录、文件根)     │
  │ 动态段 (多媒体、Skills、团队) │
  │ 用户偏好与记忆上下文          │
  │ 对话摘要                     │
  └─────────────────────────────┘

Sub Agent System Prompt:
  ┌─────────────────────────────┐
  │ 角色特化指令 (role_prompt)    │  ← 替代通用身份块
  │ 规则块 (rules_block)         │  ← 共享
  │ 环境段 (工作目录、文件根)     │  ← 共享
  │ 动态段 (多媒体、Skills、团队) │  ← 共享
  │ 用户偏好与记忆上下文          │  ← 共享
  │ 当前团队成员列表              │  ← Sub Agent 独有
  │ 对话摘要                     │  ← 共享
  └─────────────────────────────┘
```

### 3.4 公共函数提取

```rust
/// 收集环境段（工作目录、文件根）
fn collect_environment_parts(session: &Session) -> Vec<String>;

/// 收集动态段（多媒体、Skills、团队协作、用户上下文）
fn collect_dynamic_parts(config: &SystemPromptConfig) -> Vec<String>;

/// 收集摘要段
fn collect_summary_part(session: &Session) -> Option<String>;

/// 组装最终的 System Message
fn assemble_system_message(parts: Vec<String>) -> Message;
```

`build_full_system_prompt` 和 `SubAgentPromptContext::build` 均基于这些公共函数构建，消除代码重复。

### 3.5 调用侧改动

**之前**（`spawn_ready_sub_agents`）：

```rust
// system_prompt 作为 user_input 传入 execute_turn
let prompt = system_prompt;
let usage = sub_engine.execute_turn(
    &mut child_session, &prompt, &child_stream_tx,
    &mut sub_cmd_rx, None, None,
).await;
```

**之后**：

```rust
// 通过 SubAgentPromptContext 构建 system prompt
let base_config = SystemPromptConfig::from_configs(
    self.engine.models_config(),
    self.engine.agent_config(),
    &child_session.id,
);
let team_roster = format_team_roster_from_registry(&team_arc);
let ctx = SubAgentPromptContext::new(&base_config, &system_prompt, &team_roster);
child_session.system_prompt_message = Some(ctx.build(&child_session));

// user_input 为空，首轮消息通过 Command::Message 注入
let usage = sub_engine.execute_turn(
    &mut child_session, "", &child_stream_tx,
    &mut sub_cmd_rx, memory_handle, index_manager,
).await;
```

---

## 4. 团队感知机制

### 4.1 设计

Sub Agent 通过两个渠道感知团队成员：

| 渠道 | 时机 | 内容 |
|------|------|------|
| System Prompt | 创建/重建时 | 当前所有活跃成员列表 |
| 消息通知 | 新成员加入时 | 增量通知 |

### 4.2 System Prompt 中的成员列表

`SubAgentPromptContext` 的 `team_roster` 字段在构建时从 registry 生成：

```rust
fn format_team_roster_from_registry(team_arc: &Arc<Mutex<TeamContext>>) -> String {
    let Ok(team) = team_arc.lock() else { return String::new(); };
    let mut agents = team.registry.alive_agents();
    agents.sort_by(|a, b| a.role.cmp(&b.role));
    agents.iter().map(|a| {
        format!("- {} (@{})", a.label, a.role)
    }).collect::<Vec<_>>().join("\n")
}
```

生成的文本示例：

```
当前团队成员：
- Developer (@dev)
- Project Manager (@pm)
- Tester (@test)
```

### 4.3 新成员加入通知

`execute_create_agent` 末尾向所有已存在的 Agent 发送通知：

```rust
// execute_create_agent 末尾追加
let notification = format!("[团队通知] 新成员 {} (@{}) 已加入团队", label, role);
for alive in team.registry.alive_agents() {
    if alive.agent_id != agent_id {
        team.dispatch_agent_message(
            &alive.agent_id,
            AgentMessage {
                id: scru128::new().to_string(),
                from: "system".to_string(),
                to: alive.agent_id.clone(),
                content: notification.clone(),
                priority: MessagePriority::Normal,
                created_at: now_text(),
            },
            Vec::new(),
        );
    }
}
```

### 4.4 System Prompt 重建时机

System Prompt 在以下时机重建（此时 team_roster 自动更新为最新成员列表）：

- Sub Agent 首次执行（`system_prompt_message` 为 None）
- 上下文压缩后（`maybe_update_context_summary`）
- 上下文清空后（`reset_context_for_session`）

---

## 5. 能力赋予

### 5.1 设计原则

Sub Agent 的能力完全由 Main Agent 通过 `create_agent` 的 `tools` 参数决定。不指定时继承 Main Agent 的全部工具（排除 `create_agent` 和 `dismiss_agent`）。

### 5.2 Index 能力

**现状**：`spawn_ready_sub_agents` 中 `index_manager` 传 `None`，Sub Agent 调用 `index_search` 返回"未初始化"。

**方案**：共享 Main Agent 的 IndexManager。

```rust
// spawn_ready_sub_agents 中
let has_index = tool_names.contains(&"index_search".to_string());
let sub_index = if has_index {
    index_manager  // 共享 Main Agent 的 IndexManager
} else {
    None
};
```

IndexManager 是无状态查询接口，共享无并发问题。Sub Agent 在同一 workspace 下工作，索引数据完全一致。

### 5.3 Memory Recall 能力

**现状**：`spawn_ready_sub_agents` 中 `memory_handle` 传 `None`。

**方案**：由 Main Agent 通过 `tools` 参数决定是否授权。授权时共享 Main Agent 的 MemoryHandle。

```rust
let has_memory = tool_names.contains(&"recall_memory".to_string());
let sub_memory = if has_memory {
    memory_handle  // 共享 Main Agent 的 MemoryHandle
} else {
    None
};
```

**记忆策略**：共享读取，统一写入。Sub Agent 的 `recall_memory` 查询全局记忆库，但记忆写入由 Main Agent 统一管理——Sub Agent 的 turn result 通过 `deliver_main_message` 回传给 Main Agent，由 Main Agent 提交记忆候选。

### 5.4 工具过滤

`spawn_ready_sub_agents` 中根据 `AgentDescriptor.tools` 过滤可用工具：

```rust
let sub_tools: Vec<ToolSpec> = self
    .tools
    .iter()
    .filter(|t| tool_names.iter().any(|name| name == &t.name))
    .filter(|t| !matches!(t.name.as_str(), "create_agent" | "dismiss_agent"))
    .cloned()
    .collect();
```

如果 Main Agent 未在 `tools` 中包含 `index_search` 或 `recall_memory`，对应工具不会出现在 Sub Agent 的工具列表中，LLM 不会尝试调用。

---

## 6. 消息投递可靠性

### 6.1 问题

`TeamContext::dispatch_agent_message` 中，当目标 Agent 正在运行时通过 `tx.send` 注入消息，但发送失败时静默丢弃：

```rust
pub(crate) fn dispatch_agent_message(&mut self, agent_id: &str, message: AgentMessage, ...) {
    if let Some(tx) = self.active_agent_senders.get(agent_id) {
        let _ = tx.send(Command::Message { ... });  // ← 失败时静默丢弃
    } else {
        self.registry.deliver_message(agent_id, message);
    }
}
```

### 6.2 方案

发送失败时 fallback 到收件箱：

```rust
pub(crate) fn dispatch_agent_message(&mut self, agent_id: &str, message: AgentMessage, media: ...) {
    let dispatched = if let Some(tx) = self.active_agent_senders.get(agent_id) {
        tx.send(Command::Message {
            content: format_agent_message_for_prompt(&message),
            message_id: Some(message.id.clone()),
            media,
        }).is_ok()
    } else {
        false
    };

    if !dispatched {
        self.registry.deliver_message(agent_id, message);
        if let Some(waker) = &self.dispatch_waker {
            let _ = waker.send(());
        }
    }
}
```

---

## 7. 会话恢复改进

### 7.1 现状

`restore_agents_from_session_history` 从主会话 System 消息中解析 Agent 事件并重建注册表，但 child_session 被创建为空，丢失全部对话历史。

### 7.2 方案

将 child_session 持久化到磁盘，恢复时从磁盘加载。

**持久化路径**：

```
{sessions_dir}/{session_id}/agents/{agent_id}.json
```

**持久化时机**：
- Sub Agent 执行完毕后（`drain_sub_agent_inboxes` 结果处理阶段）
- 会话持久化时同步持久化所有 child_session

**恢复时机**：
- `restore_agents_from_session_history` 中，优先从磁盘加载 child_session
- 磁盘文件不存在时 fallback 到创建空 session

### 7.3 改动清单

```rust
// lifecycle.rs
fn restore_agents_from_session_history(...) {
    // ...
    let child_session = match load_child_session(&sessions_dir, &agent.agent_id) {
        Some(session) => session,
        None => create_child_session(parent_session, &agent.label),
    };
    team.registry.register_with_session(descriptor, child_session);
}

// 新增
fn load_child_session(sessions_dir: &Path, agent_id: &str) -> Option<Session>;
fn persist_child_session(sessions_dir: &Path, agent_id: &str, session: &Session);
```

---

## 8. 改动总览

### 8.1 文件改动

| 文件 | 改动类型 | 说明 |
|------|----------|------|
| `agent_team/descriptor.rs` | 修改 | 删除 `AgentLifecycle` 枚举，`AgentDescriptor` 移除 `lifecycle` 字段 |
| `agent_team/tools.rs` | 修改 | `create_agent` 移除 `lifecycle` 参数 |
| `agent_team/lifecycle.rs` | 修改 | 移除 lifecycle 处理；新增团队通知广播；新增 child_session 持久化 |
| `agent_team/registry.rs` | 不变 | — |
| `agent_team/message_bus.rs` | 不变 | — |
| `agent_team/file_lock.rs` | 不变 | — |
| `prompt/sections.rs` | 修改 | 新增 `SubAgentPromptContext`；提取公共函数 |
| `prompt/mod.rs` | 修改 | 导出 `SubAgentPromptContext` |
| `react/engine.rs` | 修改 | `spawn_ready_sub_agents` 使用 `SubAgentPromptContext`；传入 `index_manager` 和条件性 `memory_handle`；移除 Temporary 销毁逻辑 |
| `react/context.rs` | 不变 | — |
| `session.rs` | 不变 | — |
| `core/mod.rs` | 修改 | 将 `index_manager` 传入 team 调度链路 |

### 8.2 不改动的部分

- `tiangong-types`：StreamEvent 定义不变
- `tiangong-server`：远端消费逻辑不变
- `src-tauri/commands.rs`：Tauri 命令层不变
- `frontend/`：AgentPanel 和消息展示逻辑不变
- `SystemPromptConfig`：不添加任何新字段

---

## 9. 实现路径

### Phase 1：生命周期简化 + 消息可靠性

- 删除 `AgentLifecycle` 枚举
- `create_agent` 移除 `lifecycle` 参数
- `drain_sub_agent_inboxes` 移除 Temporary 自动销毁
- `dispatch_agent_message` 增加 fallback

**验证**：创建的 Agent 不再自动销毁，只能通过 `dismiss_agent` 解散。消息投递不再丢失。

### Phase 2：System Prompt 分层构建

- 提取公共函数（`collect_environment_parts`、`collect_dynamic_parts`、`collect_summary_part`、`assemble_system_message`）
- 新增 `SubAgentPromptContext`
- `spawn_ready_sub_agents` 改用 `SubAgentPromptContext` 构建 system prompt

**验证**：Sub Agent 的 system prompt 包含角色特化指令、环境信息和团队成员列表。

### Phase 3：能力赋予

- `spawn_ready_sub_agents` 传入 `index_manager`
- `spawn_ready_sub_agents` 根据 `tools` 参数条件性传入 `memory_handle`
- 未授权的工具从工具列表中过滤

**验证**：授权了 `index_search` 的 Sub Agent 可正常搜索代码；授权了 `recall_memory` 的 Sub Agent 可查询记忆。

### Phase 4：团队感知

- `execute_create_agent` 末尾向现有 Agent 广播新成员通知
- `format_team_roster_from_registry` 生成成员列表

**验证**：新 Agent 加入后，已有 Agent 收到通知并知道新成员的存在。

### Phase 5：会话持久化

- child_session 持久化到磁盘
- 恢复时从磁盘加载

**验证**：重启应用后，Sub Agent 保留之前的对话历史。

---

## 10. 约束（不变）

| 约束 | 值 | 说明 |
|------|-----|------|
| 最大 Agent 数量 | 8 | 含 Main Agent |
| Sub Agent 最大轮次 | 10 | 单次执行 |
| Sub Agent Token 预算 | 200,000 | 所有 Sub Agent 累计 |
| 并发上限 | 4 | 同时运行的 Sub Agent |
| 文件锁超时 | 300 秒 | 自动释放 |
