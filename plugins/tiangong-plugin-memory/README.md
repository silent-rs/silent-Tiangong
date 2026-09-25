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

## 独立运行（第三方 Agent）

sidecar 二进制也可脱离天工使用，与天工共享同一份记忆：

```bash
tiangong-memory-sidecar --mcp                        # stdio MCP Server
tiangong-memory-sidecar --daemon --token <令牌>      # 后台 HTTP REST（默认 127.0.0.1:7717）
tiangong-memory-sidecar --stop                       # 停止后台 daemon
tiangong-memory-sidecar --config                     # 浏览器配置页，点击"完成并关闭"后退出
tiangong-memory-sidecar --config --host 0.0.0.0 --no-open   # 远程配置：打印链接，在其他机器打开
tiangong-memory-sidecar --check-update | --update    # 从官方插件目录检查 / 自更新
```

接口、生命周期接入约定与更新校验流程见 [通用运行模式](../../docs/memory-system/14-通用运行模式.md)。
