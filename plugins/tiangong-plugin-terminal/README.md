# 终端插件（Terminal）

终端能力的 manifest v2 插件化（参考 `plugins/tiangong-plugin-interaction` 模式）：
工具声明与 prompt 引导来自本清单，
经 TsPluginAdapter 注入 Core；工具执行策略在 `src/shell.ts`（TS 壳）——
收到 tool.requested 后创建长期交互 PTY，在后台建立对应终端 App，在该 shell 内执行命令并等待真实输出、退出状态后回传，不自动展开拓展区。

`run_command` / `run_shell` 不接收终端编号：插件优先复用当前会话的第一个
空闲终端，没有可用终端时自动新建并在后台建立标签。工具结果会返回实际终端编号；
`terminal_send` / `terminal_close` 必须用该编号精确操作，避免多终端时误选。

终端面板为宿主原生容器（xterm 渲染与 PTY 会话管理不在插件沙箱内），
本插件的 UI 入口承载工具壳逻辑与 extension.tab 声明。

顶部标签切换或拓展区暂时隐藏时保留当前终端；明确关闭终端 App 标签时
结束该终端并清除恢复记录，再次打开会创建全新终端。
`run_command` / `run_shell` 创建 PTY 后会通过公共 SDK 在后台建立对应终端标签，
不会自动展开拓展区；用户主动打开后可查看执行内容。多个前后台插件实例只允许一个实例处理同一工具调用。

## 开发循环

```sh
# 首次：确认官方私钥就位（不进仓库；有密码时另设
# TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PASSWORD）
export TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH=~/.tiangong/keys/plugin-signing.key

yarn package   # 构建 + 组装 release/ + 官方签名（release.json/.sig）
# 天工「设置 → 插件管理 → 导入本地插件」选择 release/ 目录
```

sidecar 插件必须带官方签名才能启动原生 sidecar（`tauri signer` 签名，
需要本地安装 tauri-cli）。无官方私钥时可用自生成密钥调试：
`tauri signer generate -w <key>` 打包签名后，启动应用时设置
官方信任根为应用内置公钥，不接受环境变量或配置覆盖（历史测试通道已移除）。

## 与宿主的协议

- 权限：`terminal.use`（原生服务）+ `tool.provide`（工具结果提交）
- 桥接方法：`terminal.runCommand` / `terminal.runShell` / `terminal.send`
- 工具超时：run 系 600s、terminal_send 120s（宿主 ts_tools 兜底）
