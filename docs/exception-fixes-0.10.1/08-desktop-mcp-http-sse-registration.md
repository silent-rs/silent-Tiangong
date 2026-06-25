# 08 - 桌面端 MCP HTTP/SSE 注册异常修复

## 目标

修复桌面端设置页只能注册 `npx`/stdio 类型 MCP，无法注册 HTTP/SSE 协议 MCP 的异常，使 Desktop UI 能完整配置并注册远程 MCP server。

## 背景

0.10.0 发布后发现桌面端 MCP 注册能力存在协议覆盖缺口：

- 当前前端添加 MCP 服务器表单只暴露 `command`、`args`、`env`。
- `registerMcpServer` 前端 API 也只传 `name`、`command`、`args`、`env`。
- Tauri 命令 `register_mcp_server` 侧同样以 stdio/npx 模型为主。
- Core 配置模型实际上已经支持：
  - `transport: Auto/Stdio/Http`
  - `endpoint`
  - `auth_header`
  - `headers`
  - HTTP endpoint 自动识别
- 因此前后端能力不一致，导致 Desktop UI 无法正常注册 HTTP/SSE MCP。

## 范围

- `frontend/src/api/tauri.ts`
- `frontend/src/components/SettingsDialog.tsx` 中 MCP 设置组件
- `src-tauri/src/commands.rs` 中 `register_mcp_server` 命令参数和映射
- `src-tauri/src/view.rs` 中 MCP 视图结构，如当前视图未暴露 transport/endpoint/headers
- `crates/tiangong-core/src/app_state/` 中 MCP 注册请求结构，如需要扩展
- `crates/tiangong-core/src/agent_config.rs` 与 `crates/tiangong-core/src/mcp/config.rs` 的校验逻辑，如需补充 SSE 兼容说明
- Tauri 权限声明，如命令签名变化需要更新

## 依赖

- 前置任务：01
- 后续任务：可作为 03、06 的真实工具失败场景输入
- 可并行任务：02、03
- 阻塞说明：01 未完成前，无法确认本修复进入 0.10.1 异常修复范围。

## 任务

- 梳理当前 MCP 注册链路：前端表单 → `api.registerMcpServer` → Tauri `register_mcp_server` → Core `register_mcp_server` → `mcp.json`。
- 明确 Desktop UI 支持的 MCP transport 类型：
  - `stdio`：命令 + 参数 + env + cwd。
  - `http`/`sse`：endpoint + headers/auth，不允许 args/env/cwd。
  - `auto`：兼容旧数据，按 command URL 或 endpoint 推断。
- 前端添加服务器对话框增加 transport 选择。
- 当前选择 stdio 时展示命令、参数、环境变量字段。
- 当前选择 http/sse 时展示 endpoint、认证 header、自定义 headers 字段。
- 前端 API 扩展注册参数，避免继续只传 command/args/env。
- Tauri 命令扩展参数并映射到 Core 注册请求。
- 确保 `McpServerView` 返回 transport、endpoint、auth/header 等必要展示字段。
- 注册后列表中展示 transport 和 endpoint/command，便于用户确认实际注册类型。
- 对 HTTP/SSE 注册失败显示后端返回的具体错误，不只显示“请检查配置”。
- 补充回归验证：stdio/npx 仍可注册，HTTP/SSE endpoint 可注册并进入健康检查。

## 不做

- 不更换 MCP client 库。
- 不重写 MCP 调用执行链路。
- 不新增 OAuth 登录流程。
- 不实现复杂 header 模板或密钥管理 UI。
- 不改变已有 stdio/npx MCP 配置兼容性。
- 不把远程 MCP 的连通性问题误判为注册失败；注册成功后健康检查失败应单独展示。

## 验收

- Desktop 设置页可以选择 MCP transport。
- stdio/npx 类型 MCP 仍能按旧方式注册。
- HTTP/SSE MCP 可以通过 endpoint 注册，写入配置后 `transport`/`endpoint` 信息正确。
- HTTP/SSE MCP 不再被前端强制要求填写 command。
- HTTP/SSE MCP 不会被 Tauri 命令丢弃 endpoint/header 信息。
- 列表中能看出 server 是 stdio 还是 http/sse。
- 配置非法时提示具体原因，例如 endpoint 为空、endpoint 非 http/https、http 模式不支持 args/env/cwd。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
- `yarn --cwd frontend build`
- 手动验证：
  - 注册一个 stdio/npx MCP，确认兼容旧流程。
  - 注册一个 HTTP/SSE MCP endpoint，确认不要求 command。
  - 查看 MCP 列表，确认 transport 和 endpoint 展示正确。
  - 故意输入非法 endpoint，确认错误提示具体。
