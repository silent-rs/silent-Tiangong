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
    ///
    /// 仅影响后续懒创建的 PTY；已存在对话 PTY 的 cwd 由其自身管理。
    pub fn set_default_cwd(&self, cwd: String) {
        if let Ok(mut guard) = self.default_cwd.lock() {
            *guard = cwd;
        }
    }

    /// workspace 切换时同步：更新默认 cwd，并销毁所有存活 PTY。
    ///
    /// workspace 切换发生在尚未产生对话的阶段（活跃 session 处于草稿/无消息状态），
    /// 此时终端 PTY 不承载有价值的 shell 状态，直接销毁重建比发送 `cd` 更干净：
    /// - 不在终端历史里留下自动 `cd` 痕迹
    /// - 不误把 `cd` 当作按键发给交互式前台程序（vi/nano/REPL）
    /// - 用户下次打开终端时 `ensure` 用新 `default_cwd` 创建全新 PTY
    ///
    /// 已销毁的 PTY（如固定 id `__draft_terminal__`）会在前端 TerminalPanel 的
    /// `ensure` effect 中被重新创建，xterm 渲染层会感知到后端 PTY 重建。
    pub fn reset_all_for_workspace(&self, cwd: &str) {
        // 1. 更新默认 cwd，使后续懒创建的 PTY 落入新 workspace
        self.set_default_cwd(cwd.to_string());

        // 2. 销毁所有存活 PTY（drop cmd_tx → 命令循环退出 → 子进程终止）
        let ids: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions.keys().cloned().collect()
        };
        for id in ids {
            self.destroy(&id);
        }
    }

    /// 懒创建：获取或创建指定 session 的 PTY。返回 true 表示 PTY 存活。
    ///
    /// 若注册表里已存在该 session 的条目但底层 PTY 已死亡（子进程退出、
    /// `cmd_tx` 失效），会先销毁陈旧条目再重建，避免复用死掉的 PTY 导致
    /// 「终端未就绪」（草稿态固定 id 复用场景的根因，见 issue #156 后续修复）。
    pub fn ensure(&self, session_id: &str, cwd: &str) -> bool {
        // 检查是否已有存活 PTY（注意：必须在此块作用域内释放 sessions 锁，
        // 不得在持锁时调用 create_pty——create_pty 末尾会再次获取 sessions 锁，
        // std::sync::Mutex 不支持递归获取，会导致死锁。）
        let existing_is_dead = {
            let sessions = self.sessions.lock().unwrap();
            match sessions.get(session_id) {
                Some(pty) if pty.manager.is_alive() => return true,
                Some(_) => true, // 存在但已死亡，需销毁重建
                None => false,   // 不存在，直接创建
            }
        };
        if existing_is_dead {
            self.destroy(session_id);
        }
        self.create_pty(session_id, cwd)
    }

    /// 创建指定 session 的 PTY（调用方负责保证注册表里无该 session 的存活条目）。
    fn create_pty(&self, session_id: &str, cwd: &str) -> bool {
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

    /// 把临时草稿 id 的 PTY 迁移到真实 session_id（草稿态转正时调用）。
    ///
    /// 草稿态新对话用稳定临时 id 创建 PTY（如 `__draft_<n>`），首条消息创建后端
    /// session 拿到真实 id 后调用此方法完成迁移：
    /// - 注册表 key：临时 id → 真实 id
    /// - manager 内部 session_id：更新（命令循环/输出线程后续用新 id）
    /// - 持久化日志文件：临时目录 → 真实目录（rename，保留草稿期已产生的历史）
    ///
    /// 幂等：真实 id 已存在或临时 id 不存在时安全返回（不破坏已有状态）。
    /// 前端调用前应保证真实 id 尚未创建 PTY（否则会与转正迁移冲突）。
    pub fn attach_persistent_session_id(&self, draft_id: &str, persistent_id: &str) {
        if draft_id == persistent_id {
            return;
        }
        let slot = {
            let mut sessions = self.sessions.lock().unwrap();
            // 真实 id 已存在：说明 session 已有自己的 PTY，无需迁移
            if sessions.contains_key(persistent_id) {
                info!(
                    draft_id,
                    persistent_id, "真实 session 已有 PTY，跳过草稿迁移"
                );
                return;
            }
            sessions.remove(draft_id)
        };

        let Some(slot) = slot else {
            // 草稿 id 不存在（用户草稿态没打开终端就没创建），无需迁移
            return;
        };

        // 更新 manager 内部 session_id（命令循环/输出线程动态读取，自动生效）
        slot.manager.set_session_id(persistent_id.to_string());

        // 迁移持久化日志：草稿目录 → 真实目录
        let draft_log = terminal_log_path(draft_id);
        let real_log = terminal_log_path(persistent_id);
        if draft_log.exists() {
            if let Some(parent) = real_log.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    error!(
                        draft_id, persistent_id, error = %e,
                        "迁移终端日志：创建真实 session 目录失败"
                    );
                }
            }
            if let Err(e) = std::fs::rename(&draft_log, &real_log) {
                // rename 失败不阻塞迁移：日志是辅助的，PTY 本身已就绪
                error!(
                    draft_id, persistent_id, error = %e,
                    "迁移终端日志文件失败（PTY 已迁移，日志保留在草稿目录）"
                );
            }
        }

        // 重新 insert 到真实 key
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(persistent_id.to_string(), slot);
        info!(draft_id, persistent_id, "草稿 PTY 已转正迁移到真实 session");
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

    fn exec_interactive(
        &self,
        session_id: &str,
        command: &str,
        wait_secs: u64,
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
                TerminalCommand::ExecInteractive {
                    command,
                    wait_secs,
                    response_tx,
                },
                response_rx,
                180
            );
            Some(resp.into())
        })
    }

    fn exec_command_interactive(
        &self,
        session_id: &str,
        cmd: &str,
        args: &[String],
        wait_secs: u64,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<tiangong_core::terminal_trait::TerminalExecResult>,
                > + Send,
        >,
    > {
        // 拼装命令字符串，委托给 exec_interactive
        let mut parts = vec![cmd.to_string()];
        for arg in args {
            parts.push(shell_quote(arg));
        }
        let command = parts.join(" ");
        self.exec_interactive(session_id, &command, wait_secs)
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

    fn send_interactive(
        &self,
        session_id: &str,
        input: &str,
        wait_secs: u64,
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
        let input = input.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let resp: crate::types::TerminalExecResponse = send_and_wait!(
                tx,
                TerminalCommand::SendInteractive {
                    input,
                    wait_secs,
                    response_tx,
                },
                response_rx,
                180
            );
            Some(resp.into())
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
