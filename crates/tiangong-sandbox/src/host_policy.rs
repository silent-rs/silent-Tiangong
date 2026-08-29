//! sidecar 运行时的宿主权威执行策略。
//!
//! 分层隔离：WASM handler 在宿主进程内由 wasmtime 能力沙箱隔离（进程内
//! 无法再套 OS 沙箱）；所有 sidecar 进程一律经 Launcher 进 OS 沙箱。
//!
//! 不变式（沙箱不进行 agent 执行过程中升级）：
//! - 策略是纯函数，仅在连接构造时求值一次并定死，不读任何全局可变
//!   状态；插件清单与 agent 运行时的任何声明/请求都不构成提权输入。
//! - 运行中的连接不升级也不降级；策略变更只可能来自用户设置，且在
//!   下一次连接构造时生效。
//! - ephemeral 形态（command）每次请求构造独立快照与独立沙箱实例，
//!   属"每请求隔离"而非升级。
//!
//! 网络维度：沙箱开启即视为执行安全，网络全放行——沙箱的安全边界是
//! 文件域（工作区写白名单 + 凭据禁读）与资源上限；凭据目录不可读，
//! 网络开放也不会外传密钥类数据。

use std::collections::BTreeMap;

/// sidecar 与宿主的通信通道（宿主权威决定，插件不声明——spawn 时由
/// 宿主注入环境变量选择，sidecar 通用库自动适配）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarTransport {
    /// 继承管道：沙箱零监听端口、生命周期与宿主绑定。
    Stdio,
    /// 本地回环 + endpoint 文件（存量默认；流式召回等依赖连接对象）。
    Tcp,
}

/// 单个插件的宿主侧执行策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecutionPolicy {
    /// sidecar 进程是否进 OS 沙箱（继承式，要求 stdio 传输）。
    pub sandbox: bool,
    /// 沙箱内是否放行网络（沙箱开启时恒为 true，见模块说明）。
    pub allow_network: bool,
    /// 与宿主的通信通道。
    pub transport: SidecarTransport,
}

/// 解析插件的宿主执行策略。
///
/// `sandbox_disabled` 为用户全局设置（设置页"插件沙箱"开关）：关闭时
/// 所有 sidecar 回到无沙箱直跑与存量 TCP 传输——退出沙箱的唯一途径
/// 是用户主动关闭。开关由宿主读取配置传入，策略表本身不读全局状态。
pub fn resolve(_plugin_id: &str, sandbox_disabled: bool) -> HostExecutionPolicy {
    if sandbox_disabled {
        return HostExecutionPolicy {
            sandbox: false,
            allow_network: false,
            transport: SidecarTransport::Tcp,
        };
    }
    HostExecutionPolicy {
        sandbox: true,
        allow_network: true,
        transport: SidecarTransport::Stdio,
    }
}

/// 策略表快照（审计与测试用）；沙箱开关默认开启。
pub fn catalog_snapshot() -> BTreeMap<String, HostExecutionPolicy> {
    [
        "command",
        "fetch",
        "mcp",
        "scheduler",
        "memory",
        "fs",
        "terminal",
        "index",
    ]
    .iter()
    .map(|id| ((*id).to_string(), resolve(id, false)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_plugins_are_sandboxed_with_stdio_by_default() {
        for id in [
            "command",
            "fetch",
            "mcp",
            "scheduler",
            "memory",
            "fs",
            "terminal",
            "index",
            "unknown-third-party",
        ] {
            let policy = resolve(id, false);
            assert!(policy.sandbox, "{id} 默认必须进沙箱");
            assert_eq!(
                policy.transport,
                SidecarTransport::Stdio,
                "{id} 沙箱形态走 stdio"
            );
            assert!(policy.allow_network, "{id} 沙箱开启即网络全放行");
        }
    }

    #[test]
    fn user_setting_disables_all_sandboxes() {
        for id in ["command", "fetch", "fs"] {
            let policy = resolve(id, true);
            assert!(!policy.sandbox, "{id} 用户关闭后直跑");
            assert_eq!(policy.transport, SidecarTransport::Tcp, "{id} 回存量 TCP");
        }
    }

    #[test]
    fn policy_is_pure_and_stable() {
        // 不变式：同一输入恒返回同一策略（无隐藏全局状态），策略仅由
        // 插件 id 与用户开关决定——agent 执行过程中不存在升级输入。
        for id in ["command", "fetch", "mcp"] {
            assert_eq!(resolve(id, false), resolve(id, false));
            assert_eq!(resolve(id, true), resolve(id, true));
        }
    }
}
