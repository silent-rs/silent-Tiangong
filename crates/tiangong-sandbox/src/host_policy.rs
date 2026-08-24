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

/// 单个插件的宿主侧执行策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecutionPolicy {
    /// sidecar 进程是否进 OS 沙箱（继承式，要求 stdio 传输）。
    pub sandbox: bool,
    /// 沙箱内是否放行网络（仅文件写白名单之外的网络能力）。
    pub allow_network: bool,
}

/// 未知 / 未签名插件的保守默认：进沙箱、断网。
pub fn conservative_default() -> HostExecutionPolicy {
    HostExecutionPolicy {
        sandbox: true,
        allow_network: false,
    }
}

/// 需要网络能力的官方插件（网络白名单随天工发行维护）。
const OFFICIAL_NETWORKED: &[&str] = &[
    "fetch",                 // HTTP 抓取是插件本体功能
    "mcp",                   // 连接外部 MCP server
    "scheduler",             // 回调宿主 server
    "generate-image-openai", // 直连 OpenAI 兼容端点
];

/// 不进沙箱的官方特殊载体（依赖平台 API 或自身即沙箱载体）。
const OFFICIAL_UNSANDBOXED: &[&str] = &[
    "terminal",         // PTY 交互载体，独立沙箱开关
    "computer-use",     // 系统 UIA/AX/AT-SPI 自动化
    "screenshot-input", // 平台截图 API
    "fs",               // 动态路径，待 invoke 层检查配套（RFC D12）
    "command",          // 命令级沙箱载体，避免双层沙箱冲突
    "memory",           // recall 流式依赖 TCP 连接对象，stdio 适配待改造
];

/// 解析插件的宿主执行策略。
///
/// `official_signed`：插件是否携带有效的官方签名（publisher 为
/// tiangong-official）。官方插件查内置表；其余一律保守默认。
pub fn resolve(plugin_id: &str, official_signed: bool) -> HostExecutionPolicy {
    if !official_signed {
        return conservative_default();
    }
    if OFFICIAL_UNSANDBOXED.contains(&plugin_id) {
        return HostExecutionPolicy {
            sandbox: false,
            allow_network: false,
        };
    }
    HostExecutionPolicy {
        sandbox: true,
        allow_network: OFFICIAL_NETWORKED.contains(&plugin_id),
    }
}

/// 策略表快照（审计与测试用）。
pub fn catalog_snapshot() -> BTreeMap<String, HostExecutionPolicy> {
    let mut out = BTreeMap::new();
    for id in OFFICIAL_UNSANDBOXED {
        out.insert(
            (*id).to_string(),
            HostExecutionPolicy {
                sandbox: false,
                allow_network: false,
            },
        );
    }
    for id in OFFICIAL_NETWORKED {
        out.insert(
            (*id).to_string(),
            HostExecutionPolicy {
                sandbox: true,
                allow_network: true,
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_plugins_always_get_conservative_default() {
        // 未签名插件即使伪造 manifest 声明，也拿不到网络。
        let policy = resolve("fetch", false);
        assert!(policy.sandbox);
        assert!(!policy.allow_network);
    }

    #[test]
    fn official_networked_plugins_get_network() {
        let policy = resolve("fetch", true);
        assert!(policy.sandbox);
        assert!(policy.allow_network);
    }

    #[test]
    fn official_plain_plugins_are_sandboxed_offline() {
        let policy = resolve("memory-index-like", true);
        assert!(policy.sandbox);
        assert!(!policy.allow_network);
        let index = resolve("index", true);
        assert!(index.sandbox);
        assert!(!index.allow_network);
    }

    #[test]
    fn special_carriers_stay_unsandboxed() {
        for id in ["terminal", "fs", "command", "computer-use"] {
            let policy = resolve(id, true);
            assert!(!policy.sandbox, "{id} 应保持非沙箱载体");
        }
    }
}
