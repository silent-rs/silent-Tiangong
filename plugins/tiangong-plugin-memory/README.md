# Memory Plugin

Memory 插件由三个独立 crate 组成：

- `protocol`：WASM 与 sidecar 共用的私有业务协议；
- `wasm`：Core、工具、生命周期和设置页面入口；
- `sidecar`：Memory 存储、检索和模型调用进程。

通用的清单、WASM、sidecar 和本地导入说明见 [WASM 插件开发指南](../../../docs/plugin-development.md)。

完整插件使用以下命令统一检查、构建和部署：

```bash
cargo run -p xtask -- build-plugin memory
```

构建 sidecar 前需要安装 Protocol Buffers 编译器：macOS 使用 `brew install protobuf`，Linux 使用 `apt-get install protobuf-compiler`，Windows 使用 `choco install protoc`。
