# Phase 1 验证清单：基础框架

> 基于 RFC 0011 Phase 1 定义，验证目标：**主 Agent 可创建 Sub Agent，前端显示 Agent Tab，Sub Agent 可执行基本工具调用。**

---

## 一、AgentDescriptor / AgentRegistry 数据结构

### 1.1 AgentDescriptor 字段完整性

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 1.1.1 | `agent_id` 在会话内全局唯一 | 创建多个 Agent 后检查注册表 | 每个 Agent 的 `agent_id` 互不相同 |
| 1.1.2 | `role` 字段正确存储且可用于路由 | 创建 role 为 `"dev"` 的 Agent，通过 `send_message(to="dev", ...)` 发消息 | 消息正确路由到该 Agent |
| 1.1.3 | `label` 字段正确存储，用于前端显示 | 创建 label 为 `"Developer"` 的 Agent | 前端 Tab 显示 "Developer" |
| 1.1.4 | `system_prompt` 正确注入到 Sub Agent 的 ReactEngine | 创建 Agent 时指定 system_prompt，让 Agent 自述身份 | Agent 回复中体现 system_prompt 的内容 |
| 1.1.5 | `lifecycle` 字段正确区分 Persistent / Temporary | 分别创建两种生命周期的 Agent | 注册表中 `lifecycle` 字段值正确 |
| 1.1.6 | `tools` 字段正确限制 Sub Agent 可用工具集 | 创建 Agent 时 tools 仅包含 `["read_file"]`，让 Agent 尝试 `write_file` | Agent 无法调用 `write_file`，收到工具不可用的提示 |
| 1.1.7 | `status` 字段初始值为 `Idle` | 创建 Agent 后立即查询状态 | status == Idle |

### 1.2 AgentRegistry 注册表

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 1.2.1 | 注册表以 HashMap 结构存储 Agent | 检查数据结构实现 | `agents: HashMap<String, AgentDescriptor>` |
| 1.2.2 | 每个 Sub Agent 拥有独立的 ReactEngine 实例 | 创建 Agent 后检查 engines HashMap | engines 中存在对应的 ReactEngine 实例 |
| 1.2.3 | 每个 Sub Agent 拥有独立的 Session（继承父 Session 的 cwd） | 创建 Agent 后检查 sessions HashMap | sessions 中存在独立 Session，cwd 与父 Session 一致 |
| 1.2.4 | 所有 Agent 共享同一个 RuntimeEngine | 检查多个 Agent 的 RuntimeEngine 引用 | 指向同一个 RuntimeEngine 实例（Arc clone） |

---

## 二、create_agent 工具

### 2.1 正常流程

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 2.1.1 | 主 Agent 调用 `create_agent` 成功创建 Sub Agent | 主 Agent 调用 `create_agent(role="dev", label="Developer", system_prompt="...", lifecycle="persistent")` | 返回成功，包含 agent_id |
| 2.1.2 | 创建后注册表中存在该 Agent | 创建后查询注册表 | agents 中包含 role="dev" 的条目 |
| 2.1.3 | 创建后触发 `AgentCreated` StreamEvent | 监听事件流 | 收到 `AgentCreated { agent_id, role, label, lifecycle }` 事件 |
| 2.1.4 | 创建后 Agent 状态为 Idle | 查询 Agent 状态 | status == Idle |
| 2.1.5 | 创建后触发 `AgentStatusChanged` 事件 | 监听事件流 | 收到 `AgentStatusChanged { status: "idle" }` 事件 |
| 2.1.6 | 可同时创建多个不同角色的 Agent | 连续创建 dev、test、pm 三个 Agent | 三个 Agent 均创建成功，注册表包含 3 条记录 |
| 2.1.7 | 创建时 tools 参数为空则继承主 Agent 全部工具 | `create_agent(..., tools=[])` 或不传 tools | Sub Agent 可使用所有主 Agent 工具 |
| 2.1.8 | 创建时指定 tools 子集，Sub Agent 仅可使用指定工具 | `create_agent(..., tools=["read_file", "search_code"])` | Sub Agent 只能调用这两个工具 |

### 2.2 异常场景

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 2.2.1 | role 重复时拒绝创建 | 先创建 role="dev"，再创建 role="dev" | 第二次创建失败，返回错误提示 "Agent with role 'dev' already exists" |
| 2.2.2 | role 为空字符串时拒绝创建 | `create_agent(role="", ...)` | 创建失败，返回参数校验错误 |
| 2.2.3 | system_prompt 为空时拒绝创建 | `create_agent(system_prompt="", ...)` | 创建失败，返回参数校验错误 |
| 2.2.4 | lifecycle 值非法时拒绝创建 | `create_agent(lifecycle="invalid", ...)` | 创建失败，返回参数校验错误 |
| 2.2.5 | tools 列表中包含不存在的工具名时拒绝创建 | `create_agent(tools=["read_file", "nonexistent_tool"], ...)` | 创建失败，返回工具不存在的错误 |
| 2.2.6 | 达到最大 Agent 数量上限时拒绝创建 | 已创建 7 个 Sub Agent（加主 Agent 共 8 个），再创建第 8 个 | 创建失败，返回 "Maximum agent count (8) reached" |
| 2.2.7 | 非主 Agent 调用 create_agent 时拒绝 | Sub Agent 尝试调用 `create_agent` | 调用失败，返回权限不足的错误 |

---

## 三、dismiss_agent 工具

### 3.1 正常流程

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 3.1.1 | 主 Agent 调用 `dismiss_agent` 成功销毁指定 Agent | 创建 Agent 后调用 `dismiss_agent(role="dev")` | 返回成功 |
| 3.1.2 | 销毁后注册表中移除该 Agent | 销毁后查询注册表 | agents 中不再包含该 Agent |
| 3.1.3 | 销毁后 Agent 状态变为 Terminated | 销毁后查询状态 | status == Terminated |
| 3.1.4 | 销毁后触发 `AgentStatusChanged` 事件 | 监听事件流 | 收到 `AgentStatusChanged { status: "terminated" }` 事件 |
| 3.1.5 | 销毁后对应的 ReactEngine 实例被释放 | 销毁后检查 engines HashMap | engines 中不再包含该 Agent 的实例 |
| 3.1.6 | 销毁后对应的 Session 被清理 | 销毁后检查 sessions HashMap | sessions 中不再包含该 Agent 的 Session |
| 3.1.7 | 可连续销毁多个 Agent | 创建 3 个 Agent 后依次销毁 | 3 个 Agent 均被成功销毁 |
| 3.1.8 | 销毁正在执行任务的 Agent | Agent 正在 Running 状态时调用 dismiss | Agent 被中断并销毁，已执行的操作不受影响 |

### 3.2 异常场景

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 3.2.1 | 销毁不存在的 Agent | `dismiss_agent(role="nonexistent")` | 返回错误 "Agent with role 'nonexistent' not found" |
| 3.2.2 | 销毁已销毁的 Agent | 对同一 Agent 调用两次 `dismiss_agent` | 第二次返回错误 "Agent already terminated" |
| 3.2.3 | 销毁主 Agent | `dismiss_agent(role="main")` | 返回错误 "Cannot dismiss main agent" |
| 3.2.4 | 非主 Agent 调用 dismiss_agent | Sub Agent 尝试调用 `dismiss_agent` | 调用失败，返回权限不足的错误 |
| 3.2.5 | 销毁后向已销毁 Agent 发送消息 | 销毁 Agent 后调用 `send_message(to="dev", ...)` | 返回错误 "Agent 'dev' is not active" |

---

## 四、Sub Agent ReactEngine 启动与停止

### 4.1 正常流程

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 4.1.1 | Sub Agent 创建后 ReactEngine 自动启动 | 创建 Agent 后检查 | ReactEngine 进入等待消息的循环 |
| 4.1.2 | Sub Agent 收到消息后开始执行 | 向 Sub Agent 发送消息 "请读取 README.md" | Agent 状态变为 Running，执行 read_file 工具 |
| 4.1.3 | Sub Agent 执行完成后状态恢复 Idle | Agent 完成任务后查询状态 | status == Idle |
| 4.1.4 | Sub Agent 可正确使用被授权的工具 | 创建 tools=["read_file", "search_code"] 的 Agent，发送读取文件任务 | Agent 成功调用 read_file 并返回结果 |
| 4.1.5 | Sub Agent 的 system_prompt 生效 | 创建 system_prompt 为 "你是一个测试工程师" 的 Agent，发送消息 | Agent 回复体现测试工程师的角色定位 |
| 4.1.6 | Sub Agent 使用独立的 Session | 检查 Sub Agent 的 Session | Session ID 与主 Agent 不同，cwd 继承自主 Agent |
| 4.1.7 | Sub Agent 执行过程中触发 WorkerChunk 事件 | 监听事件流 | 收到 `WorkerChunk { worker_id: agent_id, ... }` 事件 |
| 4.1.8 | Sub Agent 执行完成后触发 WorkerCompleted 事件 | 监听事件流 | 收到 `WorkerCompleted { worker_id: agent_id, success: true }` 事件 |
| 4.1.9 | 多个 Sub Agent 可并行执行 | 同时向 2 个 Agent 发送独立任务 | 两个 Agent 同时处于 Running 状态，各自独立完成 |

### 4.2 异常场景

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 4.2.1 | Sub Agent 工具调用失败时优雅处理 | 让 Agent 执行 `read_file` 读取不存在的文件 | Agent 收到错误信息，可自行决定下一步（重试或报告），不会崩溃 |
| 4.2.2 | Sub Agent 达到最大轮次限制时自动停止 | 让 Agent 执行需要超过 10 轮的任务 | Agent 在第 10 轮后被强制停止，状态变为 Idle |
| 4.2.3 | Sub Agent 超过 300 秒超时时自动停止 | 让 Agent 执行长时间任务（或模拟超时） | Agent 在 300 秒后被强制停止 |
| 4.2.4 | Sub Agent 尝试调用未授权的工具 | Agent 尝试调用不在 tools 列表中的工具 | 工具调用被拒绝，Agent 收到 "Tool not available" 的提示 |
| 4.2.5 | Sub Agent 的 LLM 请求失败时重试 | 模拟 LLM API 返回错误 | Agent 按重试策略重试，最终失败则报告错误 |
| 4.2.6 | 并发达到上限（4 个）时新任务排队 | 4 个 Agent 同时运行，向第 5 个发送任务 | 第 5 个 Agent 等待，直到有 Agent 完成后才开始执行 |

---

## 五、基本 StreamEvent

### 5.1 AgentCreated 事件

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 5.1.1 | 创建 Agent 时触发 AgentCreated 事件 | 创建 Agent 并监听事件流 | 收到 AgentCreated 事件 |
| 5.1.2 | 事件包含正确的 agent_id | 检查事件字段 | agent_id 与注册表中一致 |
| 5.1.3 | 事件包含正确的 role | 检查事件字段 | role 与创建时指定的一致 |
| 5.1.4 | 事件包含正确的 label | 检查事件字段 | label 与创建时指定的一致 |
| 5.1.5 | 事件包含正确的 lifecycle | 检查事件字段 | lifecycle 为 "persistent" 或 "temporary" |

### 5.2 AgentStatusChanged 事件

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 5.2.1 | Agent 状态变更时触发事件 | 创建 Agent（Idle）、发送任务（Running）、完成（Idle） | 每次状态变更都触发事件 |
| 5.2.2 | 事件包含正确的 agent_id | 检查事件字段 | agent_id 与注册表中一致 |
| 5.2.3 | 事件包含正确的 label | 检查事件字段 | label 与创建时指定的一致 |
| 5.2.4 | 事件包含正确的 status 值 | 检查各阶段事件 | status 依次为 "idle" → "running" → "idle" |
| 5.2.5 | 销毁 Agent 时触发 terminated 状态事件 | 销毁 Agent 并监听事件流 | 收到 `AgentStatusChanged { status: "terminated" }` |

### 5.3 复用 Worker* 事件

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 5.3.1 | Sub Agent 开始执行时触发 WorkerStarted | 向 Agent 发送任务 | 收到 `WorkerStarted { worker_id: agent_id, worker_label: label }` |
| 5.3.2 | Sub Agent 输出内容时触发 WorkerChunk | Agent 执行中产生输出 | 收到 `WorkerChunk { worker_id: agent_id, ... }` |
| 5.3.3 | Sub Agent 完成时触发 WorkerCompleted | Agent 任务完成 | 收到 `WorkerCompleted { worker_id: agent_id, success: true/false }` |
| 5.3.4 | worker_id 语义正确映射为 agent_id | 检查所有 Worker* 事件 | worker_id 值等于对应 Agent 的 agent_id |

---

## 六、边界条件与约束

### 6.1 数量限制

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 6.1.1 | 最大 Agent 数量为 8（含主 Agent） | 尝试创建 8 个 Sub Agent | 第 7 个 Sub Agent 创建成功，第 8 个失败 |
| 6.1.2 | 并发运行上限为 4 个 Sub Agent | 同时运行 4 个 Agent，再触发第 5 个 | 第 5 个排队等待 |
| 6.1.3 | Sub Agent 最大轮次为 10 | 让 Agent 执行需要多轮的任务 | 第 10 轮后强制停止 |

### 6.2 重复操作

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 6.2.1 | 重复创建相同 role 被拒绝 | 连续两次 `create_agent(role="dev", ...)` | 第二次失败 |
| 6.2.2 | 重复销毁同一 Agent 被拒绝 | 连续两次 `dismiss_agent(role="dev")` | 第二次失败 |
| 6.2.3 | 销毁后可用相同 role 重新创建 | 销毁 dev 后重新创建 dev | 创建成功，获得新的 agent_id |

### 6.3 销毁后操作

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 6.3.1 | 销毁后发送消息给该 Agent 失败 | 销毁 Agent 后 `send_message(to="dev", ...)` | 返回 Agent 不活跃的错误 |
| 6.3.2 | 销毁后广播消息不包含该 Agent | 销毁一个 Agent 后 `broadcast_message(...)` | 已销毁 Agent 不在广播范围内 |
| 6.3.3 | 销毁后该 Agent 持有的文件锁被释放 | Agent 持有文件锁时被销毁 | 文件锁自动释放，其他 Agent 可获取 |

### 6.4 资源清理

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 6.4.1 | 销毁 Agent 后内存正确释放 | 创建并销毁多个 Agent，监控内存 | 无内存泄漏 |
| 6.4.2 | 会话结束时所有 Agent 被清理 | 结束会话 | 所有 Sub Agent 被销毁，资源释放 |
| 6.4.3 | Sub Agent 的 tokio task 被正确取消 | 销毁 Agent | 对应的 tokio::task 被 abort |

---

## 七、权限与安全

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 7.1 | Sub Agent 继承主 Agent 的 TrustMode | 主 Agent TrustMode=AutoApprove，创建 Sub Agent | Sub Agent 工具调用无需额外审批 |
| 7.2 | Sub Agent 工具权限受 tools 列表限制 | 创建 tools=["read_file"] 的 Agent | Agent 只能调用 read_file |
| 7.3 | Sub Agent 不能调用 create_agent | Sub Agent 尝试 create_agent | 调用失败 |
| 7.4 | Sub Agent 不能调用 dismiss_agent | Sub Agent 尝试 dismiss_agent | 调用失败 |
| 7.5 | Sub Agent 只能访问允许写入目录下的文件 | Sub Agent 尝试写入允许目录外的文件 | 写入被拒绝 |

---

## 八、前端显示（Agent Tab）

| # | 验收标准 | 验证步骤 | 预期结果 |
|---|---------|---------|---------|
| 8.1 | 创建 Agent 后前端 Tab 栏出现新 Tab | 创建 Agent | Tab 栏显示 Agent label |
| 8.2 | Tab 显示 Agent 状态指示器 | Agent 处于不同状态 | Tab 上显示空闲/运行中等状态 |
| 8.3 | 点击 Tab 可切换到对应 Agent 视图 | 点击 Agent Tab | 显示该 Agent 的执行输出 |
| 8.4 | Agent 销毁后 Tab 变灰但仍可查看历史 | 销毁 Agent | Tab 变灰，历史内容仍可查看 |
| 8.5 | Main Tab 为默认视图 | 打开会话 | 默认显示 Main Tab |
| 8.6 | Agent 事件正确路由到对应 Tab | Agent 产生输出 | 输出显示在对应 Agent Tab 下 |

---

## 九、集成验证场景

### 场景 A：基本创建-执行-销毁流程

```
1. 主 Agent 调用 create_agent(role="dev", label="Developer", system_prompt="你是 Rust 开发者", lifecycle="persistent", tools=["read_file","write_file","run_command"])
2. 验证：注册表包含 dev，前端出现 Developer Tab，触发 AgentCreated 事件
3. 主 Agent 调用 send_message(to="dev", content="请读取 Cargo.toml 文件")
4. 验证：dev Agent 状态变为 Running，执行 read_file，完成后恢复 Idle
5. 主 Agent 调用 dismiss_agent(role="dev")
6. 验证：注册表移除 dev，前端 Tab 变灰，触发 AgentStatusChanged(terminated) 事件
```

### 场景 B：多 Agent 并行执行

```
1. 创建 dev、test 两个 Agent
2. 同时向两个 Agent 发送独立任务
3. 验证：两个 Agent 同时 Running，各自独立完成
4. 事件流中包含两个 Agent 的 WorkerStarted/WorkerChunk/WorkerCompleted 事件
5. 两个 Agent 的输出互不干扰
```

### 场景 C：达到上限后创建

```
1. 连续创建 7 个 Sub Agent（达到上限 8）
2. 尝试创建第 8 个 Sub Agent
3. 验证：第 8 个创建失败，返回数量上限错误
4. 销毁其中一个
5. 再次创建
6. 验证：创建成功
```

### 场景 D：工具权限隔离

```
1. 创建 Agent A，tools=["read_file"]
2. 创建 Agent B，tools=["read_file", "write_file"]
3. 向 A 发送任务：读取并修改文件
4. 验证：A 可以读取，但无法写入
5. 向 B 发送相同任务
6. 验证：B 可以读取和写入
```

---

## 十、自动化测试建议

### 单元测试

| 测试项 | 描述 |
|--------|------|
| `test_agent_descriptor_creation` | 验证 AgentDescriptor 各字段正确初始化 |
| `test_registry_insert_and_get` | 验证注册表的增删查操作 |
| `test_registry_duplicate_role_rejected` | 验证重复 role 被拒绝 |
| `test_registry_max_capacity` | 验证达到上限后拒绝创建 |
| `test_agent_status_transitions` | 验证状态转换逻辑 Idle→Running→Idle |
| `test_agent_status_terminated` | 验证销毁后状态为 Terminated |

### 集成测试

| 测试项 | 描述 |
|--------|------|
| `test_create_agent_tool_execution` | 验证 create_agent 工具端到端执行 |
| `test_dismiss_agent_tool_execution` | 验证 dismiss_agent 工具端到端执行 |
| `test_sub_agent_tool_call` | 验证 Sub Agent 可调用授权工具 |
| `test_sub_agent_tool_restriction` | 验证 Sub Agent 无法调用未授权工具 |
| `test_sub_agent_parallel_execution` | 验证多 Agent 并行执行 |
| `test_stream_events_on_create` | 验证创建时触发正确的事件序列 |
| `test_stream_events_on_status_change` | 验证状态变更时触发正确的事件 |
| `test_dismiss_cleans_up_resources` | 验证销毁后资源完全清理 |
| `test_recreate_after_dismiss` | 验证销毁后可用相同 role 重新创建 |

---

## 验证结果记录

| 功能模块 | 通过/失败 | 备注 |
|---------|----------|------|
| 1. AgentDescriptor / AgentRegistry | ⬜ 未验证 | |
| 2. create_agent 工具 | ⬜ 未验证 | |
| 3. dismiss_agent 工具 | ⬜ 未验证 | |
| 4. Sub Agent ReactEngine | ⬜ 未验证 | |
| 5. StreamEvent | ⬜ 未验证 | |
| 6. 边界条件与约束 | ⬜ 未验证 | |
| 7. 权限与安全 | ⬜ 未验证 | |
| 8. 前端 Agent Tab | ⬜ 未验证 | |
| 9. 集成验证场景 | ⬜ 未验证 | |
