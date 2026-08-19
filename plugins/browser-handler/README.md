# 浏览器插件（Browser Handler）

浏览器能力的 manifest v2 插件化（阶段 4 完全体雏形）：
- 工具声明（browser_open/navigate/eval）与 prompt 来自本清单；
- 工具执行路由到宿主 **webview 容器原语**（`sandbox: webview`，第四种声明式
  容器——通用中立原语，非浏览器业务代码）；
- 页面渲染由 webview 原语承载；管理界面（地址栏/状态）在插件 shadow 容器。

## 开发循环

```sh
yarn package   # 构建 + 组装 release/（导入天工验证）
```

## 与宿主的协议

- 权限：`webview.use`（容器原语）+ `tool.provide`
- 桥接方法：`webview.navigate` / `webview.eval` 等（原语白名单）
- 容器：UI 贡献声明 `sandbox: "webview"`，实例生命周期归插件管理
