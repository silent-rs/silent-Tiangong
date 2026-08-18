# 审批与用户征询插件化方案

## 一、设计结论

统一采用以下模型：

> **Agent 通过 Tool Call 发起审批、确认、选择或额外输入请求；请求交给交互插件处理；当前 Agent Loop 不等待用户；用户响应或超时作为原 Tool Call 的唯一 Tool Result 写回；随后从最新 Session 继续执行 Agent。**

核心原则：

1. 审批由 Agent Tool 发起，不由 Core 直接弹窗。
2. 用户响应是 `tool` 消息，不是普通 `user` 消息。
3. Agent Loop 不阻塞等待用户。
4. 每个请求都有明确时限，第一版默认 15 秒。
5. 用户响应、超时、取消只能有一个结果生效。
6. 审批超时必须默认拒绝，不得产生授权。
7. Core 保留最终工具权限判断，插件不能直接执行受保护工具。
8. 不引入常驻 Driver、Agent Inbox、Continuation 或 Future 恢复机制。

---

## 二、统一 Agent Tool

建议提供一个统一工具：

```text
request_user
```

支持以下请求类型：

```text
approval       审批
confirm        是/否确认
choice         单选
multi_choice   多选
input          文本输入
form           表单填写
```

工具定义示例：

```json
{
  "name": "request_user",
  "description": "向用户发起限时审批、确认、选择或输入请求。调用后当前执行暂停，用户响应将作为该工具调用的结果返回。",
  "input_schema": {
    "type": "object",
    "properties": {
      "kind": {
        "type": "string",
        "enum": [
          "approval",
          "confirm",
          "choice",
          "multi_choice",
          "input",
          "form"
        ]
      },
      "title": {
        "type": "string"
      },
      "description": {
        "type": "string"
      },
      "options": {
        "type": "array"
      },
      "fields": {
        "type": "array"
      },
      "approval_challenge": {
        "type": "string"
      }
    },
    "required": ["kind", "title"]
  }
}
```

第一版不建议让 Agent 自定义超时时间，统一由宿主设置为 15 秒，避免模型给出不合理时间。

后续可以增加受限字段：

```json
{
  "timeout_seconds": 30
}
```

并由宿主限制在固定范围内。

---

## 三、请求与响应的模型协议

### 1. Agent 发起请求

例如 Agent 请求删除文件审批：

```text
assistant
  tool_call:
    id: call-request-user-1
    name: request_user
    arguments:
      kind: approval
      title: 是否允许删除临时文件？
      description: 将删除 /tmp/example
      approval_challenge: challenge-123
```

宿主收到该 Tool Call 后：

1. 保存 assistant tool-call 消息；
2. 创建交互请求；
3. 通知内置界面和交互插件；
4. Agent Loop 停止继续请求模型；
5. 暂时不写入 Tool Result；
6. 当前 turn task 结束。

Session 此时允许暂时存在一个未闭合的 `request_user` Tool Call，但在它闭合前不得再次请求模型。

### 2. 用户响应

用户选择“仅本次允许”后，宿主写入：

```text
tool
  tool_call_id: call-request-user-1
  content:
    {
      "status": "answered",
      "kind": "approval",
      "decision": "approve_once",
      "request_id": "request-123"
    }
```

然后从最新 Session 重新启动 Agent。

模型下一次看到：

```text
user: 原始任务
assistant: request_user(...)
tool: 用户选择仅本次允许
```

这比追加一条普通 User Message 更符合请求与回答的真实关系。

---

## 四、为什么不使用 User Message

用户主动发送消息和用户回答 Agent 请求是不同语义。

| 场景 | Session 角色 | 处理方式 |
|---|---|---|
| 用户提出新任务 | `user` | 空闲起轮 |
| 用户运行中改变意图 | `user` | `InjectUserMessage` 引导处理 |
| 用户回答选择题 | `tool` | 闭合 `request_user` Tool Call |
| 用户填写表单 | `tool` | 闭合 `request_user` Tool Call |
| 用户批准或拒绝 | `tool` | 闭合 Tool Call，并更新可信授权 |
| 用户取消请求 | `tool` | 写入 cancelled 结果 |
| 请求超时 | `tool` | 写入 expired 结果 |

交互响应可以复用现有“活动时投通道、空闲时起任务”的**路由方式**，但不能复用 User Message 的消息类型。

---

## 五、Agent Loop 行为

当前工具执行结果需要增加一种语义：

```text
ToolBatchOutcome
├── Completed
├── Interrupted
└── DeferredToUser
```

当 `request_user` 创建请求成功后，返回：

```text
DeferredToUser {
    request_id,
    tool_call_id,
    deadline
}
```

Agent Loop 对该结果的处理：

1. 不向模型发送下一次请求；
2. 不生成假的“请求成功”Tool Result；
3. 不等待用户响应；
4. 将当前用户轮次标记为等待用户；
5. 当前 Agent Loop 和 turn task 正常退出。

不需要保存：

- Rust Future；
- Loop 执行栈；
- `TurnContext`；
- continuation；
- 命令接收器；
- 工具任务句柄。

---

## 六、用户响应后的继续执行

建议增加一个统一入口：

```text
resolve_interaction(request_id, result)
```

处理流程：

```text
交互插件提交响应
→ InteractionRegistry 校验并原子闭合请求
→ 审批响应按需创建可信授权
→ 生成对应 Tool Result
→ 根据当前会话活动状态继续
```

### 当前没有活跃 turn task

```text
写入 Tool Result
→ 从最新 Session 构建 TurnContext
→ 创建新的 turn task
→ Agent 根据 Tool Result 继续
```

### 当前仍有活跃 turn task

用户可能在请求展示后立即点击，而旧 task 仍在收尾。

此时发送语义明确的命令：

```rust
Command::ResolveInteraction {
    request_id,
    tool_call_id,
    result,
}
```

当前 turn：

1. 校验并写入 Tool Result；
2. 切回模型请求阶段；
3. 继续分析。

该命令不能复用 `InjectUserMessage`，因为它不是新用户意图：

- 不追加 User Message；
- 不清空新意图工具历史；
- 不切换用户消息锚点；
- 不发送 UserMessage 确认事件。

因此，复用的是当前 Core 的状态分流：

```text
活动时投递当前 task
空闲时从最新 Session 起 task
```

不是复用 User Message 的具体处理函数。

---

## 七、请求管理器

建议由插件运行时提供独立的 `InteractionRegistry`：

```text
InteractionRegistry
├── create
├── respond
├── expire
├── cancel
├── close
└── query
```

请求结构：

```text
InteractionRequest
├── request_id
├── session_id
├── source_message_id
├── tool_call_id
├── kind
├── title
├── description
├── payload
├── approval_target
├── status
├── created_at
└── deadline
```

状态：

```text
Pending
Answered
Expired
Cancelled
```

合法状态转换：

```text
Pending → Answered
Pending → Expired
Pending → Cancelled
```

请求一旦闭合，不得再次修改。

---

## 八、15 秒超时

第一版统一规则：

```text
DEFAULT_INTERACTION_TIMEOUT = 15 秒
```

请求创建时记录：

```text
created_at
deadline = created_at + 15 秒
```

必须记录绝对截止时间，不能只依赖一个内存计时器。系统休眠或线程延迟后，仍应根据真实截止时间判断是否过期。

### 计时任务

请求管理器创建轻量计时任务：

```text
创建请求
→ 登记 Pending
→ 启动 deadline 定时任务
→ 立即返回 DeferredToUser
```

计时任务只持有：

- `request_id`
- `deadline`
- 请求管理器共享句柄

不能持有：

- `TurnContext`
- Agent Loop；
- 工具执行 Future；
- 插件实例锁；
- Session 可变引用。

---

## 九、超时结果

### 审批超时

审批超时必须 fail-closed：

```json
{
  "status": "expired",
  "kind": "approval",
  "request_id": "request-123",
  "timeout_seconds": 15,
  "message": "用户未在规定时间内响应，操作未获批准"
}
```

宿主行为：

- 不创建任何授权；
- 原受保护工具不能执行；
- 迟到的批准无效；
- 写入 expired Tool Result；
- 启动 Agent，让其决定放弃、解释或重新请求。

### 选择、输入和表单超时

```json
{
  "status": "expired",
  "kind": "choice",
  "request_id": "request-123",
  "timeout_seconds": 15,
  "message": "用户未在规定时间内响应"
}
```

宿主不擅自选择默认值，由 Agent 决定：

- 使用安全默认方案；
- 放弃当前步骤；
- 重新询问；
- 换一种不依赖用户输入的方案。

第一版不支持 `default_on_timeout`，尤其禁止审批超时自动批准。

---

## 十、响应与超时竞态

用户可能恰好在第 15 秒点击，必须由宿主原子闭合。

```text
响应先闭合：
Pending → Answered
超时任务醒来 → AlreadyClosed → 退出

超时先闭合：
Pending → Expired
用户随后点击 → AlreadyExpired → 拒绝
```

只有成功执行：

```text
Pending → 某个终态
```

的一方才能：

- 写入 Tool Result；
- 产生审批授权；
- 启动 Agent；
- 发布 `interaction.closed`。

必须保证同一个 Tool Call 永远只有一个 Tool Result。

---

## 十一、插件职责

交互插件负责：

- 展示审批卡片；
- 展示选项、文本框和表单；
- 显示剩余时间；
- 收集用户响应；
- 提交结构化响应；
- 收到关闭事件后同步禁用界面。

插件不能：

- 直接执行受保护工具；
- 自行追加 Tool Result；
- 自行创建审批授权；
- 直接向 Agent Loop 发送内部命令；
- 将超时解释为批准；
- 重新打开已经闭合的请求。

宿主事件：

```text
interaction.requested
interaction.closed
```

插件提交：

```text
interaction.respond
```

同一请求可以同时展示在：

- 内置桌面界面；
- iframe 或 Shadow 插件；
- 移动端；
- 企业审批插件。

第一个合法响应生效。

---

## 十二、审批安全模型

模型看到：

```text
tool result: approve_once
```

并不等于工具已经获得执行权限。

合法审批响应需要同时产生两项结果：

1. 给 Agent 的 Tool Result；
2. 给工具权限层的可信授权。

### 仅本次允许

```text
ApprovalGrant
├── session_id
├── plugin_id
├── tool_name
├── arguments_hash
├── scope = Once
└── expires_at
```

工具调用完全匹配后执行，并立即消费授权。

### 本次运行内允许

```text
RuntimeGrant
├── session_id
├── plugin_id
├── tool_name
└── scope = Runtime
```

保存在运行时内存中：

- 跨 turn 有效；
- 当前会话隔离；
- 应用重启后失效；
- 不写入 Session 文件。

### 拒绝、取消、超时

不创建任何授权。

---

## 十三、审批挑战

为了避免 Agent 自行伪造批准目标，受保护工具第一次被调用但没有授权时，工具权限层返回结构化挑战：

```json
{
  "status": "approval_required",
  "challenge_id": "challenge-123",
  "plugin_id": "filesystem",
  "tool_name": "delete_file",
  "arguments_hash": "sha256:...",
  "summary": "删除 /tmp/example"
}
```

Agent 随后调用：

```json
{
  "kind": "approval",
  "title": "是否允许删除文件？",
  "approval_challenge": "challenge-123"
}
```

`request_user` 根据 `challenge_id` 从宿主取得真实审批目标，而不是相信 Agent 提交的工具名和参数。

批准后：

```text
生成与 challenge 对应的授权
→ 写入 request_user Tool Result
→ Agent 重新调用原工具
→ 工具权限层校验并消费授权
```

如果 Agent 修改了参数：

```text
arguments_hash 不匹配
→ 原授权不可用
→ 重新生成审批挑战
```

---

## 十四、审批策略

建议区分 Agent 是否发起请求的策略：

```text
ApprovalMode
├── Always
├── AgentDecides
└── Never
```

### `Always`

受保护操作没有授权时：

```text
工具返回 approval_required
→ Agent 必须调用 request_user(approval)
```

如果 Agent 不发起，工具仍然不能执行。

### `AgentDecides`

Agent 根据系统提示和风险自行调用 `request_user`。

宿主不主动要求普通工具审批，但仍执行基本权限和沙箱限制。

### `Never`

Agent 不发起审批。

受保护工具的行为由信任模式决定：

```text
FullTrust  → 按策略直接执行
Restricted → 直接拒绝
```

“从不询问”不能自动解释为“全部批准”。

---

## 十五、Tool Call 批次规则

`request_user` 必须是独占型工具调用。

如果模型一次返回：

```text
request_user + delete_file + run_command
```

宿主不能一边询问用户一边执行其他副作用工具。

建议规则：

1. `request_user` 必须是当前批次唯一 Tool Call；
2. 与其他 Tool Call 同批出现时，其他调用不执行；
3. 为其他调用写入明确的未执行结果，或触发一次协议修复；
4. 系统提示明确要求：需要用户介入时只调用 `request_user`。

第一版建议采用协议修复：

```text
request_user 必须独占一个工具调用批次，请重新生成工具调用
```

这样避免产生多条未闭合 Tool Call。

---

## 十六、普通用户消息到达

如果仍有 Pending 交互，而用户直接发送新消息：

1. 普通消息不能自动视为交互答案；
2. 原请求闭合为 `Cancelled`；
3. 写入对应 cancelled Tool Result；
4. 新消息继续走现有 User Message 路由。

结果示例：

```json
{
  "status": "cancelled",
  "kind": "approval",
  "reason": "用户发送了新的消息"
}
```

必须先闭合悬空的 `request_user` Tool Call，再让新 User Message 进入后续模型请求，避免破坏工具协议。

---

## 十七、对 Core 的最小调整

### 删除

- 工具流水线内部阻塞式审批等待；
- `wait_approval`；
- `wait_interaction`；
- Loop 内 `recv_timeout`；
- `Command::Approval`；
- `Command::Interaction`；
- `AgentInputKind::Approval`；
- `AgentInputKind::Interaction`；
- Loop 内迟到响应判断；
- `Session.approved_tools`。

### 增加

- `request_user` 统一 Tool；
- `ToolBatchOutcome::DeferredToUser`；
- `InteractionRegistry`；
- `Command::ResolveInteraction`；
- 空闲时写入 Tool Result 并启动 turn 的入口；
- 共享运行期授权表；
- 审批挑战与一次性授权。

### 可抽取的公共启动逻辑

当前 `start_user_turn` 可以拆分为：

```text
start_turn_from_latest_session
start_user_turn
resume_with_tool_result
```

其中：

```text
start_user_turn
  = 写入 User Message
  + start_turn_from_latest_session

resume_with_tool_result
  = 写入 Tool Result
  + start_turn_from_latest_session
```

这样不需要增加复杂的恢复系统。

---

## 十八、第一版非目标

第一版不实现：

- Agent 自定义任意超时时间；
- 永久授权；
- 应用重启后恢复审批请求；
- 多人会签；
- 多级企业审批；
- 保存并恢复 Rust Future；
- continuation 序列化；
- 自动恢复原工具执行现场；
- 审批超时自动批准；
- 插件直接执行受保护工具。

应用关闭或重启时，Pending 请求统一闭合或失效；危险操作必须重新发起审批。

---

## 十九、验收场景

至少覆盖：

1. Agent 发起审批，用户批准后结果作为原 Tool Result 写入。
2. Agent 发起选择，用户选择后模型正常继续。
3. Agent 发起文本输入和表单，结果结构正确。
4. Agent Loop 等待用户期间不保留活动等待任务。
5. 15 秒超时自动写入 expired Tool Result。
6. 审批超时不产生授权。
7. 超时后迟到批准无效。
8. 用户响应与超时竞争时只有一个 Tool Result。
9. 同一请求被多个插件响应时只有第一个生效。
10. `approve_once` 只能执行一次。
11. 工具参数变化后原审批失效。
12. `approve_for_runtime` 跨 turn 有效。
13. 应用重启后运行期授权失效。
14. 会话之间不共享授权。
15. 同名工具来自不同插件时不共享授权。
16. 普通用户消息会取消未完成交互并闭合 Tool Call。
17. `request_user` 与其他工具同批时不会执行其他副作用工具。
18. 插件不可用时内置界面仍可响应。
19. iframe 和 Shadow 多实例不会互相退订。
20. Session 中不会留下无法闭合的交互 Tool Call。

## 最终方案一句话

> **Agent 通过独占的 `request_user` Tool Call 发起审批或意见征询；交互插件在默认 15 秒内收集响应；用户响应、取消或超时作为该 Tool Call 唯一的 Tool Result 写回 Session；随后按当前会话活动状态继续或重新启动 Agent；审批结果同时由宿主生成参数绑定的可信授权，Core 不阻塞、不保存执行现场，插件不直接获得工具执行权。**
