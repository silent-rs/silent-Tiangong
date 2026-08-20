# 审批与用户征询插件化方案

## 1. 结论

`plugins/tiangong-plugin-interaction` 是仅供 Desktop 使用的纯 TypeScript 插件，不包含 WASM，也不扩展 `plugin.wit`。

- 插件清单声明 `request_user` 工具规格和提示词；
- 插件 TypeScript 代码接收工具调用，负责六类参数语义、操作框和 Tool Result；
- 插件独立处理 15 秒用户时限，并把用户意见作为普通 Tool Result 返回；
- 公共运行时只提供业务无关的 TS 工具调用转发、等待、通用兜底和结果校验；
- Core 不识别 `request_user`、征询类型或审批结果，也不保存挑战和授权。

```text
Agent 调用 request_user
-> 公共运行时按清单找到 TS 工具插件
-> 宿主发布 tool.requested
-> 交互插件 TypeScript 解析参数并渲染操作框
-> 插件调用 tool.resolve 提交完整 Tool Result
-> 公共运行时闭合调用并返回现有工具流水线
-> Agent Loop 继续
```

## 2. 运行范围

纯 TS 工具插件仅支持 `entrypoints: ["desktop"]`。CLI 与 Server 不加载该工具，也不提供内置替代实现；以后如需支持无界面入口，应单独设计对应逻辑运行时。

现有 WASM 插件继续使用 `plugin.wit`，本功能不改变 WIT 契约和既有二进制兼容性。

## 3. 归属边界

### 交互插件

- `request_user` 工具规格与系统提示；
- `approval`、`confirm`、`choice`、`multi_choice`、`input`、`form` 参数解析；
- 标题、描述、候选项、字段和问题的展示编排；
- answered、expired、cancelled 的工具结果结构与文案；
- 操作框、倒计时、提交锁、重复提交保护和闭合状态。
- 审批结果的表达；该结果只是供 Agent 判断下一步的用户意见。

### 公共插件运行时

- 从已启用的 Desktop 插件清单注册 TS 工具规格与提示词；
- 生成调用 ID、记录权威会话和工具调用归属；
- 发布 `tool.requested`，等待 `tool.resolve`，并限制所有 TS 工具共有的最大执行时间；
- 工具调用到达时若无插件 UI 接应（无存活 `tool.requested` 订阅者），经 `app.open`
  桥接原语以**后台模式**（`mode=background`）隐性挂载插件实例（通用能力，官方与
  三方插件一致；插件需有 `extension.tab` 贡献）：不弹拓展区面板、不打扰用户，
  Agent 操作的页面照常在插件作用域进行，用户可随时自行打开拓展区观察（协同）。
  实例挂载完成订阅后由重放机制继续执行本次调用。同一插件短时间内重复请求会被
  冷却窗口抑制；后台会话的工具调用同样可挂载；
- `app.close` 原语关闭插件 App 实例，关闭目标必须显式声明：`instance_id`
  精确关闭一个实例，`all: true` 关闭该插件全部实例（如收起整个浏览器面板）；
  两者都缺省时拒绝，杜绝误全关。`app.open` 可携带调用方生成的 `instance_id`
  （幂等重开同一实例，并作为后续精确关闭的锚点）。执行类工具的实例由插件
  自行选择或创建，Agent 不传实例编号；插件在结果中告知实际编号，后续定向
  输入或关闭指定终端时必须引用该编号。插件声明 `app.use` 权限后可经
  `bridge.call` 主动打开/关闭自己的 App；宿主内部使用不经权限校验；
- 原子接受一次结果，拒绝错插件、迟到和重复提交；
- 调用早于插件订阅时保存待处理项，并在订阅建立后重放；
- 校验插件提交的 Tool Result 大小和基本结构。

### Core

- 将 `request_user` 与其他插件工具一样接入现有工具调用与结果配对规则；
- 把插件返回的普通 Tool Result 交给 Agent；
- 不解释审批结果，不决定是否放行后续工具，也不维护征询计时、挑战或授权状态。

Core 与公共运行时不得按 `request_user` 工具名分支，也不得维护六类业务枚举、参数规则或结果文案。

## 4. 插件声明

```json
{
  "schema_version": 2,
  "id": "interaction",
  "entrypoints": ["desktop"],
  "permissions": ["tool.provide"],
  "capabilities": {
    "tools": true,
    "prompt": true,
    "interaction": true,
    "events": ["tool.*"]
  },
  "tools": [{
    "name": "request_user",
    "description": "向用户发起限时征询",
    "input_schema": { "type": "object" },
    "timeout_ms": 20000
  }],
  "prompt": ["需要用户决策或补充信息时调用 request_user。"],
  "ui": {
    "sandbox": "iframe",
    "contributions": [{
      "slot": "session.interaction",
      "id": "interaction",
      "entry": "dist/index.html",
      "context": ["session"]
    }]
  }
}
```

`tool.provide` 允许插件接收并闭合自己声明的工具调用。交互处理器没有审批专用权限或宿主特权。

## 5. TS 工具协议

### `tool.requested`

事件包含：

- `invocation_id`：本次 TS 工具执行的权威 ID；
- `session_id` / `tool_call_id`：宿主记录的归属；
- `name` / `arguments`：不透明工具调用；
- `created_at`：调用创建时间。插件以此独立计算 15 秒用户时限。

### `tool.closed`

事件包含 `invocation_id` 与 `answered | expired | cancelled`。

### `tool.resolve`

插件提交完整通用工具结果：

```json
{
  "invocation_id": "invocation-id",
  "status": "answered",
  "result": {
    "ok": true,
    "summary": "{\"status\":\"answered\",\"result\":true}",
    "stdout": "",
    "stderr": "",
    "exit_code": 0
  }
}
```

`status` 为 `answered | expired | cancelled`，省略时默认 `answered`。
审批类结果与其他结果使用同一结构，用户选择位于 `result.summary` 的业务 JSON 中，由 Agent 读取。

## 6. 时限和故障

- 插件按 `created_at + 15 秒` 独立判断用户时限，并生成自己的 expired Tool Result；
- 宿主保留更长的通用执行上限，插件崩溃、未挂载或未响应时返回通用工具失败；
- 调用在插件订阅前产生时，订阅建立后重放，不能静默丢失；
- 插件卸载、禁用、页面销毁或 turn 取消时，待处理调用取消并释放等待者；
- 同一 `invocation_id` 只有第一次合法提交生效。

## 7. 安全边界

插件只能提交自己收到的调用结果，不能伪造权威会话、延长宿主通用上限或为一个调用生成多个结果。

审批结果不具备宿主授权能力。它只是用户意见，由 Agent 决定是否发起后续工具调用；Core 和公共运行时不读取其中的决定字段。

## 8. 非目标

- 不为 CLI 或 Server 提供 TS 工具运行环境；
- 不把 TypeScript 编译为 WASM Component；
- 不修改 `plugin.wit`；
- 不引入常驻 Driver、Agent Inbox 或新的 Agent Loop；
- 不在 Core 或公共运行时中建立审批挑战、授权表或专用结果通道。
