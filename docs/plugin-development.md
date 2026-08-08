# 天工 WASM 插件开发指南

本文说明当前插件化试开发阶段如何创建、构建和本地导入天工插件。权威宿主接口位于 [`plugin.wit`](../crates/tiangong-plugin-runtime/wit/tiangong/plugin.wit)，Memory 插件是完整参考实现。

## 插件组成

一个可安装插件至少包含 WASM Component。未签名的第三方插件只允许使用纯 WASM；原生 sidecar 仅对带有效天工官方签名的发布包开放：

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

## WASM Runtime 公共中立约束

`tiangong-plugin-runtime` 是所有插件共享的基础设施，后续开发必须保持公共、中立和业务无关。Runtime 只能提供任意插件都能复用的加载、隔离、资源限制、生命周期转发、通用反馈、sidecar 传输和制品管理能力。

禁止在 Runtime、公共 WIT 或通用 sidecar 协议中加入：

- 具体插件 ID、工具名、操作名或基于插件身份的条件分支；
- 具体插件的请求、响应、事件、配置和数据结构；
- 具体插件的数据目录、兼容路径、迁移规则或生命周期策略；
- 仅为某个插件服务的便捷方法、错误码、状态字段或特殊传输流程；
- 对 JSON 业务负载内容的解析、改写、筛选或语义判断。

单个插件需要的功能必须放在以下位置之一：

- 插件 WASM：工具行为、提示词、生命周期编排和宿主通用能力调用；
- 插件私有协议：WASM 与 sidecar 共用的业务操作、请求和响应；
- 插件 sidecar：数据库、原生库、业务处理和插件自己的兼容逻辑。

确需扩展 Runtime 时，改动必须同时满足：

1. 不知道调用方是哪一个插件也能正确工作；
2. 接口和实现不引用任何具体插件的名称或类型；
3. 至少能合理服务两类不同插件，而不是把单插件逻辑包装成“通用”接口；
4. Runtime 只转发不透明负载或通用数据，不解释业务含义；
5. 新能力有独立的通用测试，插件自己的行为测试留在插件目录。

代码审查发现单插件需求进入 Runtime 时，应退回插件侧实现。不得以临时兼容、示例插件或当前只有一个使用方为理由增加特化处理。

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
plugins/tiangong-plugin-example/
├── plugin.json
├── protocol/          # 可选，WASM 与 sidecar 共用的私有协议
├── wasm/
└── sidecar/           # 可选
```

最终导入的是构建后的完整目录，不是源码目录。纯 WASM 插件无需签名；官方 sidecar 插件还必须包含签名清单：

```text
example-plugin/
├── plugin.json
├── example_plugin.wasm
├── example-sidecar      # 清单声明 sidecar 时必须提供
├── release.json         # 官方 sidecar 插件必须提供
└── release.json.sig     # 官方 sidecar 插件必须提供
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
- 声明 sidecar 时，插件权限必须包含 `sidecar.invoke`，且发布包必须带有效官方签名。
- `model-config.read` 允许 sidecar 取得包含模型配置的应用存储根目录，仅用于需要直接调用模型的官方插件。
- `app-storage.read` 允许 sidecar 取得应用共享存储根目录，用于兼容 Index、Scheduler、Skill、MCP 等既有业务数据；该权限同样只接受官方签名授权。
- `model-config.read` 与 `app-storage.read` 都是敏感权限，不得仅通过修改 `plugin.json` 自行获取。
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
- `feedback.emit-stream-event`：向当前 turn 发送通用 `StreamEvent` JSON；Runtime 只验证公共事件结构，不理解具体插件业务。

完整实现可参考 [Memory WASM](../plugins/tiangong-plugin-memory/wasm/src/lib.rs) 和它的 [绑定入口](../plugins/tiangong-plugin-memory/wasm/src/bindings.rs)。

构建命令：

```bash
cargo build -p tiangong-plugin-example-wasm --target wasm32-wasip2 --release
```

将 `target/wasm32-wasip2/release/` 下生成的 `.wasm` 文件复制到完整插件目录，并确保名称与清单一致。

## 接入 sidecar

sidecar 启动时由天工注入以下环境变量。前三项始终可用；共享存储根只有签名权限授权后才会注入：

| 环境变量 | 内容 |
| --- | --- |
| `TIANGONG_PLUGIN_ID` | 插件 ID |
| `TIANGONG_PLUGIN_VERSION` | 插件发布版本 |
| `TIANGONG_PLUGIN_ENDPOINT` | sidecar 写入 endpoint 的文件路径 |
| `TIANGONG_PLUGIN_DATA_DIR` | 插件独立数据目录 |
| `TIANGONG_STORAGE_ROOT` | 天工应用共享存储根；仅官方签名的 `model-config.read` 或 `app-storage.read` 插件可获得 |

sidecar 必须：

1. 监听本地 TCP 端口并生成短期认证 Token。
2. 将 host、port、PID、Token 写入指定 endpoint 文件。
3. 使用运行时 `0.1.0` JSON Lines 帧协议完成认证、请求和响应。
4. 实现 `runtime.handshake`，返回插件 ID、插件版本、传输协议版本、业务协议版本和运行状态。
5. 保证握手中的插件版本与 `plugin.json`、WASM `describe()` 一致。

协议类型见 [`protocol.rs`](../crates/tiangong-plugin-runtime/src/protocol.rs)，进程行为见 [`sidecar.rs`](../crates/tiangong-plugin-runtime/src/sidecar.rs)。可运行示例见 [Memory sidecar](../plugins/tiangong-plugin-memory/sidecar/src/main.rs)。

## 本地导入与调试

完成插件目录后：

1. 启动天工并打开“设置”。
2. 进入“插件管理”。
3. 点击“导入本地插件”图标。
4. 选择包含 `plugin.json` 的完整插件目录。

导入过程会复制制品，不会直接从开发目录运行。天工随后依次检查：

- 清单格式和语义版本；
- 清单声明的 WASM 与当前平台 sidecar 是否为实际文件；
- `release.json` 与 `release.json.sig` 是否成对存在；
- 带 sidecar 时是否为有效官方签名，签名权限是否与清单一致，签名覆盖的清单、WASM 和 sidecar 哈希是否匹配；
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

本地目录由用户主动选择，但本地选择不能绕过原生代码边界：未签名纯 WASM 可以导入；任何带 sidecar 的插件都必须携带有效官方签名。第三方开发者应使用纯 WASM 和受限 Host 接口，不应获得官方签名私钥。OSS 安装还会先校验目录声明的 SHA-256，再执行统一签名验证。

## Memory 参考构建

Memory 插件同时包含私有协议、WASM 和 sidecar，可用一条命令完成检查、构建、本地部署和 OSS 制品生成：

```bash
TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH=~/.tiangong/keys/plugin-signing.key \
  cargo run -p xtask -- build-plugin memory
```

该命令会生成 `release.json` 与 `release.json.sig`。签名私钥只应保存在天工官方发布环境或本地受保护的开发密钥目录，绝不能写入插件包或提交到仓库；签名密码可通过 `TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PASSWORD` 提供。

生成的 OSS 上传目录位于：

```text
target/plugin-dist/
```

Memory 的目录和构建说明见 [`plugins/tiangong-plugin-memory/README.md`](../plugins/tiangong-plugin-memory/README.md)。

## 常见问题

### 提示清单与组件 ID 或版本不一致

同时更新 `plugin.json` 和 WASM `describe()`。带 sidecar 时还要更新握手返回的插件版本。

### 提示当前平台 sidecar 不存在

确认完整插件目录包含当前操作系统和架构可执行的 sidecar。Windows 文件名应带 `.exe`。

### sidecar 启动后仍显示异常

检查插件目录下的 `logs/sidecar.log`，并确认 endpoint 文件由当前进程生成、握手协议为 `0.1.0`。

### 导入同版本后代码没有变化

确认选择的是构建制品目录而不是源码目录，并先重新构建 WASM 或 sidecar。导入成功后插件会立即替换，无需重启天工。

## GitHub Actions 发布到 OSS

短期官方插件只通过 OSS 静态目录独立发布，不建设插件服务平台。独立工作流位于 [`.github/workflows/publish-plugins.yml`](../.github/workflows/publish-plugins.yml)，可手动填写一个插件 ID，或使用 `plugin/<plugin-id>/v<version>` 标签触发。工作流不维护插件下拉白名单，而是让仓库构建配置核对该 ID 是否对应 `plugins/` 下完整的 `plugin.json`、WASM、protocol 和 sidecar；每次只构建、签名和上传所选插件。可通过 `cargo run -p xtask -- list-plugins` 查询当前可发布插件。

日常发布一次只触发一个插件，并等待任务结束后再发布下一个。首次集中发布多个插件时每批最多触发两个，确认本批任务全部结束后再触发下一批；不要同时推送大量插件标签，因为共享发布锁只负责串行写入，等待中的任务可能被后续任务取消。

仓库需要配置以下 GitHub Actions Secrets：

- `TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY`：插件专用签名私钥内容，不得与应用更新私钥复用。
- `TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PASSWORD`：插件签名私钥密码；无密码时可留空。
- `ALIYUN_OSS_ACCESS_KEY_ID`、`ALIYUN_OSS_ACCESS_KEY_SECRET`：OSS 上传凭据。

发布顺序为：

1. macOS Apple Silicon、Linux x86_64、Windows x86_64 分别构建所选插件；带 sidecar 的插件同时生成平台签名，纯 WASM 插件不生成签名清单。
2. 三个平台产物上传为短期 Actions Artifact，签名临时文件立即删除。
3. Linux 汇总任务只合并所选插件，校验其 ID、版本、清单与 WASM 完全一致；带 sidecar 的插件必须具备三个平台的 sidecar 和签名清单，纯 WASM 插件的 sidecar 为空且不要求签名清单。
4. 发布任务使用所有插件共享的串行锁，读取 OSS 当前总目录并仅替换本次插件条目；读取失败时直接停止，不允许用空目录继续发布。
5. 先上传 `plugins/<id>/<version>/` 不可变制品及 `plugins-index/releases/<id>.json` 独立入口。
6. 合并后的完整目录按客户端结构、版本、地址和校验值规则验证通过后，保存上一份目录，再更新 `plugins-index/catalog.json`。
7. 从 OSS 回读正式目录并再次验证结构和 SHA-256；失败时立即恢复上一份目录并核对恢复结果。

任何平台构建、单插件目录合并、旧目录下载或制品上传失败都不会执行最后的总目录更新，因此客户端不会发现半发布版本。完整目录包含任何无效插件条目时也会在上传前终止。不同插件可以同时构建，但必须按上述批次限制进入发布阶段；发布阶段串行读取和更新总目录，不会互相覆盖。合并逻辑可在本地对一个插件准备好的平台目录执行：

```bash
TIANGONG_PLUGIN_EXPECTED_PLATFORMS=darwin-aarch64,linux-x86_64,windows-x86_64 \
  cargo run -p xtask -- merge-plugin-dist memory target/plugin-platforms target/plugin-release
```

将该插件独立发布目录合入现有总目录：

```bash
cargo run -p xtask -- merge-plugin-catalog \
  current-catalog.json \
  target/plugin-release/plugins-index/catalog.json \
  catalog.merged.json
```

单独验证一个完整目录：

```bash
cargo run -p xtask -- validate-plugin-catalog catalog.json
```
