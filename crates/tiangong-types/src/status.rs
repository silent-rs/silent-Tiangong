//! 运行状态类型

use serde::{Deserialize, Serialize};

/// 运行状态
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Idle,
    Planning,
    Executing,
    Completed,
    Failed,
}

impl RunStatus {
    pub fn display_name(&self) -> &str {
        match self {
            Self::Idle => "idle",
            Self::Planning => "planning",
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}
