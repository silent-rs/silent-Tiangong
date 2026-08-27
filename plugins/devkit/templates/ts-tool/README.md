# {{PLUGIN_NAME}}

天工自建插件（ts-tool 模板：UI 页面 + 向 Agent 提供工具）。

## 迭代流程

1. 修改 `plugin.json`（工具声明）与 `src/App.vue`（工具处理逻辑 `handleToolCall`）；
2. 经 plugin creator 页面或 Agent 工具依次执行：校验（plugin_validate）→ 构建（plugin_build，需要 Node ≥ 20 与 yarn）→ 安装（plugin_install，官方 Creator 自动签名免确认）；
3. 构建产物在 `release/`，构建日志在 `logs/build.log`；
4. 内容变更后重新安装需再次经用户确认。

依赖的 `@tiangong/plugin-sdk` 位于 `vendor/plugin-sdk`（随模板内置，无需网络）。
