# node-sidecar 模板

带常驻 node sidecar 的工具插件：Agent 调用 `query` 工具 → 插件页面（零构建）
经宿主桥接把请求转发给常驻 sidecar 处理。

## 依赖与打包

sidecar 是工程构建：`yarn install` 后 `build` 用 esbuild 把
`sidecar/main.mjs` 连同它 import 的一切（vendor 协议库与你在
`package.json` 的 `dependencies`）打成**单个自包含文件**进 `release/`。
安装后的 sidecar 运行时零依赖、零网络（构建期才需要网络装依赖）。

- 添加第三方依赖：`npx -y @silent-ai/plugin-creator@1.0.2 add <id> <包名>`
  （经命令通道执行，锁定精确版本），在 `sidecar/main.mjs` 正常 `import`。
- 限制：原生 `.node` 模块暂不支持打包；需要时用官方插件链路（Rust）。

## 结构

- `plugin.json`：声明 `sidecar.runtime = "node"`、入口 `sidecar/main.mjs`、
  `sidecar.invoke` 权限与 `query` 工具。
- `sidecar/main.mjs`：sidecar 源码，业务写在 `dispatch`（示例操作 `demo.echo`）。
- `sidecar/vendor/tiangong-sidecar-sdk/`：协议库副本（上游
  `plugins/sdk-sidecar`，改库后同步复制）。
- `app/index.html`：零构建工具页，`bridge.call("sidecar.<操作>")` 转发。
- `scripts/build.mjs` / `scripts/package.mjs`：打包与组装 release。

## 运行要求

- 开发机构建：yarn + Node ≥ 20（网络装依赖）。
- 宿主运行：PATH 中的 `node` ≥ 20（或 `TIANGONG_NODE_PATH` 指定）。
- 安装经 plugin-creator 原生确认；安装后 sidecar 文件被改动将拒绝启动
  （内容哈希锁定）。

## 迭代

修改 `sidecar/main.mjs` 或页面后，重新构建安装：

```bash
npx -y @silent-ai/plugin-creator@1.0.2 validate <id>
npx -y @silent-ai/plugin-creator@1.0.2 build <id>
```

然后在「插件创作」页安装（或让 Agent 调 `plugin_install`）。
