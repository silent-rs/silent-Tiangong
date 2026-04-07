//! 远程接入角色模型
//!
//! 定义远程接入的三种角色，并提供权限检查能力。

use serde::{Deserialize, Serialize};

/// 远程接入角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRole {
    /// 控制者：完整控制权，可发送消息、执行任务、管理会话
    #[default]
    Controller,
    /// 审批者：只能响应权限审批请求
    Approver,
    /// 观察者：只读查看任务状态和会话内容
    Observer,
}

impl RemoteRole {
    /// 是否可以发送消息触发任务执行
    pub fn can_send_message(&self) -> bool {
        matches!(self, Self::Controller)
    }

    /// 是否可以管理会话（创建、删除、切换）
    pub fn can_manage_sessions(&self) -> bool {
        matches!(self, Self::Controller)
    }

    /// 是否可以响应审批请求
    pub fn can_approve(&self) -> bool {
        matches!(self, Self::Controller | Self::Approver)
    }

    /// 是否可以查看任务状态和会话内容
    pub fn can_observe(&self) -> bool {
        // 所有角色都可以查看
        true
    }

    /// 是否可以取消正在运行的任务
    pub fn can_cancel_task(&self) -> bool {
        matches!(self, Self::Controller)
    }

    /// 获取角色显示名
    pub fn display_name(&self) -> &str {
        match self {
            Self::Controller => "控制者",
            Self::Approver => "审批者",
            Self::Observer => "观察者",
        }
    }
}

impl std::fmt::Display for RemoteRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_has_full_access() {
        let role = RemoteRole::Controller;
        assert!(role.can_send_message());
        assert!(role.can_manage_sessions());
        assert!(role.can_approve());
        assert!(role.can_observe());
        assert!(role.can_cancel_task());
    }

    #[test]
    fn approver_limited_access() {
        let role = RemoteRole::Approver;
        assert!(!role.can_send_message());
        assert!(!role.can_manage_sessions());
        assert!(role.can_approve());
        assert!(role.can_observe());
        assert!(!role.can_cancel_task());
    }

    #[test]
    fn observer_readonly() {
        let role = RemoteRole::Observer;
        assert!(!role.can_send_message());
        assert!(!role.can_manage_sessions());
        assert!(!role.can_approve());
        assert!(role.can_observe());
        assert!(!role.can_cancel_task());
    }

    #[test]
    fn serialization_roundtrip() {
        let role = RemoteRole::Approver;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, r#""approver""#);
        let parsed: RemoteRole = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, role);
    }

    #[test]
    fn default_is_controller() {
        assert_eq!(RemoteRole::default(), RemoteRole::Controller);
    }
}
