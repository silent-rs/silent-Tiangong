# 截图输入插件

挂载在会话输入区的截图按钮。点击后可选择屏幕区域或窗口，确认的画面会以 PNG
附件加入当前输入草稿，不会自动发送。用户取消时不会添加附件；权限不足、截图工具
缺失或截图失败时会显示原因。

插件界面使用 **Vue 3 虚拟 DOM + Vite**，逻辑层使用 WASM，区域截图由按需启动的
sidecar 完成。App 只校验 PNG 并把附件加入输入草稿，不执行截图命令。

## 目录结构

```text
plugins/screenshot-input/
├── plugin.json          # v2 清单、Shadow 容器、Slot 与权限
├── index.html           # Vite 入口
├── src/
│   ├── main.ts          # Vue 挂载与宿主卸载清理
│   └── App.vue          # 截图按钮、忙碌状态与 scoped 样式
├── protocol/            # WASM 与 sidecar 私有协议
├── wasm/                # UI 消息与 sidecar 调用桥接
├── sidecar/             # 三平台区域截图实现
├── scripts/package.mjs  # 构建、签名并组装 release/
├── package.json
├── tsconfig.json
└── vite.config.ts
```

## 开发与打包

```sh
yarn install       # 首次安装依赖
yarn dev           # 浏览器中预览组件
yarn typecheck     # 类型检查
yarn package       # 构建、签名并组装当前平台 release/ 插件包
```

`yarn package` 需要设置官方插件签名环境变量，会生成包含界面、WASM、当前平台
sidecar 和签名清单的 `release/`。天工「设置 → 插件管理 → 导入本地插件」应选择
**`release/` 目录**，不要选择源码目录。没有签名密钥时可分别运行 `yarn build` 和
仓库根目录的 `cargo check` 做开发检查，但未签名的原生 sidecar 不会被 App 启动。

## 平台支持

- macOS：使用系统 `screencapture`，需要屏幕录制权限。
- Windows：使用插件内置的全屏区域选择界面和 Windows PowerShell，不依赖第三方截图软件。
- Linux：优先使用 Wayland 的 `grim` + `slurp`，也支持 `gnome-screenshot`、
  `spectacle`、`xfce4-screenshooter`、`maim`、`scrot` 或 ImageMagick `import`。

## 宿主交互

- 按钮先调用 `plugin.capture`，经 WASM 按需启动本插件 sidecar。
- 插件取得 PNG 后调用通用 `session.input.addAttachment`，由宿主加入当前草稿。
- Shadow 模式直接继承 App 的主题变量；组件样式使用 `scoped`，不订阅主题变化。
- 插件更新、禁用或卸载时，宿主会调用登记的清理函数卸载 Vue 实例。

通用 UI 插件开发方式见 [`docs/plugin-development.md`](../../docs/plugin-development.md)。
