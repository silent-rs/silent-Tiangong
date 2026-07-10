//! 权限与安全层
//!
//! 在工具执行前进行权限检查，支持"完全信任"和"监督"两种模式。

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// 信任模式
///
/// 定义已下沉至 [`tiangong_types::TrustMode`]，此处 re-export 保持
/// `tiangong_core::permission::TrustMode` 路径稳定，core 内部用法无需改动。
pub use tiangong_types::TrustMode;

/// 工具风险等级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionLevel {
    /// 安全：只读操作（read_file, list_dir, search_code, tree_dir）
    Safe,
    /// 标准：文件写入操作（write_file, replace_in_file）
    Standard,
    /// 高级：命令执行（run_command）
    Elevated,
    /// 关键：补丁应用、动态工具、后台任务
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
    /// 插件贡献的工具权限覆盖表（由 core/mod.rs 汇总各插件的
    /// `tool_permission_overrides` 写入）。check 时优先于 classify_tool 查询。
    plugin_overrides: Arc<RwLock<std::collections::BTreeMap<String, PermissionLevel>>>,
}

impl Default for PermissionGate {
    fn default() -> Self {
        let policy = PermissionPolicy::default();
        let shared = Arc::new(RwLock::new(policy.trust_mode));
        Self {
            policy,
            shared_trust_mode: shared,
            plugin_overrides: Arc::new(RwLock::new(std::collections::BTreeMap::new())),
        }
    }
}

impl PermissionGate {
    pub fn new(policy: PermissionPolicy) -> Self {
        let shared = Arc::new(RwLock::new(policy.trust_mode));
        Self {
            policy,
            shared_trust_mode: shared,
            plugin_overrides: Arc::new(RwLock::new(std::collections::BTreeMap::new())),
        }
    }

    /// 使用外部共享的信任模式创建（确保所有持有该 Gate clone 共享同一引用）
    pub fn with_shared_trust_mode(
        policy: PermissionPolicy,
        shared: Arc<RwLock<TrustMode>>,
    ) -> Self {
        Self {
            policy,
            shared_trust_mode: shared,
            plugin_overrides: Arc::new(RwLock::new(std::collections::BTreeMap::new())),
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

    /// 写入插件贡献的工具权限覆盖表（供 core/mod.rs 汇总插件能力时调用）。
    pub fn set_plugin_overrides(
        &self,
        overrides: std::collections::BTreeMap<String, PermissionLevel>,
    ) {
        if let Ok(mut guard) = self.plugin_overrides.write() {
            *guard = overrides;
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

        // 根据工具风险等级决策：优先查插件贡献的覆盖表，未命中再走 classify_tool。
        let level = if let Ok(overrides) = self.plugin_overrides.read() {
            overrides
                .get(tool_name)
                .copied()
                .unwrap_or_else(|| classify_tool(tool_name))
        } else {
            classify_tool(tool_name)
        };
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
///
/// 仅维护 core 内置/通用工具的风险等级。各插件工具的权限等级由插件经
/// [`crate::core::Plugin::tool_permission_overrides`] 贡献，PermissionGate.check
/// 优先查覆盖表，未命中才走此函数。
pub(crate) fn classify_tool(tool_name: &str) -> PermissionLevel {
    match tool_name {
        // 安全：只读（core 内置工具）
        "read_file" | "list_dir" | "tree_dir" | "search_code" | "web_fetch" | "current_time" => {
            PermissionLevel::Safe
        }
        // 标准：文件写入
        "write_file" | "replace_in_file" => PermissionLevel::Standard,
        // 高级：命令执行 + 浏览器操作
        "run_command" | "run_shell" | "terminal_send" | "web_form_fill" | "web_click"
        | "web_load_html" => PermissionLevel::Elevated,
        // 关键：补丁应用（spawn_task/cancel_task 等后台任务工具已由
        // tiangong-plugin-task 经 tool_permission_overrides 声明，不再在此中心化维护）
        "apply_patch" => PermissionLevel::Critical,
        // 未知工具默认为关键
        _ => PermissionLevel::Critical,
    }
}

fn normalize_path_target(session: &crate::session::Session, target: &str) -> String {
    let path = std::path::PathBuf::from(target);
    if path.is_absolute() {
        return path.to_string_lossy().to_string();
    }

    let base = if !session.cwd.is_empty() {
        std::path::PathBuf::from(&session.cwd)
    } else {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    };

    base.join(path).to_string_lossy().to_string()
}

fn normalize_network_target(target: &str) -> String {
    let trimmed = target.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
        .split('@')
        .next_back()
        .unwrap_or(without_scheme)
        .split(':')
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

// ── 审计辅助函数（从 core 迁入） ──

/// 格式化工具调用参数摘要（用于 ToolStart 和 ApprovalNeeded 事件）
pub(crate) fn format_call_args_summary(call: &crate::model::ToolCall) -> String {
    use serde_json::Value;

    let args = &call.arguments;
    if !args.is_object() || args.as_object().is_none_or(|m| m.is_empty()) {
        return String::new();
    }

    if call.name == "run_command" || call.name == "run_shell" {
        if let Some(cmd) = args.get("command").and_then(Value::as_str) {
            return cmd.to_string();
        }
        if let Some(script) = args.get("script").and_then(Value::as_str) {
            let shell = args.get("shell").and_then(Value::as_str).unwrap_or("auto");
            return format!("[{shell}] {script}");
        }
    }

    if call.name == "write_file"
        && let Some(path) = args.get("path").and_then(Value::as_str)
    {
        let len = args
            .get("content")
            .and_then(Value::as_str)
            .map(|c| c.len())
            .unwrap_or(0);
        return format!("{path} ({len} bytes)");
    }

    if call.name == "recall_memory"
        && let Some(query) = args.get("query").and_then(Value::as_str)
    {
        return query.to_string();
    }

    if (call.name == "read_file" || call.name == "list_directory")
        && let Some(path) = args.get("path").and_then(Value::as_str)
    {
        return path.to_string();
    }

    if call.name == "web_form_fill" {
        let selector = args.get("selector").and_then(Value::as_str).unwrap_or("");
        let value = args.get("value").and_then(Value::as_str).unwrap_or("");
        return format!("{selector} = {value}");
    }
    if call.name == "web_click" {
        let selector = args.get("selector").and_then(Value::as_str).unwrap_or("");
        return format!("click {selector}");
    }

    let obj = args.as_object().unwrap();
    obj.iter()
        .map(|(k, v)| {
            let val = match v {
                Value::String(s) if s.chars().count() > 80 => {
                    let truncated: String = s.chars().take(77).collect();
                    format!("{truncated}...")
                }
                Value::String(s) => s.clone(),
                other => {
                    let s = other.to_string();
                    if s.chars().count() > 80 {
                        let truncated: String = s.chars().take(77).collect();
                        format!("{truncated}...")
                    } else {
                        s
                    }
                }
            };
            format!("{k}={val}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 从工具调用中推断审计目标（scope, summary）
pub(crate) fn infer_audit_target(
    call: &crate::model::ToolCall,
) -> (Option<String>, Option<String>) {
    use serde_json::Value;

    let Some(obj) = call.arguments.as_object() else {
        return infer_tool_name_scope(call.name.as_str(), None);
    };

    if call.name == "web_fetch"
        && let Some(value) = obj.get("url").and_then(Value::as_str).map(str::trim)
        && !value.is_empty()
    {
        return (Some("network".to_string()), Some(value.to_string()));
    }

    let path_keys = [
        "path",
        "file_path",
        "output_path",
        "cwd",
        "directory",
        "dir",
        "target_path",
        "workspace_path",
    ];
    for key in path_keys {
        if let Some(value) = obj.get(key).and_then(Value::as_str).map(str::trim)
            && !value.is_empty()
        {
            return (Some("path".to_string()), Some(value.to_string()));
        }
    }

    let network_keys = ["url", "endpoint", "host", "domain", "base_url"];
    for key in network_keys {
        if let Some(value) = obj.get(key).and_then(Value::as_str).map(str::trim)
            && !value.is_empty()
        {
            return (Some("network".to_string()), Some(value.to_string()));
        }
    }

    if let Some(value) = obj.get("task_id").and_then(Value::as_str).map(str::trim)
        && !value.is_empty()
    {
        return (Some("task".to_string()), Some(value.to_string()));
    }
    if let Some(values) = obj.get("task_ids").and_then(Value::as_array)
        && !values.is_empty()
    {
        let joined = values
            .iter()
            .filter_map(Value::as_str)
            .take(3)
            .collect::<Vec<_>>()
            .join(",");
        if !joined.is_empty() {
            return (Some("task".to_string()), Some(joined));
        }
    }

    if let Some(value) = obj.get("command").and_then(Value::as_str).map(str::trim)
        && !value.is_empty()
    {
        return (Some("command".to_string()), Some(value.to_string()));
    }
    if let Some(value) = obj.get("script").and_then(Value::as_str).map(str::trim)
        && !value.is_empty()
    {
        return (Some("command".to_string()), Some(value.to_string()));
    }

    infer_tool_name_scope(call.name.as_str(), None)
}

fn infer_tool_name_scope(
    tool_name: &str,
    summary: Option<String>,
) -> (Option<String>, Option<String>) {
    let scope = match tool_name {
        "read_file" | "write_file" | "replace_in_file" | "list_dir" | "tree_dir" => "path",
        "web_fetch" => "network",
        "analyze_attachment" | "generate_image" | "speech_to_text" | "text_to_speech" => "external",
        "run_command" | "run_shell" | "terminal_send" => "command",
        "web_form_extract" | "web_form_fill" | "web_click" | "web_load_html" => "browser",
        _ => return (None, summary),
    };
    (Some(scope.to_string()), summary)
}

/// 综合基础工具权限和路径/网络规则，给出最终权限判定
pub(crate) fn evaluate_tool_permission(
    engine: &crate::runtime::RuntimeEngine,
    tool_name: &str,
    target_scope: Option<&str>,
    target_summary: Option<&str>,
) -> PermissionDecision {
    let base_decision = engine.check_tool_permission(tool_name);
    let scoped_decision = match (target_scope, target_summary) {
        (Some("path"), Some(path)) => Some(engine.permission_gate().check_path(path)),
        (Some("network"), Some(target)) => Some(engine.permission_gate().check_network(target)),
        _ => None,
    };

    match (base_decision, scoped_decision) {
        (PermissionDecision::Denied { reason }, _)
        | (_, Some(PermissionDecision::Denied { reason })) => PermissionDecision::Denied { reason },
        (PermissionDecision::NeedsApproval { request_id }, _) => {
            PermissionDecision::NeedsApproval { request_id }
        }
        (_, Some(PermissionDecision::NeedsApproval { request_id })) => {
            PermissionDecision::NeedsApproval { request_id }
        }
        _ => PermissionDecision::Approved,
    }
}

/// 规范化权限目标（路径相对→绝对，网络提取域名）
pub(crate) fn normalize_permission_target(
    session: &crate::session::Session,
    target_scope: Option<&str>,
    target_summary: Option<&str>,
) -> Option<String> {
    let target = target_summary?.trim();
    if target.is_empty() {
        return None;
    }

    match target_scope {
        Some("path") => Some(normalize_path_target(session, target)),
        Some("network") => Some(normalize_network_target(target)),
        _ => Some(target.to_string()),
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
        // 各插件工具经 tool_permission_overrides 贡献，不在 classify_tool 中心化维护。
        assert_eq!(classify_tool("write_file"), PermissionLevel::Standard);
        assert_eq!(classify_tool("replace_in_file"), PermissionLevel::Standard);
        assert_eq!(classify_tool("run_command"), PermissionLevel::Elevated);
        assert_eq!(classify_tool("web_form_fill"), PermissionLevel::Elevated);
        assert_eq!(classify_tool("web_click"), PermissionLevel::Elevated);
        assert_eq!(classify_tool("web_load_html"), PermissionLevel::Elevated);
        assert_eq!(classify_tool("apply_patch"), PermissionLevel::Critical);
        // spawn_task/cancel_task 已由 tiangong-plugin-task 经 tool_permission_overrides
        // 声明，classify_tool 不再中心化维护，走未知工具默认 Critical 分支。
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
