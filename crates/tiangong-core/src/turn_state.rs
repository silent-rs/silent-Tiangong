//! Turn 级状态机定义
//!
//! 定义 Turn 执行过程中的所有可能阶段，为事件驱动状态机提供状态枚举。
//! Phase 3 中的 `TurnRunner` 将使用此状态机替换当前的 ReAct 循环。

use serde::{Deserialize, Serialize};

/// Turn 执行阶段
///
/// 表示一次 Turn（用户消息 → 助手回复）在执行过程中的当前阶段。
/// 状态转换由 `TurnRunner` 驱动。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "phase")]
pub enum TurnPhase {
    /// 初始化：构建工具定义、系统 prompt
    Init,

    /// 上下文装配：整理历史消息、环境信息、工具说明
    ContextAssembly,

    /// LLM 调用中：等待模型响应
    LlmCalling,

    /// 工具分发：权限检查、准备执行
    ToolDispatching,

    /// 等待用户审批：高风险操作需要人工确认
    WaitingApproval {
        tool_name: String,
        request_id: String,
    },

    /// 工具执行中：正在执行已批准的工具调用
    ToolExecuting,

    /// 结果处理：归纳工具结果、构建反馈消息、上下文压缩
    ResultProcessing,

    /// 响应生成：流式输出最终回复
    Responding,

    /// 执行完成
    Completed,

    /// 执行失败
    Failed {
        error: String,
    },

    /// 已取消
    Cancelled,
}

impl TurnPhase {
    /// 是否为终止状态
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TurnPhase::Completed | TurnPhase::Failed { .. } | TurnPhase::Cancelled
        )
    }

    /// 是否正在等待外部输入
    pub fn is_waiting(&self) -> bool {
        matches!(self, TurnPhase::WaitingApproval { .. })
    }

    /// 获取阶段的简短描述
    pub fn display_name(&self) -> &str {
        match self {
            TurnPhase::Init => "初始化",
            TurnPhase::ContextAssembly => "上下文装配",
            TurnPhase::LlmCalling => "模型调用",
            TurnPhase::ToolDispatching => "工具分发",
            TurnPhase::WaitingApproval { .. } => "等待审批",
            TurnPhase::ToolExecuting => "工具执行",
            TurnPhase::ResultProcessing => "结果处理",
            TurnPhase::Responding => "响应生成",
            TurnPhase::Completed => "已完成",
            TurnPhase::Failed { .. } => "已失败",
            TurnPhase::Cancelled => "已取消",
        }
    }
}

impl std::fmt::Display for TurnPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states() {
        assert!(TurnPhase::Completed.is_terminal());
        assert!(TurnPhase::Cancelled.is_terminal());
        assert!((TurnPhase::Failed { error: "err".into() }).is_terminal());

        assert!(!TurnPhase::Init.is_terminal());
        assert!(!TurnPhase::LlmCalling.is_terminal());
        assert!(!TurnPhase::ToolExecuting.is_terminal());
        assert!(!TurnPhase::Responding.is_terminal());
    }

    #[test]
    fn waiting_states() {
        let approval = TurnPhase::WaitingApproval {
            tool_name: "run_command".into(),
            request_id: "req-1".into(),
        };
        assert!(approval.is_waiting());

        assert!(!TurnPhase::Init.is_waiting());
        assert!(!TurnPhase::ToolExecuting.is_waiting());
        assert!(!TurnPhase::Completed.is_waiting());
    }

    #[test]
    fn display_names_non_empty() {
        let phases = [
            TurnPhase::Init,
            TurnPhase::ContextAssembly,
            TurnPhase::LlmCalling,
            TurnPhase::ToolDispatching,
            TurnPhase::WaitingApproval {
                tool_name: "t".into(),
                request_id: "r".into(),
            },
            TurnPhase::ToolExecuting,
            TurnPhase::ResultProcessing,
            TurnPhase::Responding,
            TurnPhase::Completed,
            TurnPhase::Failed { error: "e".into() },
            TurnPhase::Cancelled,
        ];
        for phase in &phases {
            assert!(!phase.display_name().is_empty(), "{phase:?} has empty display_name");
            assert!(!format!("{phase}").is_empty());
        }
    }

    #[test]
    fn serialization_roundtrip() {
        let phase = TurnPhase::WaitingApproval {
            tool_name: "run_command".into(),
            request_id: "abc-123".into(),
        };
        let json = serde_json::to_string(&phase).unwrap();
        assert!(json.contains("waiting_approval"));
        assert!(json.contains("run_command"));

        let deserialized: TurnPhase = serde_json::from_str(&json).unwrap();
        if let TurnPhase::WaitingApproval { tool_name, request_id } = deserialized {
            assert_eq!(tool_name, "run_command");
            assert_eq!(request_id, "abc-123");
        } else {
            panic!("expected WaitingApproval");
        }
    }

    #[test]
    fn failed_preserves_error() {
        let phase = TurnPhase::Failed { error: "网络超时".into() };
        let json = serde_json::to_string(&phase).unwrap();
        let deserialized: TurnPhase = serde_json::from_str(&json).unwrap();
        if let TurnPhase::Failed { error } = deserialized {
            assert_eq!(error, "网络超时");
        } else {
            panic!("expected Failed");
        }
    }
}
