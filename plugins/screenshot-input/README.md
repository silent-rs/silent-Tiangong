# 截图输入插件

挂载在会话输入区的截图按钮。点击后可选择屏幕区域或窗口，确认的画面会以 PNG
附件加入当前输入草稿，不会自动发送。系统屏幕共享授权被拒绝或用户取消时，不会
添加附件。

插件使用 **Vue 3 虚拟 DOM + Vite** 开发，构建为自包含单文件 HTML，并由宿主注入
`session.input-action` 的 Shadow DOM。Vue 负责组件状态更新，Shadow DOM 负责与 App
界面的挂载和样式隔离。

## 目录结构

```text
plugins/screenshot-input/
├── plugin.json          # v2 清单、Shadow 容器、Slot 与权限
├── index.html           # Vite 入口
├── src/
│   ├── main.ts          # Vue 挂载与宿主卸载清理
│   └── App.vue          # 截图按钮、忙碌状态与 scoped 样式
├── scripts/package.mjs  # 组装可导入的 release/
├── package.json
├── tsconfig.json
└── vite.config.ts
```

## 开发与打包

```sh
yarn install       # 首次安装依赖
yarn dev           # 浏览器中预览组件
yarn typecheck     # 类型检查
yarn package       # 构建并组装 release/ 插件包
```

`yarn package` 会生成 `release/plugin.json` 和 `release/dist/index.html`，并校验清单
入口。天工「设置 → 插件管理 → 导入本地插件」应选择 **`release/` 目录**，不要选择
源码目录。

## 宿主交互

- 按钮调用通用桥接方法 `session.input.captureRegion`，清单声明 `session.write` 权限。
- 截图完成后的附件由宿主加入当前草稿，插件不直接操作输入组件。
- Shadow 模式直接继承 App 的主题变量；组件样式使用 `scoped`，不订阅主题变化。
- 插件更新、禁用或卸载时，宿主会调用登记的清理函数卸载 Vue 实例。

通用 UI 插件开发方式见 [`docs/plugin-development.md`](../../docs/plugin-development.md)。
