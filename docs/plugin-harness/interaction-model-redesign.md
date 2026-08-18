# 审批与用户征询插件化方案

## 1. 结论

Agent 通过 `request_user` Tool Call 发起审批或征询。当前 Agent Loop 在这个工具调用边界**异步等待 Tool Result**，不会继续请求模型，也不会阻塞 Tokio 工作线程；响应、取消或超时只有一个结果生效。

界面不再由 Core 或固定 React 卡片实现，而由声明 `capabilities.interaction=true` 的交互处理器插件实现：

```text
Agent request_user Tool Call
→ Core 创建受控请求和绝对 deadline
→ 桌面宿主发布 interaction.requested
→ session.interaction 插件渲染
→ 插件调用 interaction.resolve
→ 宿主按 request_id 权威路由并原子闭合
→ 原 Tool Call 获得唯一 Tool Result
→ 原 Agent Loop 继续
```

## 2. 支持类型

- `approval`：仅本次允许、本次运行内允许、拒绝
- `confirm`：是/否
- `choice`：单选
- `multi_choice`：多选
- `input`：文本输入
- `form`：结构化表单

`approval` 必须显式携带原工具返回的 `approval_challenge`。宿主从挑战表获取真实插件、工具和参数摘要；缺失、过期或会话不匹配均拒绝，不自动选择其他挑战。

## 3. 插件声明

```json
{
  "schema_version": 2,
  "permissions": ["interaction.handle"],
  "capabilities": {
    "interaction": true,
    "events": ["interaction.*"]
  },
  "ui": {
    "sandbox": "iframe",
    "contributions": [{
      "slot": "session.interaction",
      "id": "interaction-handler",
      "entry": "app/index.html",
      "context": ["session"]
    }]
  }
}
```

交互处理属于特权能力：Bridge 同时校验 `interaction.handle` 权限和 `capabilities.interaction=true`。

## 4. Bridge 协议

### `interaction.requested`

请求事件包含：

- `request_id`
- `session_id`
- `tool_call_id`
- `kind`
- `title` / `description`
- `payload`
- `created_at`
- `deadline`

### `interaction.closed`

闭合事件包含 `request_id`、`session_id` 和 `answered | expired | cancelled`。

### `interaction.resolve`

```json
{
  "request_id": "request-id",
  "result_json": "{\"decision\":\"approve_once\"}"
}
```

插件不传 session_id。宿主按 request_id 从注册表查询权威会话并投递对应 Core，避免切换会话后错投。

## 5. 时限和竞态

- 后端生成绝对 deadline，并在注册表锁内原子判断 Answered 或 Expired。
- deadline 后的迟到提交不能生成授权。
- 前端插件显示倒计时，到期立即禁用，但不自行制造 Tool Result。
- 提交后进入 submitting，禁用重复操作，并等待 `interaction.closed` 最终状态。
- 无处理器、处理器崩溃或未响应时，Core 在默认 15 秒后安全超时。

## 6. 安全边界

插件只能展示请求和提交候选响应，不能：

- 直接执行受保护工具；
- 生成或修改审批挑战；
- 指定请求所属会话；
- 延长 deadline；
- 直接写授权表；
- 让同一请求产生多个 Tool Result。

审批批准后，Core 只按挑战中的可信目标生成一次性或运行期授权。

## 7. 默认处理器插件

`plugins/interaction-handler` 是 Vue 3 + Vite 工程化插件（`yarn build` 产出自包含单文件），作为默认交互处理器真实使用：覆盖六种请求、倒计时、提交锁和闭合状态。第三方工程化处理器可使用 `plugins/sdk` 的 `createInteractionHandler()` 开发并替换。

## 8. 非目标

- 不引入常驻 Driver、Agent Inbox 或 Continuation。
- 不退出当前 turn 后重新起轮。
- 不持久化 Rust Future 或 TurnContext。
- 不让插件参与 Core 的最终权限裁决。
