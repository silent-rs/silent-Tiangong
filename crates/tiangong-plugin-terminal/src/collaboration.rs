use std::sync::Mutex;
use std::time::Instant;

/// 终端协作状态
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TerminalBusyState {
    /// 终端空闲，可以接受用户输入或 Agent 命令
    #[default]
    Idle,
    /// 用户最近正在操作终端
    UserActive,
    /// Agent 正在执行非交互命令
    AgentRunning { command_id: String },
    /// 终端有前台交互进程（Agent 通过 exec_interactive 启动，如 vi/nano/REPL）
    AgentInteractive { command_id: String },
}

/// 终端输入来源
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputSource {
    /// 前端 xterm.js 用户键盘输入
    User,
    /// Agent 调用 terminal_input 或内部自动输入
    Agent,
}

/// 终端活跃状态追踪器
pub struct TerminalActivityTracker {
    last_user_input: Mutex<Instant>,
    busy_state: Mutex<TerminalBusyState>,
    /// 当前 Agent 命令期间用户是否干预过
    user_intervened: Mutex<bool>,
    /// 用户在终端提交的命令行队列（回车截断的完整命令，供注入 Agent 对话链）
    pending_user_commands: Mutex<Vec<String>>,
}

impl TerminalActivityTracker {
    pub(crate) fn new() -> Self {
        Self {
            last_user_input: Mutex::new(Instant::now() - std::time::Duration::from_secs(3600)),
            busy_state: Mutex::new(TerminalBusyState::Idle),
            user_intervened: Mutex::new(false),
            pending_user_commands: Mutex::new(Vec::new()),
        }
    }

    /// 记录用户输入，如果当前 Agent 正在执行则标记干预
    pub(crate) fn record_user_input(&self) {
        if let Ok(mut t) = self.last_user_input.lock() {
            *t = Instant::now();
        }
        // Agent 执行期间用户输入 → 标记干预
        if let Ok(state) = self.busy_state.lock() {
            if matches!(
                &*state,
                TerminalBusyState::AgentRunning { .. } | TerminalBusyState::AgentInteractive { .. }
            ) {
                if let Ok(mut flag) = self.user_intervened.lock() {
                    *flag = true;
                }
            }
        }
    }

    /// 检查用户是否在指定时间内活跃
    #[allow(dead_code)]
    pub(crate) fn is_user_active(&self, threshold: std::time::Duration) -> bool {
        self.last_user_input
            .lock()
            .map(|t| t.elapsed() < threshold)
            .unwrap_or(false)
    }

    /// 设置协作状态，同时清除干预标记（新命令开始）
    pub(crate) fn set_busy_state(&self, state: TerminalBusyState) {
        if let Ok(mut s) = self.busy_state.lock() {
            *s = state;
        }
        // 进入新命令时清除干预标记
        if let Ok(mut flag) = self.user_intervened.lock() {
            *flag = false;
        }
    }

    /// 获取当前协作状态
    pub(crate) fn busy_state(&self) -> TerminalBusyState {
        self.busy_state
            .lock()
            .map(|s| s.clone())
            .unwrap_or(TerminalBusyState::Idle)
    }

    /// 取出并重置用户干预标记
    pub(crate) fn take_user_intervened(&self) -> bool {
        self.user_intervened
            .lock()
            .map(|mut flag| {
                let v = *flag;
                *flag = false;
                v
            })
            .unwrap_or(false)
    }

    /// 记录用户在终端提交的完整命令行（回车截断后上报）。
    pub(crate) fn record_user_command(&self, command: String) {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Ok(mut cmds) = self.pending_user_commands.lock() {
            cmds.push(trimmed.to_string());
        }
    }
}

impl TerminalBusyState {
    /// 转为前端可读的 phase 字符串
    pub(crate) fn phase_label(&self) -> &'static str {
        match self {
            TerminalBusyState::Idle => "Idle",
            TerminalBusyState::UserActive => "UserActive",
            TerminalBusyState::AgentRunning { .. } => "Running",
            TerminalBusyState::AgentInteractive { .. } => "Interactive",
        }
    }
}

/// 用户终端操作注入（ToolInput 实现）。
///
/// 用户在终端提交命令（回车截断）时，通过 AgentInput trait 统一投递到 Agent 对话链。
/// tool_name 为 `terminal_user_input`，render 返回结构化 JSON。
pub struct TerminalUserInput {
    pub command: String,
}

impl tiangong_core::agent_input::ToolInput for TerminalUserInput {
    fn tool_name(&self) -> &str {
        "terminal_user_input"
    }

    fn render(&self) -> serde_json::Value {
        serde_json::json!({
            "action": "user_executed",
            "command": self.command.trim(),
        })
    }
}
