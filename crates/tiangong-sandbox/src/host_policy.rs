//! command 通道的宿主权威执行策略。
//!
//! 只有已通过官方签名启动门槛的 sidecar 会进入此处。当前分支只改变
//! command：强制 stdio、强制沙箱、禁止网络；其它插件保持原有 TCP 路径。

use std::collections::BTreeMap;

/// sidecar 与宿主的通信通道（宿主权威决定，插件不声明——spawn 时由
/// 宿主注入环境变量选择，sidecar 通用库自动适配）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarTransport {
    /// 继承管道：沙箱零网络放行、无监听端口、生命周期与宿主绑定。
    Stdio,
    /// 本地回环 + endpoint 文件（存量默认；流式召回等依赖连接对象）。
    Tcp,
}

/// 单个插件的宿主侧执行策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecutionPolicy {
    /// sidecar 进程是否进 OS 沙箱（继承式，要求 stdio 传输）。
    pub sandbox: bool,
    /// 沙箱内是否放行网络（仅文件写白名单之外的网络能力）。
    pub allow_network: bool,
    /// 与宿主的通信通道。
    pub transport: SidecarTransport,
}

/// 解析插件的宿主执行策略。
pub fn resolve(plugin_id: &str) -> HostExecutionPolicy {
    if plugin_id == "command" {
        return HostExecutionPolicy {
            sandbox: true,
            allow_network: false,
            transport: SidecarTransport::Stdio,
        };
    }
    HostExecutionPolicy {
        sandbox: false,
        allow_network: false,
        transport: SidecarTransport::Tcp,
    }
}

/// 策略表快照（审计与测试用）。
pub fn catalog_snapshot() -> BTreeMap<String, HostExecutionPolicy> {
    ["command"]
        .iter()
        .map(|id| ((*id).to_string(), resolve(id)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_the_only_sandboxed_official_plugin() {
        let policy = resolve("command");
        assert!(policy.sandbox);
        assert_eq!(policy.transport, SidecarTransport::Stdio);
        // 其它官方插件按存量默认（TCP、不沙箱），逐批迁移归独立分支。
        for id in [
            "fetch",
            "mcp",
            "scheduler",
            "memory",
            "fs",
            "terminal",
            "index",
        ] {
            let policy = resolve(id);
            assert!(!policy.sandbox, "{id} 本分支不沙箱化");
            assert_eq!(policy.transport, SidecarTransport::Tcp, "{id} 保持存量 TCP");
        }
    }
}
