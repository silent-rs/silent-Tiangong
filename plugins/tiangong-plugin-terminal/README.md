# 终端插件（Terminal）

终端能力的 manifest v2 插件化。工具声明与 prompt 引导来自本清单，经
TsPluginAdapter 注入 Core。

工具执行策略在 sidecar（`sidecar/src/service.rs`，Rust 进程）：plugin.json
sidecar 握手通过 `tool:<name>` capabilities 注册五个工具 Handler，宿主把工具调用直连 sidecar，并随请求注入
宿主权威会话上下文（`session_id` 与会话工作目录，来自 Session 真相源）。
每个会话的终端编排（选终端、新建、开标签、执行、收结果）由 sidecar 内
对应会话作用域唯一完成；页面实例（`src/main.ts` + `terminal-view.ts`）
只承担显示与输入，不参与工具调度，也无 `tool.requested` 竞争接应。

`run_command` / `run_shell` 不接收终端编号：优先复用当前会话的第一个
空闲终端，没有可用终端时自动新建并静默建立标签（不展开拓展区）。
工具结果会返回实际终端编号；`terminal_send` / `terminal_close` 必须用
该编号精确操作，且目标终端必须属于当前会话——跨会话操作明确拒绝，
不回退当前可见会话。

sidecar 执行中经进度帧请求宿主 App 原语（`host_action`，白名单
`app.open` / `app.close`）：长任务执行期间终端标签即时建立，用户可
实时查看输出。

顶部标签切换或拓展区暂时隐藏时保留当前终端；明确关闭终端 App 标签时
结束该终端并清除恢复记录，再次打开会创建全新终端。Sub Agent / Bot
等后台会话无需任何页面实例即可独立执行终端工具。

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

- 权限：`tool.provide`（工具结果提交）+ `sidecar.invoke`（视图直调
  sidecar 操作）+ `app.use`（App 标签原语）
- 工具直连：宿主经 `invoke_with_context` 调用，operation 为工具名；
  请求帧携带 `context`（session_id / invocation_id / 权威 workspace），
  缺失即拒绝执行
- 工具超时：run 系 300s、terminal_send 120s、面板操作 30s（清单
  `timeout_ms`，宿主工具级超时；会话取消按 session_id 定向取消请求）
