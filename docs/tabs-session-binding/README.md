# 统一工作区 Tabs 任务索引

> 总设计：`../tabs-session-binding.md`
>
> 进度记录：`PROGRESS.md`

> 状态说明（2026-07-12）：01/02 中把 Tab 放进 Core Session 的方案已废弃。当前由 Browser/Terminal 插件分别保存自身元数据，Desktop 只保存 `{kind, id}` 跨插件布局引用；这些文件保留为历史任务记录。

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
- 每个任务必须写明前置任务、后续任务、可并行任务和阻塞说明。
- 代码任务必须包含验证命令。
- 前端任务优先用 `yarn build` 验证。
- Rust 任务优先用 `cargo fmt -- --check` 和 `cargo check --workspace` 验证。
- 端到端任务只做验证和小修，不混入新功能。

## 依赖总览

| 任务 | 前置任务 | 主要后续任务 | 可并行任务 |
|------|----------|--------------|------------|
| 01 | 无 | 02、06、09、12 | 无 |
| 02 | 01 | 09、12 | 03、06 |
| 03 | 01 | 04、10、12、13 | 02、06、08 |
| 04 | 03 | 05、10、14 | 06、08 |
| 05 | 04 | 14 | 07、08、09、11 |
| 06 | 01 | 07、11、12、13 | 02、03、04、08 |
| 07 | 06 | 11、12、14 | 05、08、09、10 |
| 08 | 无 | 09、10、11、12 | 02、03、04、06 |
| 09 | 01、02、08 | 10、11、12、14 | 05、07 |
| 10 | 03、04、09、13 | 12、14 | 11 |
| 11 | 06、07、09、13 | 12、14 | 10 |
| 12 | 01、02、03、06、07、09、10、11、13 | 14 | 无 |
| 13 | 02、03、06 | 10、11、12、14 | 04、05、08、09 |
| 14 | 01-13 | 无 | 无 |
