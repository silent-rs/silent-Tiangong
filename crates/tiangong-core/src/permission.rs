//! 权限与安全层
//!
//! 在工具执行前进行权限检查，支持"完全信任"和"监督"两种模式。

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// 信任模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustMode {
    /// 完全信任：所有工具自动放行，不弹审批
    FullTrust,
    /// 监督模式：高风险操作需要用户确认
    #[default]
    Supervised,
}

/// 工具风险等级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionLevel {
    /// 安全：只读操作（read_file, list_dir, search_code, tree_dir）
    Safe,
    /// 标准：文件写入操作（write_file, replace_in_file）
    Standard,
    /// 高级：命令执行（run_command）
    Elevated,
    /// 关键：补丁应用、MCP 工具、后台任务
    Critical,
}

/// 路径级权限规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRule {
    /// 路径模式（支持 glob，如 `/etc/**`、`~/.ssh/*`）
    pub pattern: String,
    /// 是否允许访问
    pub allow: bool,
}

/// 网络目标权限规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkRule {
    /// 目标模式（域名或 IP，如 `*.example.com`、`192.168.1.*`）
    pub pattern: String,
    /// 是否允许访问
    pub allow: bool,
}

/// 权限策略配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionPolicy {
    /// 信任模式
    #[serde(default)]
    pub trust_mode: TrustMode,
    /// 始终自动放行的工具名列表
    #[serde(default)]
    pub auto_approve: Vec<String>,
    /// 始终拒绝的工具名列表
    #[serde(default)]
    pub always_deny: Vec<String>,
    /// 路径级规则（按顺序匹配，首条命中生效）
    #[serde(default)]
    pub path_rules: Vec<PathRule>,
    /// 网络目标规则（按顺序匹配，首条命中生效）
    #[serde(default)]
    pub network_rules: Vec<NetworkRule>,
}

/// 权限决策结果
#[derive(Debug, Clone)]
pub enum PermissionDecision {
    /// 允许执行
    Approved,
    /// 拒绝执行
    Denied { reason: String },
    /// 需要用户审批
    NeedsApproval { request_id: String },
}

/// 权限审计记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionAuditEntry {
    pub tool_name: String,
    pub level: String,
    pub decision: String,
    pub timestamp: String,
}

/// 权限检查网关
///
/// trust_mode 通过 `Arc<RwLock>` 共享，允许运行中的任务实时感知权限模式变更。
#[derive(Debug, Clone)]
pub struct PermissionGate {
    policy: PermissionPolicy,
    /// 共享的信任模式引用，clone 后仍指向同一个 RwLock
    shared_trust_mode: Arc<RwLock<TrustMode>>,
}

impl Default for PermissionGate {
    fn default() -> Self {
        let policy = PermissionPolicy::default();
        let shared = Arc::new(RwLock::new(policy.trust_mode));
        Self {
            policy,
            shared_trust_mode: shared,
        }
    }
}

impl PermissionGate {
    pub fn new(policy: PermissionPolicy) -> Self {
        let shared = Arc::new(RwLock::new(policy.trust_mode));
        Self {
            policy,
            shared_trust_mode: shared,
        }
    }

    /// 使用外部共享的信任模式创建（确保所有 clone 共享同一引用）
    pub fn with_shared_trust_mode(
        policy: PermissionPolicy,
        shared: Arc<RwLock<TrustMode>>,
    ) -> Self {
        // 同步初始值
        if let Ok(mut guard) = shared.write() {
            *guard = policy.trust_mode;
        }
        Self {
            policy,
            shared_trust_mode: shared,
        }
    }

    /// 获取共享的信任模式引用（用于跨 RuntimeEngine 实例共享）
    pub fn shared_trust_mode_ref(&self) -> Arc<RwLock<TrustMode>> {
        self.shared_trust_mode.clone()
    }

    /// 实时更新信任模式（所有持有该 Gate clone 的线程立即生效）
    pub fn set_trust_mode(&self, mode: TrustMode) {
        if let Ok(mut guard) = self.shared_trust_mode.write() {
            *guard = mode;
        }
    }

    /// 对工具调用进行权限检查
    pub fn check(&self, tool_name: &str) -> PermissionDecision {
        // 读取共享的信任模式（实时值）
        let trust_mode = self
            .shared_trust_mode
            .read()
            .map(|g| *g)
            .unwrap_or(self.policy.trust_mode);

        // 完全信任模式：直接放行
        if trust_mode == TrustMode::FullTrust {
            return PermissionDecision::Approved;
        }

        // 检查始终拒绝列表
        if self.policy.always_deny.iter().any(|n| n == tool_name) {
            return PermissionDecision::Denied {
                reason: format!("工具 {tool_name} 在拒绝列表中"),
            };
        }

        // 检查始终允许列表
        if self.policy.auto_approve.iter().any(|n| n == tool_name) {
            return PermissionDecision::Approved;
        }

        // 根据工具风险等级决策
        let level = classify_tool(tool_name);
        match level {
            PermissionLevel::Safe => PermissionDecision::Approved,
            PermissionLevel::Standard => PermissionDecision::Approved,
            PermissionLevel::Elevated | PermissionLevel::Critical => {
                PermissionDecision::NeedsApproval {
                    request_id: scru128::new().to_string(),
                }
            }
        }
    }

    /// 获取当前信任模式（实时值）
    pub fn trust_mode(&self) -> TrustMode {
        self.shared_trust_mode
            .read()
            .map(|g| *g)
            .unwrap_or(self.policy.trust_mode)
    }

    /// 检查路径访问权限
    ///
    /// 按路径规则顺序匹配，首条命中生效。
    /// 无匹配规则时默认允许。
    pub fn check_path(&self, path: &str) -> PermissionDecision {
        let trust_mode = self
            .shared_trust_mode
            .read()
            .map(|g| *g)
            .unwrap_or(self.policy.trust_mode);

        if trust_mode == TrustMode::FullTrust {
            return PermissionDecision::Approved;
        }

        for rule in &self.policy.path_rules {
            if path_matches(&rule.pattern, path) {
                return if rule.allow {
                    PermissionDecision::Approved
                } else {
                    PermissionDecision::Denied {
                        reason: format!("路径 {path} 被规则 {} 拒绝", rule.pattern),
                    }
                };
            }
        }

        PermissionDecision::Approved
    }

    /// 检查网络目标访问权限
    ///
    /// 按网络规则顺序匹配，首条命中生效。
    /// 无匹配规则时默认允许。
    pub fn check_network(&self, target: &str) -> PermissionDecision {
        let trust_mode = self
            .shared_trust_mode
            .read()
            .map(|g| *g)
            .unwrap_or(self.policy.trust_mode);

        if trust_mode == TrustMode::FullTrust {
            return PermissionDecision::Approved;
        }

        for rule in &self.policy.network_rules {
            if network_matches(&rule.pattern, target) {
                return if rule.allow {
                    PermissionDecision::Approved
                } else {
                    PermissionDecision::Denied {
                        reason: format!("网络目标 {target} 被规则 {} 拒绝", rule.pattern),
                    }
                };
            }
        }

        PermissionDecision::Approved
    }
}

// PermissionGate 的 Default 由 PermissionPolicy::default() 提供（FullTrust 模式）

/// 简单路径模式匹配（支持 `*` 通配符和 `**` 递归匹配）
fn path_matches(pattern: &str, path: &str) -> bool {
    if pattern == path {
        return true;
    }
    // /** 匹配任意子路径
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path.starts_with(prefix);
    }
    // /* 匹配单层
    if let Some(prefix) = pattern.strip_suffix("/*")
        && let Some(rest) = path.strip_prefix(prefix)
    {
        return rest.starts_with('/') && !rest[1..].contains('/');
    }
    false
}

/// 简单网络模式匹配（支持 `*` 通配符）
fn network_matches(pattern: &str, target: &str) -> bool {
    if pattern == target {
        return true;
    }
    // *.example.com 匹配 sub.example.com
    if let Some(suffix) = pattern.strip_prefix('*') {
        return target.ends_with(suffix) && target.len() > suffix.len();
    }
    // 192.168.1.* 匹配 192.168.1.100
    if let Some(prefix) = pattern.strip_suffix(".*")
        && let Some(rest) = target.strip_prefix(prefix)
    {
        return rest.starts_with('.');
    }
    false
}

/// 根据工具名分类风险等级（pub(crate) 供测试访问）
pub(crate) fn classify_tool(tool_name: &str) -> PermissionLevel {
    match tool_name {
        // 安全：只读
        "read_file" | "list_dir" | "tree_dir" | "search_code" | "get_skill_detail" => {
            PermissionLevel::Safe
        }
        // 标准：文件写入
        "write_file" | "replace_in_file" => PermissionLevel::Standard,
        // 高级：命令执行
        "run_command" | "run_shell" => PermissionLevel::Elevated,
        // 关键：补丁、后台任务、多媒体
        "apply_patch" | "spawn_task" | "cancel_task" => PermissionLevel::Critical,
        // MCP 工具和未知工具默认为关键
        _ => PermissionLevel::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_trust_approves_everything() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::FullTrust,
            ..Default::default()
        });
        assert!(matches!(
            gate.check("run_command"),
            PermissionDecision::Approved
        ));
        assert!(matches!(
            gate.check("apply_patch"),
            PermissionDecision::Approved
        ));
        assert!(matches!(
            gate.check("some_mcp_tool"),
            PermissionDecision::Approved
        ));
    }

    #[test]
    fn supervised_approves_safe_tools() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            ..Default::default()
        });
        assert!(matches!(
            gate.check("read_file"),
            PermissionDecision::Approved
        ));
        assert!(matches!(
            gate.check("list_dir"),
            PermissionDecision::Approved
        ));
        assert!(matches!(
            gate.check("search_code"),
            PermissionDecision::Approved
        ));
        assert!(matches!(
            gate.check("write_file"),
            PermissionDecision::Approved
        ));
    }

    #[test]
    fn supervised_requires_approval_for_elevated() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            ..Default::default()
        });
        assert!(matches!(
            gate.check("run_command"),
            PermissionDecision::NeedsApproval { .. }
        ));
        assert!(matches!(
            gate.check("run_shell"),
            PermissionDecision::NeedsApproval { .. }
        ));
        assert!(matches!(
            gate.check("apply_patch"),
            PermissionDecision::NeedsApproval { .. }
        ));
        assert!(matches!(
            gate.check("unknown_mcp_tool"),
            PermissionDecision::NeedsApproval { .. }
        ));
    }

    #[test]
    fn always_deny_overrides_trust() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            always_deny: vec!["read_file".to_string()],
            ..Default::default()
        });
        assert!(matches!(
            gate.check("read_file"),
            PermissionDecision::Denied { .. }
        ));
    }

    #[test]
    fn auto_approve_overrides_level() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            auto_approve: vec!["run_command".to_string()],
            ..Default::default()
        });
        assert!(matches!(
            gate.check("run_command"),
            PermissionDecision::Approved
        ));
    }

    #[test]
    fn deny_takes_priority_over_approve() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            auto_approve: vec!["run_command".to_string()],
            always_deny: vec!["run_command".to_string()],
            ..Default::default()
        });
        // deny 先检查，优先于 approve
        assert!(matches!(
            gate.check("run_command"),
            PermissionDecision::Denied { .. }
        ));
    }

    #[test]
    fn classify_tool_levels() {
        assert_eq!(classify_tool("read_file"), PermissionLevel::Safe);
        assert_eq!(classify_tool("tree_dir"), PermissionLevel::Safe);
        assert_eq!(classify_tool("write_file"), PermissionLevel::Standard);
        assert_eq!(classify_tool("replace_in_file"), PermissionLevel::Standard);
        assert_eq!(classify_tool("run_command"), PermissionLevel::Elevated);
        assert_eq!(classify_tool("apply_patch"), PermissionLevel::Critical);
        assert_eq!(classify_tool("spawn_task"), PermissionLevel::Critical);
        assert_eq!(classify_tool("unknown_tool"), PermissionLevel::Critical);
    }

    #[test]
    fn default_gate_is_supervised() {
        let gate = PermissionGate::default();
        assert_eq!(gate.trust_mode(), TrustMode::Supervised);
        assert!(matches!(
            gate.check("apply_patch"),
            PermissionDecision::NeedsApproval { .. }
        ));
    }

    #[test]
    fn trust_mode_serialization() {
        let json = serde_json::to_string(&TrustMode::Supervised).unwrap();
        assert_eq!(json, r#""supervised""#);
        let parsed: TrustMode = serde_json::from_str(r#""full_trust""#).unwrap();
        assert_eq!(parsed, TrustMode::FullTrust);
    }

    #[test]
    fn policy_serialization_roundtrip() {
        let policy = PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            auto_approve: vec!["read_file".into()],
            always_deny: vec!["apply_patch".into()],
            ..Default::default()
        };
        let json = serde_json::to_string(&policy).unwrap();
        let parsed: PermissionPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.trust_mode, TrustMode::Supervised);
        assert_eq!(parsed.auto_approve, vec!["read_file"]);
        assert_eq!(parsed.always_deny, vec!["apply_patch"]);
    }

    #[test]
    fn path_rule_deny() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            path_rules: vec![PathRule {
                pattern: "/etc/**".into(),
                allow: false,
            }],
            ..Default::default()
        });
        assert!(matches!(
            gate.check_path("/etc/passwd"),
            PermissionDecision::Denied { .. }
        ));
        assert!(matches!(
            gate.check_path("/home/user/file.txt"),
            PermissionDecision::Approved
        ));
    }

    #[test]
    fn path_rule_single_level() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            path_rules: vec![PathRule {
                pattern: "/tmp/*".into(),
                allow: false,
            }],
            ..Default::default()
        });
        assert!(matches!(
            gate.check_path("/tmp/file.txt"),
            PermissionDecision::Denied { .. }
        ));
        // 子目录不匹配 /*
        assert!(matches!(
            gate.check_path("/tmp/sub/file.txt"),
            PermissionDecision::Approved
        ));
    }

    #[test]
    fn network_rule_deny_domain() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            network_rules: vec![NetworkRule {
                pattern: "*.evil.com".into(),
                allow: false,
            }],
            ..Default::default()
        });
        assert!(matches!(
            gate.check_network("sub.evil.com"),
            PermissionDecision::Denied { .. }
        ));
        assert!(matches!(
            gate.check_network("good.com"),
            PermissionDecision::Approved
        ));
    }

    #[test]
    fn network_rule_deny_ip_range() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::Supervised,
            network_rules: vec![NetworkRule {
                pattern: "10.0.0.*".into(),
                allow: false,
            }],
            ..Default::default()
        });
        assert!(matches!(
            gate.check_network("10.0.0.1"),
            PermissionDecision::Denied { .. }
        ));
        assert!(matches!(
            gate.check_network("192.168.1.1"),
            PermissionDecision::Approved
        ));
    }

    #[test]
    fn full_trust_bypasses_path_rules() {
        let gate = PermissionGate::new(PermissionPolicy {
            trust_mode: TrustMode::FullTrust,
            path_rules: vec![PathRule {
                pattern: "/etc/**".into(),
                allow: false,
            }],
            ..Default::default()
        });
        assert!(matches!(
            gate.check_path("/etc/passwd"),
            PermissionDecision::Approved
        ));
    }
}
