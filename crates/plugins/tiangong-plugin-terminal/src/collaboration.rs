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
    /// 终端有前台交互进程（Agent 通过 exec_interactive 启动）
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
    on_idle: Option<Box<dyn Fn() + Send + Sync>>,
}

impl TerminalActivityTracker {
    pub(crate) fn new() -> Self {
        Self {
            last_user_input: Mutex::new(Instant::now() - std::time::Duration::from_secs(3600)),
            busy_state: Mutex::new(TerminalBusyState::Idle),
            user_intervened: Mutex::new(false),
            on_idle: None,
        }
    }

    pub(crate) fn with_idle_callback(callback: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            on_idle: Some(Box::new(callback)),
            ..Self::new()
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
        let became_idle = if let Ok(mut s) = self.busy_state.lock() {
            let became_idle =
                !matches!(*s, TerminalBusyState::Idle) && matches!(&state, TerminalBusyState::Idle);
            *s = state;
            became_idle
        } else {
            false
        };
        // 进入新命令时清除干预标记
        if let Ok(mut flag) = self.user_intervened.lock() {
            *flag = false;
        }
        if became_idle {
            self.notify_idle();
        }
    }

    /// 仅在终端空闲时为即将提交的 Agent 命令占用终端。
    pub(crate) fn try_reserve_agent_command(&self, command_id: String) -> bool {
        let Ok(mut state) = self.busy_state.lock() else {
            return false;
        };
        if !matches!(*state, TerminalBusyState::Idle) {
            return false;
        }
        *state = TerminalBusyState::AgentRunning { command_id };
        drop(state);
        if let Ok(mut flag) = self.user_intervened.lock() {
            *flag = false;
        }
        true
    }

    /// 选择结果未进入实际执行时，只释放仍属于该选择结果的占用。
    pub(crate) fn release_agent_reservation(&self, command_id: &str) {
        let Ok(mut state) = self.busy_state.lock() else {
            return;
        };
        let owned = matches!(
            &*state,
            TerminalBusyState::AgentRunning {
                command_id: current
            } if current == command_id
        );
        if owned {
            *state = TerminalBusyState::Idle;
        }
        drop(state);
        if owned {
            self.notify_idle();
        }
    }

    /// 获取当前协作状态
    pub(crate) fn busy_state(&self) -> TerminalBusyState {
        self.busy_state
            .lock()
            .map(|s| s.clone())
            .unwrap_or(TerminalBusyState::Idle)
    }

    fn notify_idle(&self) {
        if let Some(callback) = &self.on_idle {
            callback();
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_agent_reservations_only_claim_idle_terminal_once() {
        let tracker = Arc::new(TerminalActivityTracker::new());
        let barrier = Arc::new(Barrier::new(8));
        let workers = (0..8)
            .map(|index| {
                let tracker = Arc::clone(&tracker);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let command_id = format!("reservation-{index}");
                    barrier.wait();
                    tracker
                        .try_reserve_agent_command(command_id.clone())
                        .then_some(command_id)
                })
            })
            .collect::<Vec<_>>();

        let winners = workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(winners.len(), 1);
        tracker.release_agent_reservation("not-the-owner");
        assert!(matches!(
            tracker.busy_state(),
            TerminalBusyState::AgentRunning { .. }
        ));
        tracker.release_agent_reservation(&winners[0]);
        assert_eq!(tracker.busy_state(), TerminalBusyState::Idle);

        let next_command = "next-command".to_string();
        assert!(tracker.try_reserve_agent_command(next_command.clone()));
        tracker.release_agent_reservation(&winners[0]);
        assert_eq!(
            tracker.busy_state(),
            TerminalBusyState::AgentRunning {
                command_id: next_command.clone()
            }
        );
        tracker.release_agent_reservation(&next_command);
        assert_eq!(tracker.busy_state(), TerminalBusyState::Idle);
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

/// 单个终端 Tab 的状态快照（用于首轮 `terminal_data` 注入）。
#[derive(Debug, Clone)]
pub struct TerminalTabSnapshot {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub shell: String,
    pub phase: String,
    pub alive: bool,
    /// 本轮新增输出；没有新增内容时为空。
    pub recent_output: String,
    /// 本段新增输出的单调游标区间 `[output_cursor_start, output_cursor_end)`。
    pub output_cursor_start: usize,
    pub output_cursor_end: usize,
    /// 起始游标之前的输出已被环形缓冲淘汰或受注入上限截断。
    pub output_truncated: bool,
}

/// 终端状态快照注入（ToolInput 实现）。
///
/// 对齐浏览器的 `browser_data`：agent 首轮启动时注入当前终端全貌——各 Tab 的工作
/// 目录、shell 类型、运行阶段和最近输出，使 agent 像感知浏览器页面一样感知终端
/// 上下文（用户已在终端做了什么、当前处于什么环境），避免重复执行已知命令。
///
/// tool_name 为 `terminal_data`，render 返回结构化 JSON。
pub struct TerminalStateData {
    pub tabs: Vec<TerminalTabSnapshot>,
    pub active_tab_id: Option<String>,
}

impl tiangong_core::agent_input::ToolInput for TerminalStateData {
    fn tool_name(&self) -> &str {
        "terminal_data"
    }

    fn render(&self) -> serde_json::Value {
        let tabs = self
            .tabs
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "title": t.title,
                    "cwd": t.cwd,
                    "shell": t.shell,
                    "phase": t.phase,
                    "alive": t.alive,
                    "recent_output": t.recent_output,
                    "output_cursor_start": t.output_cursor_start,
                    "output_cursor_end": t.output_cursor_end,
                    "output_truncated": t.output_truncated,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "tabs": tabs,
            "active_tab_id": self.active_tab_id,
        })
    }
}
