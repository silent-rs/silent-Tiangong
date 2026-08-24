//! 宿主权威执行策略表（RFC 0017 审查修订：透明执行封套）。
//!
//! 策略权威反转：sidecar 的沙箱与网络能力**不读插件 manifest**（插件自声明
//! 构成提权通道），由宿主按插件身份查表——
//!
//! ```text
//! 宿主策略表 ∩ 用户授权（信任通道） = 有效范围
//! ```
//!
//! 优先级：
//! 1. 官方签名插件的宿主策略表（内置，随天工发行）
//! 2. 未签名插件：保守默认（数据目录可写、断网），可被 L3/L4 信任授权
//!    收紧或后续经用户授权扩展
//!
//! manifest 的 `sandbox` / `sandbox_network` 字段保留为开发提示语义，
//! 不参与安全决策。

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

/// 未知 / 未签名插件的保守默认：进沙箱、断网、管道通信。
pub fn conservative_default() -> HostExecutionPolicy {
    HostExecutionPolicy {
        sandbox: true,
        allow_network: false,
        transport: SidecarTransport::Stdio,
    }
}

/// 任务边界收敛（review 修订）：本表只治理 command（唯一沙箱化的命令通道）。
/// 其它官方插件（fetch/mcp/scheduler/memory/fs/terminal 等）的沙箱、网络与
/// transport 逐批迁移决策归 feature/sidecar-sandbox-migration 分支，当前一律
/// 按存量默认（TCP、不沙箱）运行，避免本分支强制存量 sidecar 走 stdio。
/// 解析插件的宿主执行策略。
///
/// 收敛语义：command 的执行操作走宿主一次性沙箱实例（`invoke_command_ephemeral`，
/// 常驻连接同样按沙箱处理）；未签名插件保守默认（进沙箱、断网、管道）；
/// 其余官方插件按存量默认（TCP、不沙箱）运行。
pub fn resolve(plugin_id: &str, official_signed: bool) -> HostExecutionPolicy {
    if !official_signed {
        return conservative_default();
    }
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
        .map(|id| ((*id).to_string(), resolve(id, true)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_plugins_get_conservative_default() {
        let policy = resolve("anything", false);
        assert!(policy.sandbox);
        assert!(!policy.allow_network);
        assert_eq!(policy.transport, SidecarTransport::Stdio);
    }

    #[test]
    fn command_is_the_only_sandboxed_official_plugin() {
        let policy = resolve("command", true);
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
            let policy = resolve(id, true);
            assert!(!policy.sandbox, "{id} 本分支不沙箱化");
            assert_eq!(policy.transport, SidecarTransport::Tcp, "{id} 保持存量 TCP");
        }
    }
}
