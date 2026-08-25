# node-sidecar 模板

带常驻 node sidecar 的工具插件：Agent 调用 `query` 工具 → 插件页面（本模板
零构建，无 Node 工程与网络依赖）经宿主桥接把请求转发给常驻 sidecar 处理。

## 结构

- `plugin.json`：声明 `sidecar.runtime = "node"`、入口 `sidecar/main.mjs`、
  `sidecar.invoke` 权限与 `query` 工具。
- `sidecar/main.mjs`：sidecar 入口，业务写在 `dispatch`（示例操作 `demo.echo`）。
- `sidecar/vendor/tiangong-sidecar-sdk/`：协议库副本（上游
  `plugins/sdk-sidecar`，改库后同步复制）。
- `app/index.html`：零构建工具页，`bridge.call("sidecar.<操作>")` 转发。

## 运行要求

- 宿主可找到 Node ≥ 20（PATH 中的 `node`，或以 `TIANGONG_NODE_PATH` 指定）。
- 安装经 plugin-creator 原生确认；安装后 sidecar 文件被改动将拒绝启动
  （内容哈希锁定）。

## 迭代

修改 `sidecar/main.mjs` 或页面后，重新构建安装：

```bash
npx -y @silent-ai/plugin-creator@1.0.2 validate <id>
npx -y @silent-ai/plugin-creator@1.0.2 build <id>
```

然后在「插件创作」页安装（或让 Agent 调 `plugin_install`）。
