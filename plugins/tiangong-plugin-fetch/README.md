# Fetch Plugin

Fetch 插件（`web_fetch`，text 模式提取正文 / download 模式落盘，含 SSRF 防护）由三个独立 crate 组成：

- `protocol`：WASM 与 sidecar 共用的私有业务协议；
- `wasm`：工具规格、参数解析与生命周期入口，桥接 sidecar；
- `sidecar`：reqwest 阻塞抓取、SSRF 防护与 download 落盘进程。

通用的清单、WASM、sidecar 和本地导入说明见 [WASM 插件开发指南](../../../docs/plugin-development.md)。

完整插件使用以下命令统一检查、构建和部署：

```bash
cargo run -p xtask -- build-plugin fetch
```

## 入口分工

- CLI / Server：由 runtime 按 `plugin.json` 的 `entrypoints: ["cli", "server"]` 自动加载。
- GUI：不加载本插件，`web_fetch` 由 browser 插件（内嵌浏览器渲染）提供。
