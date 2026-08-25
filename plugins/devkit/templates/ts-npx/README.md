# {{PLUGIN_NAME}}

天工自建插件（ts-npx 模板：npx 命令行脚本能力）。

执行模型：脚本由 Agent 经**命令执行通道**运行（无 sidecar、无宿主执行层、
无需构建）——plugin.json 的能力说明（prompt）告诉 Agent 怎么调用；
沙箱、联网审批、会话信任全部复用命令通道的现成机制。

## 迭代流程

1. 修改 `tools/main.ts`（CLI 式：参数输入、stdout 输出 JSON 结果）与
   `plugin.json` 的能力说明段（教 Agent 怎么用）；
2. 开发期自测：经命令通道执行 devkit 试运行
   （npx -y @silent-ai/plugin-creator@1.0.0 run <id> -- <参数>，
   按 run.json 声明执行，日志在 `logs/run.log`）；
3. 分发：plugin_validate → plugin_build（零构建直接打包）→ plugin_install
   （原生确认，内容哈希锁定）；
4. 装好后：Agent 按说明书经命令通道执行
   `npx -y tsx@4.19.2 ~/.tiangong/plugins/{{PLUGIN_ID}}/tools/main.ts ...`。

要求本机 Node ≥ 20；首次运行联网下载 tsx（经命令网络审批）。
