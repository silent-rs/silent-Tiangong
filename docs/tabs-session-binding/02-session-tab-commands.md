# 02 - 会话 Tab 读写命令

## 目标

提供前端读写当前会话统一 Tab 元数据的后端命令。

## 范围

- `src-tauri/src/commands.rs`
- `src-tauri/src/main.rs`
- `frontend/src/api/tauri.ts` 仅增加 API 封装

## 任务

- 新增 `get_session_tabs(session_id)`。
- 新增 `set_session_tabs(session_id, tabs, active_tab_id)`。
- 命令只操作 `Session.tabs` 和 `Session.active_tab_id`。
- `set_session_tabs` 反序列化失败时返回错误或保留原值，不得写入空数据覆盖会话。
- 调用写入后持久化会话文件。
- 前端 API 增加类型定义。

## 不做

- 不创建 PTY。
- 不创建 WebView。
- 不实现 Tabs UI。

## 验收

- 写入一组 browser/terminal 混合 Tab 后能读回。
- 非法 tabs payload 不会清空原会话 Tab。
- 会话不存在时返回明确错误。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
- `yarn build`
