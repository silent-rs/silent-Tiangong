//! 按对话（session）和终端 Tab 独立的 PTY 注册表 + session-aware provider。
//!
//! 每个对话可以懒创建多个 PTY（独立 shell + 独立历史 + 独立 cwd），
//! 旧的纯 session_id 调用会路由到当前活跃终端 Tab，新的 session_id:tab_id
//! 复合 id 可以精确路由到指定 Tab。

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::Emitter;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::collaboration::{TerminalActivityTracker, TerminalBusyState};
use crate::manager::{spawn_command_loop, TerminalManager};
use crate::output_processor;
use crate::session_store::{TerminalSessionPersisted, TerminalSessionStore, TerminalTabPersisted};
use crate::types::TerminalCommand;
use crate::util::shell_quote;

/// 单个对话的 PTY 槽位
#[derive(Clone)]
pub struct SessionPty {
    pub tab_id: String,
    pub title: String,
    pub created_at: String,
    pub manager: Arc<TerminalManager>,
    pub cmd_tx: mpsc::Sender<TerminalCommand>,
    /// 该终端 Tab 专属的协作状态机（用户/Agent 协作，跨 Tab 互不干扰）
    pub activity: Arc<TerminalActivityTracker>,
}

/// 单个对话内的终端 Tab 集合。
pub struct SessionTabs {
    pub tabs: Mutex<HashMap<String, SessionPty>>,
    pub active_tab_id: Mutex<Option<String>>,
    pub activity: Arc<TerminalActivityTracker>,
}

impl SessionTabs {
    fn new() -> Self {
        Self {
            tabs: Mutex::new(HashMap::new()),
            active_tab_id: Mutex::new(None),
            activity: Arc::new(TerminalActivityTracker::new()),
        }
    }

    fn active_or_first_tab_id(&self) -> Option<String> {
        let tabs = self.tabs.lock().unwrap();
        if let Some(active) = self.active_tab_id.lock().unwrap().clone() {
            if tabs.contains_key(&active) {
                return Some(active);
            }
        }
        tabs.keys().next().cloned()
    }

    fn set_active_tab(&self, tab_id: impl Into<String>) {
        *self.active_tab_id.lock().unwrap() = Some(tab_id.into());
    }
}

#[derive(Debug, Clone)]
struct TerminalRoute {
    session_id: String,
    tab_id: Option<String>,
}

struct TerminalTabCreation {
    title: Option<String>,
    created_at: Option<String>,
    activate: bool,
    preferred_active: Option<String>,
}

fn parse_terminal_id(value: &str) -> Option<TerminalRoute> {
    if value.trim().is_empty() {
        return None;
    }
    if let Some((session_id, tab_id)) = value.split_once(':') {
        let session_id = session_id.trim();
        let tab_id = tab_id.trim();
        if session_id.is_empty() || tab_id.is_empty() {
            return None;
        }
        return Some(TerminalRoute {
            session_id: session_id.to_string(),
            tab_id: Some(tab_id.to_string()),
        });
    }
    Some(TerminalRoute {
        session_id: value.to_string(),
        tab_id: None,
    })
}

fn terminal_instance_id(session_id: &str, tab_id: &str) -> String {
    format!("{session_id}:{tab_id}")
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// 按对话管理的 PTY 注册表
pub struct SessionPtyRegistry {
    sessions: Mutex<HashMap<String, Arc<SessionTabs>>>,
    /// 串行化终端插件元数据的读改写，避免并发创建/关闭互相覆盖。
    persistence: Mutex<()>,
    app: tauri::AppHandle,
    /// 懒创建 PTY 时使用的默认 cwd，由 `set_cwd` / workspace 切换同步
    default_cwd: Mutex<String>,
}

impl SessionPtyRegistry {
    pub fn new(app: tauri::AppHandle, default_cwd: String) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            persistence: Mutex::new(()),
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
    /// - 不误把 `cd` 当作按键发给交互式前台程序
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
            self.destroy_runtime_session(&id);
        }
    }

    fn session_tabs(&self, session_id: &str) -> Arc<SessionTabs> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(SessionTabs::new()))
            .clone()
    }

    fn existing_session_tabs(&self, session_id: &str) -> Option<Arc<SessionTabs>> {
        self.sessions.lock().unwrap().get(session_id).cloned()
    }

    fn emit_tab_updated(&self, session_id: &str, active_tab_id: Option<String>, source: &str) {
        let _ = self.app.emit(
            "terminal:tab_updated",
            &crate::types::TerminalTabUpdatedEvent {
                session_id: session_id.to_string(),
                active_tab_id,
                source: source.to_string(),
            },
        );
    }

    fn persisted_snapshot(&self, session_id: &str) -> anyhow::Result<TerminalSessionPersisted> {
        let persisted = TerminalSessionStore::load_or_migrate_legacy(session_id)?;
        Ok(self.merge_runtime_metadata(session_id, persisted))
    }

    fn merge_runtime_metadata(
        &self,
        session_id: &str,
        persisted: TerminalSessionPersisted,
    ) -> TerminalSessionPersisted {
        let mut by_id = persisted
            .tabs
            .into_iter()
            .map(|tab| (tab.id.clone(), tab))
            .collect::<HashMap<_, _>>();
        let mut runtime_active = None;
        if let Some(session_tabs) = self.existing_session_tabs(session_id) {
            let tabs = session_tabs.tabs.lock().unwrap();
            runtime_active = session_tabs.active_tab_id.lock().unwrap().clone();
            for tab in tabs.values() {
                by_id.insert(
                    tab.tab_id.clone(),
                    TerminalTabPersisted {
                        id: tab.tab_id.clone(),
                        title: tab.title.clone(),
                        created_at: tab.created_at.clone(),
                    },
                );
            }
        }
        let mut persisted_tabs = by_id.into_values().collect::<Vec<_>>();
        persisted_tabs.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let active_tab_id = runtime_active
            .filter(|active| persisted_tabs.iter().any(|tab| tab.id == *active))
            .or_else(|| {
                persisted
                    .active_tab_id
                    .filter(|active| persisted_tabs.iter().any(|tab| tab.id == *active))
            })
            .or_else(|| persisted_tabs.first().map(|tab| tab.id.clone()));
        TerminalSessionPersisted {
            tabs: persisted_tabs,
            active_tab_id,
        }
    }

    fn persist_session_metadata(&self, session_id: &str) {
        let _guard = self.persistence.lock().unwrap();
        let state = match self.persisted_snapshot(session_id) {
            Ok(state) => state,
            Err(error) => {
                warn!(%error, session_id, "加载终端会话状态失败，取消本次覆盖写入");
                return;
            }
        };
        if let Err(error) = TerminalSessionStore::save(session_id, &state) {
            warn!(%error, session_id, "持久化终端会话状态失败");
        }
    }

    /// 懒创建：获取或创建指定 session / terminal id 的 PTY。返回 true 表示 PTY 存活。
    ///
    /// 若注册表里已存在该 Tab 的条目但底层 PTY 已死亡（子进程退出、
    /// `cmd_tx` 失效），会先销毁陈旧条目再重建，避免复用死掉的 PTY 导致
    /// 「终端未就绪」（草稿态固定 id 复用场景的根因，见 issue #156 后续修复）。
    pub fn ensure(&self, terminal_id: &str, cwd: &str) -> bool {
        self.ensure_with_title(terminal_id, cwd, None, true)
    }

    fn ensure_with_title(
        &self,
        terminal_id: &str,
        cwd: &str,
        title: Option<String>,
        activate: bool,
    ) -> bool {
        let Some(route) = parse_terminal_id(terminal_id) else {
            return false;
        };
        let session_tabs = self.session_tabs(&route.session_id);
        let persisted = TerminalSessionStore::load_or_migrate_legacy(&route.session_id)
            .unwrap_or_else(|error| {
                warn!(%error, session_id = %route.session_id, "加载终端会话状态失败");
                TerminalSessionPersisted::default()
            });
        let tab_id = route
            .tab_id
            .clone()
            .or_else(|| session_tabs.active_or_first_tab_id())
            .or_else(|| persisted.active_tab_id.clone())
            .or_else(|| persisted.tabs.first().map(|tab| tab.id.clone()))
            .unwrap_or_else(|| scru128::new().to_string());
        let persisted_tab = persisted.tabs.iter().find(|tab| tab.id == tab_id);
        let title = title.or_else(|| persisted_tab.map(|tab| tab.title.clone()));
        let created_at = persisted_tab.map(|tab| tab.created_at.clone());
        let preferred_active = persisted.active_tab_id.clone();

        {
            let mut tabs = session_tabs.tabs.lock().unwrap();
            match tabs.get(&tab_id) {
                Some(pty) if pty.manager.is_alive() => {
                    if let Some(title) = title {
                        tabs.get_mut(&tab_id).expect("tab exists").title = title;
                    }
                    drop(tabs);
                    if activate || preferred_active.as_deref() == Some(&tab_id) {
                        session_tabs.set_active_tab(tab_id);
                    }
                    self.persist_session_metadata(&route.session_id);
                    return true;
                }
                Some(_) => {
                    tabs.remove(&tab_id);
                }
                None => {}
            }
        }

        let instance_id = terminal_instance_id(&route.session_id, &tab_id);
        let creation = TerminalTabCreation {
            title,
            created_at,
            activate,
            preferred_active,
        };
        self.create_pty(&route.session_id, &tab_id, &instance_id, cwd, creation)
    }

    /// 创建指定终端 Tab 的 PTY（调用方负责保证注册表里无该 Tab 的存活条目）。
    fn create_pty(
        &self,
        session_id: &str,
        tab_id: &str,
        instance_id: &str,
        cwd: &str,
        creation: TerminalTabCreation,
    ) -> bool {
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

        let mut manager = TerminalManager::new(instance_id.to_string(), effective_cwd.clone());

        let log_path = terminal_log_path(session_id, tab_id);
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
        // 每个终端 Tab 独立协作状态机：用户在该 Tab 打断 agent 不会影响其它 Tab
        let activity = Arc::new(TerminalActivityTracker::new());

        let pty_state =
            manager.start_and_spawn_reader(instance_id, &effective_cwd, self.app.clone());
        if pty_state.is_none() {
            error!(session_id, tab_id, "终端 Tab PTY 启动失败");
            return false;
        }

        let mgr = manager.clone();
        let app = self.app.clone();
        let sid = session_id.to_string();
        let tid = tab_id.to_string();
        let act = activity.clone();
        tauri::async_runtime::spawn(async move {
            spawn_command_loop(rx, mgr, app, pty_state, Some(act)).await;
            info!(session_id = %sid, tab_id = %tid, "终端 Tab PTY 命令循环退出");
        });

        let session_tabs = self.session_tabs(session_id);
        let mut tabs = session_tabs.tabs.lock().unwrap();
        tabs.insert(
            tab_id.to_string(),
            SessionPty {
                tab_id: tab_id.to_string(),
                title: creation.title.unwrap_or_else(|| "终端".to_string()),
                created_at: creation
                    .created_at
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(tiangong_types::now_text),
                manager,
                cmd_tx: tx,
                activity,
            },
        );
        drop(tabs);
        if creation.activate || creation.preferred_active.as_deref() == Some(tab_id) {
            session_tabs.set_active_tab(tab_id.to_string());
        }
        self.persist_session_metadata(session_id);
        info!(session_id, tab_id, "终端 Tab PTY 已创建");
        true
    }

    /// 获取指定 session / terminal id 的 PTY 槽位。
    pub fn get(&self, terminal_id: &str) -> Option<SessionPty> {
        let route = parse_terminal_id(terminal_id)?;
        let session_tabs = self.existing_session_tabs(&route.session_id)?;
        let tab_id = route
            .tab_id
            .or_else(|| session_tabs.active_or_first_tab_id())?;
        let tabs = session_tabs.tabs.lock().unwrap();
        tabs.get(&tab_id).cloned()
    }

    fn interactive_slot_for_session(&self, session_id: &str) -> Option<SessionPty> {
        let session_tabs = self.existing_session_tabs(session_id)?;
        let tabs = session_tabs.tabs.lock().unwrap();
        let active = session_tabs.active_tab_id.lock().unwrap().clone();

        if let Some(active) = active {
            if let Some(slot) = tabs.get(&active) {
                if slot.manager.is_alive()
                    && matches!(
                        slot.activity.busy_state(),
                        TerminalBusyState::AgentInteractive { .. }
                    )
                {
                    return Some(slot.clone());
                }
            }
        }

        tabs.values()
            .find(|slot| {
                slot.manager.is_alive()
                    && matches!(
                        slot.activity.busy_state(),
                        TerminalBusyState::AgentInteractive { .. }
                    )
            })
            .cloned()
    }

    pub fn list_tabs(&self, session_id: &str) -> crate::types::TerminalTabListResponse {
        let persisted =
            TerminalSessionStore::load_or_migrate_legacy(session_id).unwrap_or_else(|error| {
                warn!(%error, session_id, "加载终端会话状态失败");
                TerminalSessionPersisted::default()
            });
        let mut tab_infos = persisted
            .tabs
            .into_iter()
            .map(|tab| {
                (
                    tab.id.clone(),
                    crate::types::TerminalTabInfo {
                        id: tab.id,
                        title: tab.title,
                        created_at: tab.created_at,
                        alive: false,
                        cwd: String::new(),
                        shell: String::new(),
                        phase: "idle".to_string(),
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let mut active_tab_id = persisted.active_tab_id;

        if let Some(session_tabs) = self.existing_session_tabs(session_id) {
            let tabs = session_tabs.tabs.lock().unwrap();
            let active = session_tabs.active_tab_id.lock().unwrap().clone();
            for slot in tabs.values() {
                tab_infos.insert(
                    slot.tab_id.clone(),
                    crate::types::TerminalTabInfo {
                        id: slot.tab_id.clone(),
                        title: slot.title.clone(),
                        created_at: slot.created_at.clone(),
                        alive: slot.manager.is_alive(),
                        cwd: slot.manager.cwd(),
                        shell: slot.manager.shell(),
                        phase: slot.activity.busy_state().phase_label().to_string(),
                    },
                );
            }
            active_tab_id = active
                .filter(|active| tab_infos.contains_key(active))
                .or(active_tab_id);
        }
        let mut tab_infos = tab_infos.into_values().collect::<Vec<_>>();
        tab_infos.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let active_tab_id = active_tab_id
            .filter(|active| tab_infos.iter().any(|tab| tab.id == *active))
            .or_else(|| tab_infos.first().map(|tab| tab.id.clone()));
        crate::types::TerminalTabListResponse {
            tabs: tab_infos,
            active_tab_id,
        }
    }

    pub fn tab_new(
        &self,
        session_id: &str,
        title: Option<String>,
        cwd: Option<String>,
    ) -> Result<String, String> {
        if session_id.trim().is_empty() {
            return Err("终端 Tab 创建失败：session_id 为空".to_string());
        }
        let tab_id = scru128::new().to_string();
        let terminal_id = terminal_instance_id(session_id, &tab_id);
        let cwd = cwd.unwrap_or_default();
        if !self.ensure_with_title(&terminal_id, &cwd, title, true) {
            return Err(format!("终端 Tab PTY 启动失败：{session_id}:{tab_id}"));
        }
        self.emit_tab_updated(session_id, Some(tab_id.clone()), "user");
        Ok(tab_id)
    }

    pub fn tab_restore(
        &self,
        session_id: &str,
        tab_id: &str,
        title: Option<String>,
    ) -> Result<(), String> {
        if session_id.trim().is_empty() || tab_id.trim().is_empty() {
            return Err("终端 Tab 恢复失败：session_id 或 tab_id 为空".to_string());
        }
        let terminal_id = terminal_instance_id(session_id, tab_id);
        if !self.ensure_with_title(&terminal_id, "", title, false) {
            return Err(format!("终端 Tab PTY 恢复失败：{session_id}:{tab_id}"));
        }
        self.emit_tab_updated(session_id, Some(tab_id.to_string()), "restore");
        Ok(())
    }

    pub fn tab_switch(&self, session_id: &str, tab_id: &str) -> bool {
        let Some(session_tabs) = self.existing_session_tabs(session_id) else {
            return false;
        };
        if !session_tabs.tabs.lock().unwrap().contains_key(tab_id) {
            return false;
        }
        session_tabs.set_active_tab(tab_id.to_string());
        self.persist_session_metadata(session_id);
        self.emit_tab_updated(session_id, Some(tab_id.to_string()), "user");
        true
    }

    pub fn tab_close(&self, session_id: &str, tab_id: &str) -> Result<bool, String> {
        let existed = self.close_tab_persisted(session_id, tab_id)?;
        if existed {
            let next_active = self
                .existing_session_tabs(session_id)
                .and_then(|tabs| tabs.active_or_first_tab_id());
            self.emit_tab_updated(session_id, next_active, "user");
        }
        Ok(existed)
    }

    /// 为 Agent 命令执行选择终端：优先复用当前会话空闲终端，全部繁忙时创建新终端。
    pub fn select_for_command(
        &self,
        session_id: &str,
    ) -> Option<crate::capability::TerminalSelection> {
        let route = parse_terminal_id(session_id)?;
        let session_tabs = self.session_tabs(&route.session_id);

        if let Some(tab_id) = route.tab_id {
            let terminal_id = terminal_instance_id(&route.session_id, &tab_id);
            let slot = self.get(&terminal_id);
            if let Some(slot) = slot {
                if slot.manager.is_alive() {
                    if matches!(slot.activity.busy_state(), TerminalBusyState::Idle) {
                        session_tabs.set_active_tab(tab_id.clone());
                        self.persist_session_metadata(&route.session_id);
                        self.emit_tab_updated(
                            &route.session_id,
                            Some(tab_id.clone()),
                            "agent_command",
                        );
                        return Some(crate::capability::TerminalSelection {
                            session_id: route.session_id,
                            tab_id,
                            terminal_id,
                            created_new: false,
                            reason: crate::capability::TerminalSelectionReason::ReusedIdle,
                        });
                    }
                    // 指定 Tab 存活但正在忙，避开它，后续走空闲 Tab / 新建 Tab。
                } else {
                    if !self.ensure(&terminal_id, "") {
                        return None;
                    }
                    session_tabs.set_active_tab(tab_id.clone());
                    self.emit_tab_updated(&route.session_id, Some(tab_id.clone()), "agent_command");
                    return Some(crate::capability::TerminalSelection {
                        session_id: route.session_id,
                        tab_id,
                        terminal_id,
                        created_new: true,
                        reason: crate::capability::TerminalSelectionReason::NoAvailableTerminal,
                    });
                }
            } else {
                if !self.ensure(&terminal_id, "") {
                    return None;
                }
                session_tabs.set_active_tab(tab_id.clone());
                self.emit_tab_updated(&route.session_id, Some(tab_id.clone()), "agent_command");
                return Some(crate::capability::TerminalSelection {
                    session_id: route.session_id,
                    tab_id,
                    terminal_id,
                    created_new: true,
                    reason: crate::capability::TerminalSelectionReason::NoAvailableTerminal,
                });
            }
        }

        let mut had_live_terminal = false;
        let mut dead_tabs = Vec::new();
        let mut idle_tab_id = None;
        {
            let tabs = session_tabs.tabs.lock().unwrap();
            for (tab_id, slot) in tabs.iter() {
                if !slot.manager.is_alive() {
                    dead_tabs.push(tab_id.clone());
                    continue;
                }
                had_live_terminal = true;
                if matches!(slot.activity.busy_state(), TerminalBusyState::Idle) {
                    idle_tab_id = Some(tab_id.clone());
                    break;
                }
            }
        }

        if !dead_tabs.is_empty() {
            let mut tabs = session_tabs.tabs.lock().unwrap();
            for tab_id in dead_tabs {
                tabs.remove(&tab_id);
            }
        }

        if let Some(tab_id) = idle_tab_id {
            session_tabs.set_active_tab(tab_id.clone());
            self.persist_session_metadata(&route.session_id);
            self.emit_tab_updated(&route.session_id, Some(tab_id.clone()), "agent_command");
            return Some(crate::capability::TerminalSelection {
                session_id: route.session_id.clone(),
                tab_id: tab_id.clone(),
                terminal_id: terminal_instance_id(&route.session_id, &tab_id),
                created_new: false,
                reason: crate::capability::TerminalSelectionReason::ReusedIdle,
            });
        }

        let reason = if had_live_terminal {
            crate::capability::TerminalSelectionReason::AllBusy
        } else {
            crate::capability::TerminalSelectionReason::NoAvailableTerminal
        };
        let tab_id = scru128::new().to_string();
        let terminal_id = terminal_instance_id(&route.session_id, &tab_id);
        if !self.ensure(&terminal_id, "") {
            return None;
        }
        session_tabs.set_active_tab(tab_id.clone());
        self.emit_tab_updated(&route.session_id, Some(tab_id.clone()), "agent_command");
        Some(crate::capability::TerminalSelection {
            session_id: route.session_id,
            tab_id,
            terminal_id,
            created_new: true,
            reason,
        })
    }

    /// 销毁指定 session 或终端 Tab 的 PTY（drop cmd_tx → 命令循环退出 → 子进程终止）。
    pub fn destroy(&self, terminal_id: &str) {
        let Some(route) = parse_terminal_id(terminal_id) else {
            return;
        };
        if let Some(tab_id) = route.tab_id {
            if let Err(error) = self.close_tab_persisted(&route.session_id, &tab_id) {
                warn!(%error, session_id = %route.session_id, tab_id, "删除终端 Tab 失败");
            }
            return;
        }

        let slot = self.destroy_runtime_session(&route.session_id);
        let _guard = self.persistence.lock().unwrap();
        if let Err(error) = TerminalSessionStore::remove(&route.session_id) {
            warn!(%error, session_id = %route.session_id, "删除终端会话状态失败");
        }
        if slot.is_some() {
            info!(session_id = %route.session_id, "对话所有终端 Tab PTY 已销毁");
        }
    }

    /// 仅销毁运行中的 PTY，不删除插件持久化的 Tab 元数据。
    fn destroy_runtime_session(&self, session_id: &str) -> Option<Arc<SessionTabs>> {
        self.sessions.lock().unwrap().remove(session_id)
    }

    fn close_tab_persisted(&self, session_id: &str, tab_id: &str) -> Result<bool, String> {
        let _guard = self.persistence.lock().unwrap();
        let persisted = TerminalSessionStore::load_or_migrate_legacy(session_id)
            .map_err(|error| format!("加载终端会话状态失败：{error}"))?;
        let session_tabs = self.existing_session_tabs(session_id);
        let previous_active = session_tabs
            .as_ref()
            .and_then(|tabs| tabs.active_tab_id.lock().unwrap().clone());
        let removed = session_tabs.as_ref().and_then(|session_tabs| {
            let mut tabs = session_tabs.tabs.lock().unwrap();
            let removed = tabs.remove(tab_id);
            drop(tabs);
            let next_active = session_tabs.active_or_first_tab_id();
            *session_tabs.active_tab_id.lock().unwrap() = next_active;
            removed
        });

        let mut state = self.merge_runtime_metadata(session_id, persisted);
        let existed = removed.is_some() || state.tabs.iter().any(|tab| tab.id == tab_id);
        if !existed {
            return Ok(false);
        }
        state.tabs.retain(|tab| tab.id != tab_id);
        state.active_tab_id = state
            .active_tab_id
            .filter(|active| state.tabs.iter().any(|tab| tab.id == *active))
            .or_else(|| state.tabs.first().map(|tab| tab.id.clone()));
        if let Err(error) = TerminalSessionStore::save(session_id, &state) {
            if let (Some(session_tabs), Some(removed)) = (session_tabs.as_ref(), removed) {
                session_tabs
                    .tabs
                    .lock()
                    .unwrap()
                    .insert(tab_id.to_string(), removed);
            }
            if let Some(session_tabs) = session_tabs {
                *session_tabs.active_tab_id.lock().unwrap() = previous_active;
            }
            return Err(format!("持久化终端 Tab 删除失败：{error}"));
        }
        info!(session_id, tab_id, "终端 Tab 已删除");
        Ok(true)
    }

    /// 把临时草稿 id 的 PTY 迁移到真实 session_id（草稿态转正时调用）。
    ///
    /// 草稿态新对话用稳定临时 id 创建 PTY（如 `__draft_<n>`），首条消息创建后端
    /// session 拿到真实 id 后调用此方法完成迁移：
    /// - 注册表 key：临时 id → 真实 id
    /// - manager 内部 session_id：更新（命令循环/输出线程后续用新 id）
    /// - 插件 Tab 元数据：临时 id → 真实 id（先写目标，再删除草稿状态）
    ///
    /// 幂等：真实 id 已存在或临时 id 不存在时安全返回（不破坏已有状态）。
    /// 前端调用前应保证真实 id 尚未创建 PTY（否则会与转正迁移冲突）。
    pub fn attach_persistent_session_id(&self, draft_id: &str, persistent_id: &str) {
        if draft_id == persistent_id {
            return;
        }
        let draft_tabs = {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.remove(draft_id)
        };

        if let Some(draft_tabs) = draft_tabs {
            {
                let tabs = draft_tabs.tabs.lock().unwrap();
                for (tab_id, slot) in tabs.iter() {
                    let new_instance_id = terminal_instance_id(persistent_id, tab_id);
                    slot.manager.set_session_id(new_instance_id);
                }
            }

            let existing_tabs = {
                let sessions = self.sessions.lock().unwrap();
                sessions.get(persistent_id).cloned()
            };

            if let Some(existing_tabs) = existing_tabs {
                let mut existing = existing_tabs.tabs.lock().unwrap();
                let mut draft = draft_tabs.tabs.lock().unwrap();
                for (tab_id, slot) in draft.drain() {
                    existing.entry(tab_id).or_insert(slot);
                }
                if existing_tabs.active_tab_id.lock().unwrap().is_none() {
                    *existing_tabs.active_tab_id.lock().unwrap() =
                        draft_tabs.active_tab_id.lock().unwrap().clone();
                }
            } else {
                let mut sessions = self.sessions.lock().unwrap();
                match sessions.entry(persistent_id.to_string()) {
                    Entry::Vacant(entry) => {
                        entry.insert(draft_tabs);
                    }
                    Entry::Occupied(entry) => {
                        let existing_tabs = entry.get().clone();
                        drop(sessions);
                        let mut existing = existing_tabs.tabs.lock().unwrap();
                        let mut draft = draft_tabs.tabs.lock().unwrap();
                        for (tab_id, slot) in draft.drain() {
                            existing.entry(tab_id).or_insert(slot);
                        }
                        if existing_tabs.active_tab_id.lock().unwrap().is_none() {
                            *existing_tabs.active_tab_id.lock().unwrap() =
                                draft_tabs.active_tab_id.lock().unwrap().clone();
                        }
                    }
                }
            }
        }

        let _guard = self.persistence.lock().unwrap();
        let draft_state = match TerminalSessionStore::load(draft_id) {
            Ok(state) => state,
            Err(error) => {
                warn!(%error, draft_id, "加载草稿终端会话状态失败，保留原状态");
                return;
            }
        };
        let mut target_state = match TerminalSessionStore::load_or_migrate_legacy(persistent_id) {
            Ok(state) => state,
            Err(error) => {
                warn!(%error, persistent_id, "加载目标终端会话状态失败，保留草稿状态");
                return;
            }
        };
        let mut target_ids = target_state
            .tabs
            .iter()
            .map(|tab| tab.id.clone())
            .collect::<std::collections::HashSet<_>>();
        for tab in draft_state.tabs {
            if target_ids.insert(tab.id.clone()) {
                target_state.tabs.push(tab);
            }
        }
        if target_state.active_tab_id.is_none() {
            target_state.active_tab_id = draft_state.active_tab_id;
        }
        target_state = self.merge_runtime_metadata(persistent_id, target_state);
        if let Err(error) = TerminalSessionStore::save(persistent_id, &target_state) {
            warn!(%error, persistent_id, "保存目标终端会话状态失败，保留草稿状态");
            return;
        }
        if let Err(error) = TerminalSessionStore::remove(draft_id) {
            warn!(%error, draft_id, "删除草稿终端会话状态失败");
        }
        info!(
            draft_id,
            persistent_id, "草稿终端 Tabs 已转正迁移到真实 session"
        );
    }

    /// 列出所有 session 的状态摘要（phase 取自各对话的协作状态机）
    pub fn list_statuses(&self) -> Vec<crate::types::TerminalSessionStatus> {
        let sessions: Vec<_> = self.sessions.lock().unwrap().values().cloned().collect();
        let mut statuses = Vec::new();
        for session_tabs in sessions {
            let tabs = session_tabs.tabs.lock().unwrap();
            statuses.extend(
                tabs.values()
                    .map(|slot| crate::types::TerminalSessionStatus {
                        session_id: slot.manager.session_id(),
                        alive: slot.manager.is_alive(),
                        cwd: slot.manager.cwd(),
                        shell: slot.manager.shell(),
                        phase: slot.activity.busy_state().phase_label().to_string(),
                    }),
            );
        }
        statuses
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
        if session_id.trim().is_empty() {
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
        if session_id.trim().is_empty() {
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

    fn interactive_tx(&self, session_id: &str) -> Option<mpsc::Sender<TerminalCommand>> {
        let route = parse_terminal_id(session_id)?;
        if route.tab_id.is_some() {
            return self.tx(session_id);
        }
        // 只返回处于 AgentInteractive 态的 tab；找不到返回 None（不 fallback active/first）
        self.registry
            .interactive_slot_for_session(&route.session_id)
            .map(|slot| slot.cmd_tx)
    }
}

/// 系统 PTY 日志回填的最大行数
const DEFAULT_LOG_TAIL_LINES: usize = 5000;

/// 分对话、分终端 Tab 的 PTY 持久化日志路径。
fn terminal_log_path(session_id: &str, tab_id: &str) -> std::path::PathBuf {
    tiangong_config::io::storage_root()
        .join("sessions")
        .join(sanitize_path_segment(session_id))
        .join(format!("terminal-{}.log", sanitize_path_segment(tab_id)))
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

impl crate::capability::TerminalProvider for SessionAwareTerminalProvider {
    fn select_for_command(
        &self,
        session_id: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<crate::capability::TerminalSelection>> + Send>,
    > {
        let selection = self.registry.select_for_command(session_id);
        Box::pin(async move { selection })
    }

    fn exec(
        &self,
        session_id: &str,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<crate::capability::TerminalExecResult>> + Send>,
    > {
        let tx = match self.interactive_tx(session_id) {
            Some(tx) => tx,
            None => return Box::pin(async { None }),
        };
        let command = command.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let cancellation = Arc::new(crate::types::TerminalExecCancellation::default());
            let mut cancel_on_drop = CancelTerminalExecOnDrop::new(Arc::clone(&cancellation));
            let completion = crate::types::TerminalExecCompletion::new(Arc::clone(&cancellation));
            if tx
                .send(TerminalCommand::Exec {
                    command,
                    timeout_secs,
                    response_tx,
                    cancellation,
                    completion,
                })
                .await
                .is_err()
            {
                cancel_on_drop.disarm();
                return None;
            }
            cancel_on_drop.arm();
            let resp = tokio::time::timeout(std::time::Duration::from_secs(180), response_rx)
                .await
                .ok()?
                .ok()?;
            cancel_on_drop.disarm();
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
        Box<dyn std::future::Future<Output = Option<crate::capability::TerminalExecResult>> + Send>,
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
        Box<dyn std::future::Future<Output = Option<crate::capability::TerminalExecResult>> + Send>,
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
        Box<dyn std::future::Future<Output = Option<crate::capability::TerminalExecResult>> + Send>,
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
        Box<dyn std::future::Future<Output = Option<crate::capability::TerminalExecResult>> + Send>,
    > {
        // 优先路由到处于 AgentInteractive 态的 tab（即使它已不是 active tab），
        // 保证交互程序的后续输入发到正确的终端；找不到才回退到 active/first tab。
        let tx = match self.interactive_tx(session_id) {
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

struct CancelTerminalExecOnDrop {
    cancellation: Arc<crate::types::TerminalExecCancellation>,
    armed: bool,
}

impl CancelTerminalExecOnDrop {
    fn new(cancellation: Arc<crate::types::TerminalExecCancellation>) -> Self {
        Self {
            cancellation,
            armed: false,
        }
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelTerminalExecOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.request_and_wait();
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn dropping_accepted_exec_waits_for_command_loop_cleanup() {
        let cancellation = Arc::new(crate::types::TerminalExecCancellation::default());
        let mut guard = CancelTerminalExecOnDrop::new(Arc::clone(&cancellation));
        guard.arm();
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();

        let dropper = std::thread::spawn(move || {
            drop(guard);
            let _ = dropped_tx.send(());
        });
        while !cancellation.is_requested() {
            std::thread::yield_now();
        }
        assert!(
            dropped_rx
                .recv_timeout(std::time::Duration::from_millis(30))
                .is_err(),
            "cleanup acknowledgement 前不应释放执行 Future"
        );

        cancellation.mark_finished();
        dropped_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        dropper.join().unwrap();
    }

    #[tokio::test]
    async fn queued_exec_releases_barrier_when_receiver_closes() {
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(1);
        let cancellation = Arc::new(crate::types::TerminalExecCancellation::default());
        let completion = crate::types::TerminalExecCompletion::new(Arc::clone(&cancellation));
        let (response_tx, _response_rx) = tokio::sync::oneshot::channel();

        assert!(command_tx
            .send(TerminalCommand::Exec {
                command: "sleep 60".to_string(),
                timeout_secs: None,
                response_tx,
                cancellation: Arc::clone(&cancellation),
                completion,
            })
            .await
            .is_ok());

        let mut guard = CancelTerminalExecOnDrop::new(Arc::clone(&cancellation));
        guard.arm();

        // receiver 关闭会丢弃已入队 payload；payload 自身必须确认完成，
        // 否则调用 Future 的 Drop 会永久卡在取消栅栏上。
        drop(command_rx);
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(guard);
            let _ = dropped_tx.send(());
        });

        dropped_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("队列关闭后应立即释放取消栅栏");
        assert!(cancellation.is_requested());
        dropper.join().unwrap();
    }
}
