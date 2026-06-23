# 12 - 会话切换恢复与防抖持久化

## 目标

将统一 Tab 列表绑定到当前会话，切换会话时恢复对应 Tab 集合。

## 范围

- `frontend/src/components/TabsContainer.tsx`
- `frontend/src/store/useStore.ts`
- `frontend/src/api/tauri.ts`

## 任务

- 监听 active session 变化。
- 切换会话时调用 `get_session_tabs`。
- 按 Tab 类型恢复后端运行实例：
  - terminal 调 `terminal_tab_restore`
  - browser 调 `browser_switch_session`
- Tab 列表变化后防抖调用 `set_session_tabs`。
- hydrate 期间不得用空列表覆盖已有会话数据。
- 草稿会话转正时处理临时终端 id 迁移。

## 不做

- 不实现新的 Tab UI。
- 不改后端数据模型。

## 验收

- 会话 A 和 B 各自拥有独立 Tab 列表。
- 切换会话不会丢失旧会话 Tab。
- 重启应用后能恢复会话 Tab 元数据。

## 验证

- `yarn build`
