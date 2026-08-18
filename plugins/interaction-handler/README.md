# 交互处理器插件（Interaction Handler）

天工默认交互处理器：处理 `request_user` 工具发起的审批、确认、选择、输入与
表单请求。第三方可仿照本工程开发自己的处理器并替换（宿主保留闭合判定、
授权与超时权威，插件只提交用户选择）。

基于 **Vue 3 + Vite** 工程化开发，产物为自包含单文件 HTML（JS/CSS 内联，
适配宿主 iframe `srcdoc` 容器）。

## 目录结构

```text
plugins/interaction-handler/
├── plugin.json      # v2 清单：interaction.handle 权限 + session.interaction Slot
├── index.html       # Vite 入口
├── src/
│   ├── main.ts
│   └── App.vue      # 处理器 UI：六种请求 + 倒计时 + 提交锁 + 闭合状态
├── dist/index.html  # 构建产物（清单 entry 指向此处）
└── vite.config.ts   # 单文件打包（vite-plugin-singlefile）
```

## 开发与构建

```sh
yarn install        # 首次
yarn build          # 构建到 dist/（单文件，自包含）
yarn dev            # 本地开发服务器（预览 UI；宿主联调仍需 build 后导入）
```

修改 `src/App.vue` 后执行 `yarn build`，重新在天工导入本目录（或对已装插件
执行热加载）查看效果。

## 导入使用

天工「设置 → 插件管理 → 导入本地插件」，选择**本插件目录**（须包含
`plugin.json` 与 `dist/`）。

## 与宿主的协议

- 订阅 `interaction.requested` / `interaction.closed`（需声明
  `capabilities.events: ["interaction.*"]`）
- 提交 `interaction.resolve(request_id, result)`；不提交会话，宿主按
  request_id 权威路由
- 倒计时按宿主下发的绝对 `deadline` 展示，到期本地禁用；后端超时为准
- 主题跟随：宿主 hostContext 的设计 token 以 CSS 变量注入（如
  `var(--primary)`）

工程化依赖：`@tiangong/plugin-sdk`（本地路径引用 `../sdk`）提供
`createInteractionHandler()`（onRequested/onClosed/resolve）与桥接类型。
