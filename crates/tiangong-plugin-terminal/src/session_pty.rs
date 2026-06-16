//! 按对话（session）独立的 PTY 注册表 + session-aware provider。
//!
//! 每个对话懒创建自己的 PTY（独立 shell + 独立历史 + 独立 cwd），
//! agent 命令和面板操作都路由到当前对话的 PTY。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{error, info};

use crate::collaboration::TerminalActivityTracker;
use crate::manager::{spawn_command_loop, TerminalManager};
use crate::output_processor;
use crate::types::TerminalCommand;
use crate::util::shell_quote;

/// 单个对话的 PTY 槽位
pub struct SessionPty {
    pub manager: Arc<TerminalManager>,
    pub cmd_tx: mpsc::Sender<TerminalCommand>,
    /// 该对话专属的协作状态机（用户/Agent 协作，跨对话互不干扰）
    pub activity: Arc<TerminalActivityTracker>,
}

/// 按对话管理的 PTY 注册表
pub struct SessionPtyRegistry {
    sessions: Mutex<HashMap<String, SessionPty>>,
    app: tauri::AppHandle,
    /// 懒创建 PTY 时使用的默认 cwd，由 `set_cwd` / workspace 切换同步
    default_cwd: Mutex<String>,
}

impl SessionPtyRegistry {
    pub fn new(app: tauri::AppHandle, default_cwd: String) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            app,
            default_cwd: Mutex::new(default_cwd),
        }
    }

    /// 更新默认 cwd（workspace 切换或初始化同步时调用）。
    /// 仅影响后续懒创建的 PTY；已存在的对话 PTY 的 cwd 由其自身管理。
    pub fn set_default_cwd(&self, cwd: String) {
        if let Ok(mut guard) = self.default_cwd.lock() {
            *guard = cwd;
        }
    }

    /// 懒创建：获取或创建指定 session 的 PTY。返回 true 表示 PTY 存活。
    pub fn ensure(&self, session_id: &str, cwd: &str) -> bool {
        {
            let sessions = self.sessions.lock().unwrap();
            if sessions.contains_key(session_id) {
                return true;
            }
        }

        let default_cwd = self
            .default_cwd
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let effective_cwd = if cwd.is_empty() {
            default_cwd
        } else {
            cwd.to_string()
        };

        let mut manager = TerminalManager::new(session_id.to_string(), effective_cwd.clone());

        // 分对话落盘日志
        let log_path = terminal_log_path(session_id);
        if let Some(logger) = output_processor::OutputLogger::open(log_path) {
            let tail = output_processor::read_log_tail(logger.path(), DEFAULT_LOG_TAIL_LINES);
            if !tail.is_empty() {
                let mut state = manager.state.lock().unwrap();
                for line in &tail {
                    output_processor::backfill_line(&mut state, line.clone());
                }
                drop(state);
            }
            manager.set_logger(Arc::new(logger));
        }

        let manager = Arc::new(manager);
        let (tx, rx) = mpsc::channel::<TerminalCommand>(16);
        // 每个对话独立协作状态机：用户在该对话打断 agent 不会影响其它对话
        let activity = Arc::new(TerminalActivityTracker::new());

        let pty_state =
            manager.start_and_spawn_reader(session_id, &effective_cwd, self.app.clone());
        if pty_state.is_none() {
            error!(session_id, "对话 PTY 启动失败");
            return false;
        }

        let mgr = manager.clone();
        let app = self.app.clone();
        let sid = session_id.to_string();
        let act = activity.clone();
        tauri::async_runtime::spawn(async move {
            spawn_command_loop(rx, mgr, app, pty_state, Some(act)).await;
            info!(session_id = %sid, "对话 PTY 命令循环退出");
        });

        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(
            session_id.to_string(),
            SessionPty {
                manager,
                cmd_tx: tx,
                activity,
            },
        );
        info!(session_id, "对话 PTY 已创建");
        true
    }

    /// 获取指定 session 的 PTY 槽位
    pub fn get(&self, session_id: &str) -> Option<SessionPty> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session_id).map(|s| SessionPty {
            manager: s.manager.clone(),
            cmd_tx: s.cmd_tx.clone(),
            activity: s.activity.clone(),
        })
    }

    /// 销毁指定 session 的 PTY（drop cmd_tx → 命令循环退出 → 子进程终止）
    pub fn destroy(&self, session_id: &str) {
        let slot = {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.remove(session_id)
        };
        if slot.is_some() {
            info!(session_id, "对话 PTY 已销毁");
        }
    }

    /// 列出所有 session 的状态摘要（phase 取自各对话的协作状态机）
    pub fn list_statuses(&self) -> Vec<crate::types::TerminalSessionStatus> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .iter()
            .map(|(id, slot)| crate::types::TerminalSessionStatus {
                session_id: id.clone(),
                alive: slot.manager.is_alive(),
                cwd: slot.manager.cwd(),
                shell: slot.manager.shell(),
                phase: slot.activity.busy_state().phase_label().to_string(),
            })
            .collect()
    }
}

/// Session-aware TerminalProvider：每个方法显式接收 session_id，路由到对应对话的 PTY。
///
/// 不持有任何全局 mutable session 状态——session_id 由调用方（RuntimeEngine）
/// 在每次工具执行时显式传入，避免并发对话之间的路由竞态。
pub struct SessionAwareTerminalProvider {
    registry: Arc<SessionPtyRegistry>,
}

impl SessionAwareTerminalProvider {
    pub fn new(registry: Arc<SessionPtyRegistry>) -> Self {
        Self { registry }
    }

    /// 获取指定 session 的 cmd_tx（懒创建）
    fn tx(&self, session_id: &str) -> Option<mpsc::Sender<TerminalCommand>> {
        if session_id.is_empty() {
            return None;
        }
        // 懒创建：若不存在则用 default cwd 创建
        if self.registry.get(session_id).is_none() {
            let default_cwd = self
                .registry
                .default_cwd
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            self.registry.ensure(session_id, &default_cwd);
        }
        self.registry.get(session_id).map(|s| s.cmd_tx)
    }

    /// 获取指定 session 的 manager（懒创建）
    fn manager(&self, session_id: &str) -> Option<Arc<TerminalManager>> {
        if session_id.is_empty() {
            return None;
        }
        if self.registry.get(session_id).is_none() {
            let default_cwd = self
                .registry
                .default_cwd
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            self.registry.ensure(session_id, &default_cwd);
        }
        self.registry.get(session_id).map(|s| s.manager)
    }
}

/// 系统 PTY 日志回填的最大行数
const DEFAULT_LOG_TAIL_LINES: usize = 5000;

/// 分对话的 PTY 持久化日志路径：`~/.tiangong/sessions/<session_id>/terminal.log`
fn terminal_log_path(session_id: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home)
        .join(".tiangong")
        .join("sessions")
        .join(session_id)
        .join("terminal.log")
}

// ===== TerminalProvider trait 实现 =====
// 所有方法显式接收 session_id，按参数路由到对应对话的 PTY（无全局状态）。

macro_rules! send_and_wait {
    ($tx:expr, $cmd:expr, $rx:expr, $timeout:expr) => {{
        if $tx.send($cmd).await.is_err() {
            return None;
        }
        tokio::time::timeout(std::time::Duration::from_secs($timeout), $rx)
            .await
            .ok()?
            .ok()?
    }};
}

impl tiangong_core::terminal_trait::TerminalProvider for SessionAwareTerminalProvider {
    fn exec(
        &self,
        session_id: &str,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<tiangong_core::terminal_trait::TerminalExecResult>,
                > + Send,
        >,
    > {
        let tx = match self.tx(session_id) {
            Some(tx) => tx,
            None => return Box::pin(async { None }),
        };
        let command = command.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let resp: crate::types::TerminalExecResponse = send_and_wait!(
                tx,
                TerminalCommand::Exec {
                    command,
                    timeout_secs,
                    response_tx,
                },
                response_rx,
                180
            );
            Some(resp.into())
        })
    }

    fn exec_command(
        &self,
        session_id: &str,
        cmd: &str,
        args: &[String],
        timeout_secs: Option<u64>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<tiangong_core::terminal_trait::TerminalExecResult>,
                > + Send,
        >,
    > {
        // 拼装命令字符串
        let mut parts = vec![cmd.to_string()];
        for arg in args {
            parts.push(shell_quote(arg));
        }
        let command = parts.join(" ");
        self.exec(session_id, &command, timeout_secs)
    }

    fn recent_output(
        &self,
        session_id: &str,
        lines: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        let manager = self.manager(session_id);
        Box::pin(async move {
            let manager = manager?;
            Some(manager.recent_output(lines))
        })
    }

    fn current_cwd(
        &self,
        session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        let manager = self.manager(session_id);
        Box::pin(async move {
            let manager = manager?;
            Some(manager.cwd())
        })
    }

    fn send_input(
        &self,
        session_id: &str,
        input: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<()>> + Send>> {
        let tx = match self.tx(session_id) {
            Some(tx) => tx,
            None => return Box::pin(async { None }),
        };
        let input = input.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            send_and_wait!(
                tx,
                TerminalCommand::SendInput {
                    input,
                    source: crate::collaboration::InputSource::Agent,
                    response_tx,
                },
                response_rx,
                5
            );
            Some(())
        })
    }

    fn reset(
        &self,
        session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<()>> + Send>> {
        let tx = match self.tx(session_id) {
            Some(tx) => tx,
            None => return Box::pin(async { None }),
        };
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            send_and_wait!(tx, TerminalCommand::Reset { response_tx }, response_rx, 10);
            Some(())
        })
    }
}
