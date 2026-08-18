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

## 开发循环（打包 → 本地导入 → 验证）

```sh
yarn install        # 首次
yarn dev            # 本地开发服务器（预览 UI 布局与交互）
yarn package        # 开发期打包：构建 + 组装 release/ 插件包
```

完整开发流程：

1. 修改 `src/App.vue` 等源码；
2. `yarn package`——构建自包含产物并组装 **`release/`**（plugin.json + dist/，
   不含源码与 node_modules），打包即校验清单与 entry；
3. 天工「设置 → 插件管理 → 导入本地插件」，选择 **`release/` 目录**——
   走正式导入流程（清单校验 → 事务安装 → 注册表加载）；
4. 发起一次审批/征询验证处理器行为；已安装时可在插件管理中热加载或
   重新导入新版本。

> `yarn build` 仅产出 `dist/`；分发制品（OSS 目录、签名）属于后续发布流程，
> 开发期不需要。

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
