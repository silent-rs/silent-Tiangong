# plugin creator（插件创作）

自建插件辅助插件（RFC 0017 §11 / S5.1 一期）：官方签名纯 TS 工具插件，无自有
sidecar。贡献「插件创作」extension.tab 页面与 `plugin_init / plugin_build /
plugin_install / plugin_validate / plugin_logs` 工具集，页面与 Agent 调用共用
同一 `plugin-dev.*` 宿主受限通道。

## 用户旅程

1. 用户对 Agent 说"帮我做一个 XX 插件"（或在创作页手动新建）；
2. `plugin_init` 按模板（ui-app / ts-tool）生成骨架到 `~/.tiangong/plugins-dev/<id>/`；
3. Agent 按需求在该目录填充代码（普通编码工作流）；
4. `plugin_validate` → `plugin_build`（零构建模板直接打包；工程模板执行
   yarn install/build/package，需 Node ≥ 20 与 yarn）；
5. `plugin_install` → 宿主以本机用户密钥自动签名并安装启用（免确认，仅官方 Plugin Creator 可触发）；
6. 迭代：修改 → 重新 build/install；故障：`plugin_logs` 诊断。

## 模板

模板位于 `templates/`（plugin.json `resources` 声明，随插件包分发），与仓库
`plugins/templates/` 同源：

- `ui-app`：纯 UI 插件，零构建依赖；
- `ts-tool`：TS 工具插件（interaction 同款结构），vendor 内置 SDK。

## 开发

```bash
yarn install
yarn build      # typecheck + vite 单文件构建
yarn package    # 组装 release/（含 templates 与内容树清单）
```

开发验证：天工「设置 → 插件管理 → 导入本地插件」选择 `release/` 目录。

## 权限面

`tool.provide`（工具集）、`bridge.call`（页面通信）、`plugin-dev.use`（受限
开发通道：写范围锁定 plugins-dev 开发目录与只读日志）、`storage.private`
（安装历史）。不可触达信任库、公钥库与宿主设置。
