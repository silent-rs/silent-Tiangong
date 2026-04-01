//! 权限与安全层
//!
//! 在工具执行前进行权限检查，支持"完全信任"和"监督"两种模式。

use serde::{Deserialize, Serialize};

/// 信任模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustMode {
    /// 完全信任：所有工具自动放行，不弹审批
    #[default]
    FullTrust,
    /// 监督模式：高风险操作需要用户确认
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
#[derive(Debug, Clone, Default)]
pub struct PermissionGate {
    policy: PermissionPolicy,
}

impl PermissionGate {
    pub fn new(policy: PermissionPolicy) -> Self {
        Self { policy }
    }

    /// 对工具调用进行权限检查
    pub fn check(&self, tool_name: &str) -> PermissionDecision {
        // 完全信任模式：直接放行
        if self.policy.trust_mode == TrustMode::FullTrust {
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

    /// 获取当前信任模式
    pub fn trust_mode(&self) -> TrustMode {
        self.policy.trust_mode
    }
}

// PermissionGate 的 Default 由 PermissionPolicy::default() 提供（FullTrust 模式）

/// 根据工具名分类风险等级
fn classify_tool(tool_name: &str) -> PermissionLevel {
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
