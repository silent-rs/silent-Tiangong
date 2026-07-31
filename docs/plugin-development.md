# 天工 WASM 插件开发指南

本文说明当前插件化试开发阶段如何创建、构建和本地导入天工插件。权威宿主接口位于 [`plugin.wit`](../crates/tiangong-plugin-runtime/wit/tiangong/plugin.wit)，Memory 插件是完整参考实现。

## 插件组成

一个可安装插件至少包含 WASM Component，也可以带一个原生 sidecar：

```text
WASM Component
    |  通用 Host 接口
    v
天工插件运行时
    |  通用认证与 JSON Lines 协议
    v
可选 sidecar
```

- WASM 负责工具、提示词注入、生命周期和设置页面。
- sidecar 负责数据库、文件系统、原生库或长时间运行的业务。
- 天工只负责加载、资源限制、sidecar 进程管理和消息转发，不理解插件业务负载。
- WASM 与 sidecar 之间的私有业务协议由插件自己维护。

## 开发环境

当前仓库使用 Rust 2024 edition，WASM 目标为 `wasm32-wasip2`：

```bash
rustup target add wasm32-wasip2
cargo check -p tiangong-plugin-runtime
```

需要修改天工前端时使用 yarn：

```bash
yarn --cwd frontend install
yarn --cwd frontend dev
```

## 目录结构

建议把同一插件的源码放在一个目录中：

```text
crates/plugins/tiangong-plugin-example/
├── plugin.json
├── protocol/          # 可选，WASM 与 sidecar 共用的私有协议
├── wasm/
└── sidecar/           # 可选
```

最终导入的是构建后的完整目录，不是源码目录：

```text
example-plugin/
├── plugin.json
├── example_plugin.wasm
└── example-sidecar    # 清单声明 sidecar 时必须提供
```

`runtime`、`logs` 和 `data` 由天工管理，不需要放进导入目录。

## 插件清单

`plugin.json` 使用以下格式：

```json
{
  "schema_version": 1,
  "id": "example",
  "version": "0.1.0",
  "wasm": {
    "binary": "example_plugin.wasm"
  },
  "sidecar": {
    "binary": "example-sidecar",
    "transport_protocol": "0.1.0",
    "business_protocol": 1,
    "startup_timeout_ms": 15000,
    "request_timeout_ms": 30000
  },
  "permissions": ["sidecar.invoke"]
}
```

约束如下：

- `schema_version` 当前必须为 `1`。
- `id` 只能包含 ASCII 字母、数字、点、下划线和连字符。
- `version` 必须是语义版本，例如 `0.1.0`。
- `wasm.binary` 和 `sidecar.binary` 必须是插件目录内的安全相对路径。
- WASM `describe()` 返回的 ID、版本必须与清单完全一致。
- 声明 sidecar 时，插件权限需要包含 `sidecar.invoke`。
- Windows 制品在清单名称后自动使用 `.exe` 后缀，清单本身不要重复添加平台后缀。

不需要 sidecar 的插件应删除整个 `sidecar` 字段，并可将 `permissions` 留空。

## 实现 WASM Component

插件必须实现 [`tiangong-plugin` world](../crates/tiangong-plugin-runtime/wit/tiangong/plugin.wit)，包括 `plugin` 与 `plugin-ui` 两组导出。即使插件没有页面，也需要实现 `plugin-ui` 并返回空贡献列表。

最小 crate 配置：

```toml
[package]
name = "tiangong-plugin-example-wasm"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.46"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

在本仓库开发时直接引用权威 WIT：

```rust
wit_bindgen::generate!({
    path: "../../../tiangong-plugin-runtime/wit/tiangong/plugin.wit",
});
```

实现时需要关注这些导出：

- `describe`：返回稳定的插件 ID、显示名称和版本。
- `tool-specs`、`handle-tool`：声明并执行工具。
- `prompt-sections`：向当前会话提示词添加插件内容。
- `set-workspace` 和会话、轮次生命周期：接收宿主只读状态。
- `contributions`：声明设置页入口。
- `open-view`、`get-view-resource`、`handle-view-message`：提供页面和双向消息。
- `shutdown`：释放 WASM 内部状态。

宿主导入目前包括：

- `clock.now-millis`：读取真实时间。
- `sidecar.invoke`：向当前插件绑定的 sidecar 转发操作名和 JSON。

完整实现可参考 [Memory WASM](../crates/plugins/tiangong-plugin-memory/wasm/src/lib.rs) 和它的 [绑定入口](../crates/plugins/tiangong-plugin-memory/wasm/src/bindings.rs)。

构建命令：

```bash
cargo build -p tiangong-plugin-example-wasm --target wasm32-wasip2 --release
```

将 `target/wasm32-wasip2/release/` 下生成的 `.wasm` 文件复制到完整插件目录，并确保名称与清单一致。

## 接入 sidecar

sidecar 启动时由天工注入以下环境变量：

| 环境变量 | 内容 |
| --- | --- |
| `TIANGONG_PLUGIN_ID` | 插件 ID |
| `TIANGONG_PLUGIN_VERSION` | 插件发布版本 |
| `TIANGONG_PLUGIN_ENDPOINT` | sidecar 写入 endpoint 的文件路径 |
| `TIANGONG_PLUGIN_DATA_DIR` | 插件独立数据目录 |

sidecar 必须：

1. 监听本地 TCP 端口并生成短期认证 Token。
2. 将 host、port、PID、Token 写入指定 endpoint 文件。
3. 使用运行时 `0.1.0` JSON Lines 帧协议完成认证、请求和响应。
4. 实现 `runtime.handshake`，返回插件 ID、插件版本、传输协议版本、业务协议版本和运行状态。
5. 保证握手中的插件版本与 `plugin.json`、WASM `describe()` 一致。

协议类型见 [`protocol.rs`](../crates/tiangong-plugin-runtime/src/protocol.rs)，进程行为见 [`sidecar.rs`](../crates/tiangong-plugin-runtime/src/sidecar.rs)。可运行示例见 [Memory sidecar](../crates/plugins/tiangong-plugin-memory/sidecar/src/main.rs)。

## 本地导入与调试

完成插件目录后：

1. 启动天工并打开“设置”。
2. 进入“插件管理”。
3. 点击“导入本地插件”图标。
4. 选择包含 `plugin.json` 的完整插件目录。

导入过程会复制制品，不会直接从开发目录运行。天工随后依次检查：

- 清单格式和语义版本；
- 清单声明的 WASM 与当前平台 sidecar 是否为实际文件；
- WASM 是否能实例化；
- WASM 描述符与清单 ID、版本是否一致；
- sidecar 是否能启动并通过身份、版本和协议握手。

全部检查成功后才会切换插件。导入失败时继续使用原版本。

版本规则：

- 未安装的插件可以直接导入。
- 同版本允许重新导入，便于本地开发调试。
- 更高版本按升级处理，并保留一个可回滚版本。
- 更低版本会被拒绝，需要使用“回滚”或修改插件版本。
- 替换时保留当前插件的 `runtime`、`logs` 和 `data`；开发目录中的这些内容不会被导入。

本地目录由用户主动选择，因此不要求 OSS SHA-256。只应导入可信源码构建出的插件。OSS 安装仍会验证目录中声明的 SHA-256。

## Memory 参考构建

Memory 插件同时包含私有协议、WASM 和 sidecar，可用一条命令完成检查、构建、本地部署和 OSS 制品生成：

```bash
cargo run -p xtask -- build-plugin memory
```

生成的 OSS 上传目录位于：

```text
target/plugin-dist/
```

Memory 的目录和构建说明见 [`crates/plugins/tiangong-plugin-memory/README.md`](../crates/plugins/tiangong-plugin-memory/README.md)。

## 常见问题

### 提示清单与组件 ID 或版本不一致

同时更新 `plugin.json` 和 WASM `describe()`。带 sidecar 时还要更新握手返回的插件版本。

### 提示当前平台 sidecar 不存在

确认完整插件目录包含当前操作系统和架构可执行的 sidecar。Windows 文件名应带 `.exe`。

### sidecar 启动后仍显示异常

检查插件目录下的 `logs/sidecar.log`，并确认 endpoint 文件由当前进程生成、握手协议为 `0.1.0`。

### 导入同版本后代码没有变化

确认选择的是构建制品目录而不是源码目录，并先重新构建 WASM 或 sidecar。导入成功后插件会立即替换，无需重启天工。
