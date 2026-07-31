# Memory Plugin

Memory 插件由三个独立 crate 组成：

- `protocol`：WASM 与 sidecar 共用的私有业务协议；
- `wasm`：Core、工具、生命周期和设置页面入口；
- `sidecar`：Memory 存储、检索和模型调用进程。

完整插件使用以下命令统一检查、构建和部署：

```bash
cargo run -p xtask -- build-plugin memory
```
