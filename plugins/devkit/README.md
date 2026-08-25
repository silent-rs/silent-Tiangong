# @silent-ai/plugin-creator（devkit）

天工插件创作工具链 CLI。经命令通道执行（Agent 的 run_command）：

```bash
npx -y @silent-ai/plugin-creator@1.0.2 init ui-app my-dashboard --name 我的看板
npx -y @silent-ai/plugin-creator@1.0.2 validate my-dashboard
npx -y @silent-ai/plugin-creator@1.0.2 build my-dashboard
npx -y @silent-ai/plugin-creator@1.0.2 run my-dashboard -- --name 测试
npx -y @silent-ai/plugin-creator@1.0.2 logs dev:my-dashboard
```

约定：stdout 输出单个 JSON 结果（`{ok, ...}`）；人读信息写 stderr；非零退出码表示失败。
开发目录固定 `~/.tiangong/plugins-dev/<id>/`（`--root` 或 `TIANGONG_PLUGINS_DEV` 覆盖）。

命令与职责：

| 命令 | 说明 |
|---|---|
| `init` | 按模板生成骨架（模板随本包分发：ui-app / ts-tool / ts-npx / node-sidecar），占位符替换，防劫持与重复初始化 |
| `add` | 为工程模板项目添加依赖（`yarn add --exact` 锁定精确版本；包名校验拒绝注入与 file:/git 等本地依赖） |
| `validate` | 清单校验（plugin.json 解析、字段、UI 入口存在性、sidecar 权限一致性） |
| `build` | 工程模板 yarn install→build→package；零构建模板内建打包（含内容树清单） |
| `run` | 按项目 run.json 试运行（npx 子进程、120s 超时、npm 缓存隔离） |
| `logs` | 读 `dev:<id>`（build.log/run.log）或 `plugin:<id>`（安装目录运行日志）尾部 |

安装插件不走本 CLI——用 plugin-creator 天工插件的 `plugin_install` 工具
（宿主原生确认 + 注册表加载）。

## 发布

```bash
cd plugins/devkit
npm publish --access public   # 版本号随 package.json，命令引用处同步精确版本
```

## 开发验证

```bash
node bin/cli.mjs init ts-npx demo --name 演示 --root /tmp/plugins-dev
node bin/cli.mjs build demo --root /tmp/plugins-dev
```
