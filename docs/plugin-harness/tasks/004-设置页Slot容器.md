# T004 - settings.plugin-page Slot 前端容器

## 目标

把现有设置页插件渲染迁移到新 Slot + Host Bridge 机制：用 `settings.plugin-page` Slot 承载旧 WASM 插件的设置页，验证「旧插件经新桥接渲染」的最小闭环。

## 范围

- 前端：`frontend/src/components/PluginSettingsPanel.tsx`、`PluginIframe`（或新增 Slot 容器组件）。
- 桥接：替换直接调用 `pluginCall` 的路径，改走 T003 的 `bridge.call`（`plugin.*`）。

## 依赖

- 前置任务：T002（贡献列表能按 Slot 取出）、T003（桥接命令可用）。
- 后续任务：T005（端到端验证）、T006（沙箱容器扩展）。
- 可并行任务：无。
- 阻塞说明：需要 T002 提供「按 settings Slot 列贡献」、T003 提供桥接调用，两者缺一不可。

## 任务

- [ ] 实现最小 Slot 容器组件（暂仅 iframe 模式，沿用 `srcdoc + postMessage`）。
- [ ] `PluginSettingsPanel` 从 Slot Registry/贡献列表读取 `settings.plugin-page` 贡献（不再硬编码「设置页」语义）。
- [ ] iframe 内 `plugin_call` 消息改经宿主桥接 `bridge.call("plugin.*", ...)` 转发，保持结果回传。
- [ ] 保留主题 token 注入（`hostContext`）行为不变。

## 不做

- 不实现 Shadow DOM 容器（T006）。
- 不实现 `extension.tab` 与能力矩阵（T007 之后）。
- 不改动 WASM `plugin-ui` 接口本身，仅迁移前端调用通道。

## 验收

- 旧 WASM 插件（如 Memory）的设置页在设置面板中正常渲染。
- 页面内双向通信（读配置、改配置）经新桥接正常工作。
- 主题切换时 token 注入仍生效。

## 验证

- `yarn --cwd frontend build` 通过。
- 手动：打开「设置 → 插件」，确认 Memory 等插件设置页可加载、可交互、无 console 报错。
