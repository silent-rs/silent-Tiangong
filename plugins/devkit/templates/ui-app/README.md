# {{PLUGIN_NAME}}

天工自建插件（ui-app 模板：纯 UI 页面插件，零构建依赖）。

## 迭代流程

1. 编辑 `app/index.html`（页面与逻辑都在这个自包含文件里，内含宿主桥接封装与看板示例）；
2. 经 plugin creator 页面或 Agent 工具依次执行：校验（plugin_validate）→ 构建（plugin_build，零构建模板无需 Node，直接打包）→ 安装（plugin_install，官方 Creator 自动签名免确认）；
3. 构建产物在 `release/`；
4. 内容变更后重新安装需再次经用户确认。
