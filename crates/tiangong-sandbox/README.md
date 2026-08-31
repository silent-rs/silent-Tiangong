# tiangong-sandbox

`tiangong-sandbox` 是一个可独立使用的操作系统沙箱 Launcher。调用方提供策略和目标命令，Launcher 验证请求后使用当前平台的隔离能力启动目标进程；隔离能力不可用、策略非法或目标不可信时会明确拒绝执行，不会退回普通进程。

## 平台实现

- macOS：Seatbelt（`sandbox-exec`）
- Linux：bubblewrap（`bwrap`）
- Windows：AppContainer 与 Job Object

目标进程创建的子进程会继承相同约束。不同平台的底层能力有所区别，但策略入口和失败关闭原则保持一致。

## 安装目录

Sandbox 不决定宿主的存储布局。安装目录由 App、服务或其他调用方选择，目录内文件名约定为：

```text
<宿主选择的目录>/tiangong-sandbox[.exe]
<宿主选择的目录>/tiangong-sandbox[.exe].sig
```

库提供程序定位、签名验证和自检能力，但不会自动拼接天工的存储根或 `sandbox` 目录。

## 快速使用

### 直接在命令行中设置策略

```bash
tiangong-sandbox run \
  --mode workspace-write \
  --workspace /absolute/workspace \
  --writable /absolute/run-temp \
  --protect /absolute/workspace/protected \
  --deny-read /home/user/.ssh \
  --network deny \
  --max-cpu-seconds 300 \
  --max-memory-bytes 2147483648 \
  --max-processes 64 \
  -- command arg1 arg2
```

参数说明：

- `--mode`：`read-only` 或 `workspace-write`，默认 `workspace-write`；不允许 `full-access`。
- `--workspace`：主工作区，直接策略形式必须提供绝对路径。
- `--writable`：额外可写绝对路径，可以重复。
- `--protect`：即使位于可写范围内也保持只读的绝对路径，可以重复。
- `--deny-read`：禁止读取的绝对路径，可以重复。
- `--network`：`allow` 或 `deny`，默认 `deny`。
- `--max-cpu-seconds`：CPU 时间上限，必须大于零。
- `--max-memory-bytes`：内存上限，必须大于零。
- `--max-processes`：进程数量上限，必须大于零。
- `--`：策略参数与目标命令的分隔符。

未设置资源参数时，默认限制为 300 秒 CPU 时间、2 GiB 内存和 64 个进程。

### 使用策略文件

```bash
tiangong-sandbox run --policy /absolute/policy.json -- command arg1 arg2
```

`--policy` 不能与直接策略参数混用。策略文件使用 `SandboxPolicy` JSON：

```json
{
  "mode": "workspace_write",
  "workspace": "/absolute/workspace",
  "extra_writable": ["/absolute/run-temp"],
  "protected_paths": ["/absolute/workspace/protected"],
  "denied_read_paths": ["/home/user/.ssh"],
  "allow_network": false,
  "resource_limits": {
    "max_cpu_time_seconds": 300,
    "max_memory_bytes": 2147483648,
    "max_processes": 64
  }
}
```

## 默认安全行为

- 网络默认禁止。
- `workspace-write` 只允许写入工作区和显式指定的额外可写路径。
- `read-only` 不提供可写根。
- `full-access` 请求会被 Launcher 拒绝。
- 命令、工作区及策略路径必须通过形态和绝对路径检查。
- 目标程序会校验普通文件形态、摘要和平台权限。
- 平台沙箱不可用时拒绝执行，不静默降级。
- 敏感路径不会由通用核心隐式猜测；宿主应通过 `protected_paths` 和 `denied_read_paths` 明确传入。crate 另提供可选的常见凭据与天工宿主预设。

## 宿主协议

除 `run` 命令外，宿主也可以使用 Launcher 请求协议：

- Unix：通过继承的文件描述符 3 传入长度前缀 JSON 帧。
- Windows：通过一次性环境信封传入请求，并可关联宿主进程和停止事件。

请求包含独立的协议版本和策略 Schema。版本不兼容时 Launcher 会拒绝启动。目标校验默认使用路径、摘要和权限；需要天工插件清单比对时，宿主可以显式选择对应校验方式。

## 签名、自检与更新

检查当前平台能力和协议信息：

```bash
tiangong-sandbox --self-check
```

检查和安装官方更新：

```bash
tiangong-sandbox check-update
tiangong-sandbox update
tiangong-sandbox update --root /absolute/install-directory
```

默认更新链使用 crate 内置的官方公钥。第三方宿主可以显式提供自己的 HTTPS 更新清单和 minisign 公钥，但环境变量不能静默替换信任根。天工生产验证入口始终只接受内置官方公钥，并在验签后执行真实自检。

## 构建与验证

在仓库根目录执行：

```bash
cargo build --locked -p tiangong-sandbox --bin tiangong-sandbox
cargo check --locked -p tiangong-sandbox --all-targets
cargo test --locked -p tiangong-sandbox
cargo clippy --locked -p tiangong-sandbox --all-targets -- -D warnings
```

真实隔离测试需要当前环境允许应用对应平台的沙箱。已经处于受限沙箱中的开发环境可能无法再次嵌套，此时 Launcher 会报告平台不可用并拒绝执行。

## 设计资料

- [Sandbox 通用化与宿主安装目录](../../docs/sandbox-direct-storage.md)
- [Sandbox 自管理与首版发布](../../docs/sandbox-self-management.md)

## 许可证

Apache-2.0
