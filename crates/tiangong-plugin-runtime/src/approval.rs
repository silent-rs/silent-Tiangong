//! 审批接缝（Approval Seam）契约与路由骨架。
//!
//! 审批请求/响应是宿主与审批处理器（默认内置 UI 或三方插件）之间的稳定契约；
//! Core 的审批状态机（等待/放行/拒绝）保留，本模块只定形契约与处理器路由，
//! 不感知具体工具名与业务负载（宿主中性）。
//!
//! 风险分级策略（tool-spec 的 dangerous 元数据等）接入前，宿主以
//! `standard` 占位；三方处理器经 `capabilities.approval = true` 注册。

use serde::{Deserialize, Serialize};

/// 审批风险等级（展示与路由策略输入；分级策略由工具元数据提供）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRisk {
    Safe,
    #[default]
    Standard,
    Elevated,
    Critical,
}

/// 一次审批请求（任何需要人确认的操作产生一条）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub request_id: String,
    /// 发起方插件 ID（内置工具为空）。
    #[serde(default)]
    pub plugin_id: String,
    pub tool_name: String,
    /// 人可读的操作摘要。
    #[serde(default)]
    pub summary: String,
    /// 参数摘要/完整参数 JSON 文本（按发起方策略）。
    #[serde(default)]
    pub arguments: String,
    pub risk: ApprovalRisk,
}

/// 审批响应结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionKind {
    Approved,
    Rejected,
    /// 允许并记住（会话级对同工具放行）。
    AlwaysAllow,
}

/// 一次审批响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub request_id: String,
    pub decision: ApprovalDecisionKind,
    /// 拒绝原因等补充说明（可选）。
    #[serde(default)]
    pub reason: String,
}

/// 审批处理器注册项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalHandler {
    /// 处理器插件 ID。
    pub plugin_id: String,
    /// 接管范围（如工具名前缀或 `*` 全量；默认 `*`）。
    pub scope: String,
}

/// 审批路由表：默认处理器（内置 UI）+ 三方处理器注册。
///
/// 三方处理器按 scope 匹配请求的工具名，命中即由宿主经桥接 `approval.*`
/// 转发；未命中回退默认处理器。卸载时 `unregister_plugin` 回滚。
#[derive(Debug, Clone, Default)]
pub struct ApprovalRouter {
    handlers: Vec<ApprovalHandler>,
}

/// 默认处理器的 plugin_id 标识（内置审批 UI）。
pub const DEFAULT_HANDLER: &str = "__builtin__";

impl ApprovalRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册处理器；同 (plugin_id, scope) 重复注册刷新。
    pub fn register(&mut self, handler: ApprovalHandler) {
        self.handlers.retain(|item| item != &handler);
        self.handlers.push(handler);
    }

    /// 卸载插件的全部注册（贡献可逆）。
    pub fn unregister_plugin(&mut self, plugin_id: &str) {
        self.handlers.retain(|item| item.plugin_id != plugin_id);
    }

    /// 路由：按工具名匹配处理器 scope（前缀或全量 `*`），最长前缀优先；
    /// 未命中回退默认处理器。
    pub fn route(&self, tool_name: &str) -> &str {
        self.handlers
            .iter()
            .filter(|handler| handler.scope == "*" || tool_name.starts_with(&handler.scope))
            .max_by_key(|handler| handler.scope.len())
            .map(|handler| handler.plugin_id.as_str())
            .unwrap_or(DEFAULT_HANDLER)
    }

    /// 已注册处理器列表（调试/管理页展示）。
    pub fn handlers(&self) -> &[ApprovalHandler] {
        &self.handlers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 未注册时回退默认处理器() {
        let router = ApprovalRouter::new();
        assert_eq!(router.route("fs_write"), DEFAULT_HANDLER);
    }

    #[test]
    fn 前缀匹配与最长优先() {
        let mut router = ApprovalRouter::new();
        router.register(ApprovalHandler {
            plugin_id: "com.audit.fs".to_string(),
            scope: "fs_".to_string(),
        });
        router.register(ApprovalHandler {
            plugin_id: "com.audit.all".to_string(),
            scope: "*".to_string(),
        });

        // fs_ 前缀命中专用处理器（最长前缀优先于 `*`）
        assert_eq!(router.route("fs_write"), "com.audit.fs");
        // 其他工具走全量处理器
        assert_eq!(router.route("terminal_run"), "com.audit.all");
    }

    #[test]
    fn 卸载回滚注册() {
        let mut router = ApprovalRouter::new();
        router.register(ApprovalHandler {
            plugin_id: "com.audit".to_string(),
            scope: "*".to_string(),
        });
        router.unregister_plugin("com.audit");
        assert!(router.handlers().is_empty());
        assert_eq!(router.route("any"), DEFAULT_HANDLER);
    }

    #[test]
    fn 契约序列化_snake_case() {
        let response = ApprovalResponse {
            request_id: "r1".to_string(),
            decision: ApprovalDecisionKind::AlwaysAllow,
            reason: String::new(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"decision\":\"always_allow\""));
        let parsed: ApprovalResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, response);
    }
}
