# Command Plugin

Command 插件（`run_command` / `run_shell` 基础命令执行，含命令白名单、路径越界、shell 脚本校验）由三个独立 crate 组成：

- `protocol`：WASM 与 sidecar 共用的私有业务协议（含 `CommandAccessContext` 访问能力上下文，为后续沙箱能力预留）；
- `wasm`：工具规格、参数解析、prompt 段落与生命周期入口，桥接 sidecar；
- `sidecar`：tokio 子进程 spawn（kill_on_drop / env_clear / stdio pipe / 超时）、命令校验策略（可替换，为沙箱预留）进程。

通用的清单、WASM、sidecar 和本地导入说明见 [WASM 插件开发指南](../../../docs/plugin-development.md)。

完整插件使用以下命令统一检查、构建和部署：

```bash
cargo run -p xtask -- build-plugin command
```

## 入口分工

- CLI / Server：由 runtime 按 `plugin.json` 的 `entrypoints: ["cli", "server"]` 自动加载。
- GUI：不加载本插件（entrypoints 不含 desktop），`run_command` / `run_shell` 由 terminal 插件（PTY 执行 + 命令回显）提供。

## 沙箱预留

- **命令校验策略可替换**：sidecar 内 `CommandPolicy` trait 抽象了命令/路径校验，当前实现基于信任模式；未来引入命令 AST 化、env 黑名单、网络出口控制时只换实现，业务代码不动。
- **访问能力随请求下发**：`CommandAccessContext`（protocol 层）承载当前会话的 `full_trust` + `workspace` + `allowed_commands`，未来细化权限只扩结构并 bump business-protocol，不动 WIT/wasm。
- **env 注入策略化**：sidecar spawn 子进程时保留 allowlist + runtime_env + file_env 三层注入框架，未来加危险 env key 黑名单（LD_PRELOAD / DYLD_* / BASH_ENV 等）只换策略，注入框架不动。
