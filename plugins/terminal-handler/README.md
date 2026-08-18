# 终端插件（Terminal Handler）

终端能力的 manifest v2 插件化（参考 `plugins/interaction-handler` 模式）：
工具声明（run_command / run_shell / terminal_send）与 prompt 引导来自本清单，
经 TsPluginAdapter 注入 Core；工具执行策略在 `src/shell.ts`（TS 壳）——
收到 tool.requested 后经宿主桥接 `terminal.*` 原生服务（PTY）执行并回传。

终端面板为宿主原生容器（xterm 渲染与 PTY 会话管理不在插件沙箱内），
本插件的 UI 入口承载工具壳逻辑与 extension.tab 声明。

## 开发循环

```sh
yarn package   # 构建 + 组装 release/
# 天工「设置 → 插件管理 → 导入本地插件」选择 release/ 目录
```

## 与宿主的协议

- 权限：`terminal.use`（原生服务）+ `tool.provide`（工具结果提交）
- 桥接方法：`terminal.runCommand` / `terminal.runShell` / `terminal.send`
- 工具超时：run 系 600s、terminal_send 120s（宿主 ts_tools 兜底）
