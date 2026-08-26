# node-tool 模板

**无界面纯工具插件**：Agent 直接调用 plugin.json 声明的工具，宿主把调用
直连到本插件的按需 node sidecar 执行——没有拓展区页面，装完即用，@提及
点名调用亦可。

适合"纯能力"型插件（文本处理、格式转换、数据查询等）；需要界面交互或
面板呈现的用 node-sidecar / ts-tool 模板。

## 工具契约

- 操作名 = 工具名（如 `text_analyze`），参数 = 工具调用参数对象；
- sidecar dispatch 返回 ToolOutcome 形状：`{ok, summary, stdout?, stderr?,
  exit_code?}`（summary 会展示给 Agent，stdout 放正文输出）；
- 新增工具：sidecar/main.mjs 加分支 + plugin.json 的 tools 加声明，两处
  同名对应。

## 依赖与打包

esbuild 将 `sidecar/main.mjs` 连同协议库与 dependencies 打成单个自包含
文件进 `release/`；运行时零依赖、零网络。添加依赖用
`plugin-creator add <id> <包名>`（锁定精确版本）。

## 迭代

```bash
npx -y @silent-ai/plugin-creator@1.0.2 validate <id>
npx -y @silent-ai/plugin-creator@1.0.2 build <id>
```

然后用 plugin_install 安装（原生确认）。
