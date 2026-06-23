# 统一工作区 Tabs 任务索引

> 总设计：`../tabs-session-binding.md`

任务按可单独开发、单独验证的粒度拆分。每个任务原则上独立提交。

## 任务顺序

1. `01-session-tab-model.md`：会话 Tab 数据模型
2. `02-session-tab-commands.md`：会话 Tab 读写命令
3. `03-terminal-registry-multitab.md`：终端多 Tab 注册表
4. `04-terminal-selection.md`：终端空闲选择与繁忙新建
5. `05-terminal-result-feedback.md`：命令结果反馈终端选择信息
6. `06-browser-session-switch.md`：浏览器会话切换
7. `07-browser-blank-lazy-webview.md`：浏览器空白页懒创建
8. `08-frontend-workspace-shell.md`：前端单一工作区面板
9. `09-frontend-tabs-container.md`：统一 Tabs 容器
10. `10-terminal-tab-content.md`：终端 Tab 内容组件
11. `11-browser-tab-content.md`：浏览器 Tab 内容组件
12. `12-session-restore-persistence.md`：会话切换恢复与防抖持久化
13. `13-permissions-and-api.md`：Tauri API 与权限声明
14. `14-end-to-end-verification.md`：端到端验收

## 拆分原则

- 一个任务只改一个主要边界。
- 代码任务必须包含验证命令。
- 前端任务优先用 `yarn build` 验证。
- Rust 任务优先用 `cargo fmt -- --check` 和 `cargo check --workspace` 验证。
- 端到端任务只做验证和小修，不混入新功能。
