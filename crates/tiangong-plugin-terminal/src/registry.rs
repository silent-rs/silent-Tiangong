use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{error, info};

use crate::manager::{spawn_command_loop, TerminalManager};

/// 单个 session 的 PTY 槽位
pub struct SessionSlot {
    pub manager: Arc<TerminalManager>,
    pub cmd_tx: mpsc::Sender<crate::types::TerminalCommand>,
    pub(crate) activity: Arc<crate::collaboration::TerminalActivityTracker>,
}

/// 按对话管理的终端会话注册表
pub struct TerminalSessionRegistry {
    slots: Mutex<HashMap<String, SessionSlot>>,
    /// 当前终端面板显示的 session ID（None 表示面板关闭）
    panel_session: Mutex<Option<String>>,
    app: tauri::AppHandle,
    default_cwd: String,
}

impl TerminalSessionRegistry {
    pub fn new(app: tauri::AppHandle, default_cwd: String) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            panel_session: Mutex::new(None),
            app,
            default_cwd,
        }
    }

    /// 设置当前终端面板显示的 session（前端面板打开/关闭时调用）
    pub fn set_panel_session(&self, session_id: Option<&str>) {
        let mut panel = self.panel_session.lock().unwrap();
        *panel = session_id.map(|s| s.to_string());
    }

    /// 获取面板 session 的 slot（面板打开且 session 存在时返回）
    pub fn get_panel_slot(&self) -> Option<Arc<SessionSlot>> {
        let panel_id = self.panel_session.lock().unwrap().clone();
        panel_id.as_ref().and_then(|id| self.get_slot(id))
    }

    /// 懒加载：获取或创建指定 session 的 PTY
    pub fn ensure_slot(&self, session_id: &str, cwd: &str) -> bool {
        {
            let slots = self.slots.lock().unwrap();
            if slots.contains_key(session_id) {
                return true;
            }
        }

        let effective_cwd = if cwd.is_empty() {
            self.default_cwd.clone()
        } else {
            cwd.to_string()
        };

        let manager = Arc::new(TerminalManager::new(
            session_id.to_string(),
            effective_cwd.clone(),
        ));
        let (tx, rx) = mpsc::channel::<crate::types::TerminalCommand>(16);

        let pty_state =
            manager.start_and_spawn_reader(session_id, &effective_cwd, self.app.clone());
        if pty_state.is_none() {
            error!(session_id, "交互 PTY 启动失败");
            return false;
        }

        let mgr = manager.clone();
        let app = self.app.clone();
        let sid = session_id.to_string();
        let activity = Arc::new(crate::collaboration::TerminalActivityTracker::new());
        let activity_clone = activity.clone();
        tauri::async_runtime::spawn(async move {
            spawn_command_loop(rx, mgr, app, pty_state, Some(activity_clone)).await;
            info!(session_id = %sid, "交互 PTY 命令循环退出");
        });

        let mut slots = self.slots.lock().unwrap();
        slots.insert(
            session_id.to_string(),
            SessionSlot {
                manager,
                cmd_tx: tx,
                activity,
            },
        );
        info!(session_id, "交互 PTY 已创建");
        true
    }

    /// 获取指定 session 的 slot
    pub fn get_slot(&self, session_id: &str) -> Option<Arc<SessionSlot>> {
        // Slot 内部字段本身是 Arc，但这里返回一个包裹以便使用
        // 实际上直接返回引用即可，但 Tauri 命令是 async 的需要所有权
        let slots = self.slots.lock().unwrap();
        slots.get(session_id).map(|s| {
            Arc::new(SessionSlot {
                manager: s.manager.clone(),
                cmd_tx: s.cmd_tx.clone(),
                activity: s.activity.clone(),
            })
        })
    }

    /// 销毁指定 session 的 PTY
    pub fn destroy_slot(&self, session_id: &str) {
        let slot = {
            let mut slots = self.slots.lock().unwrap();
            slots.remove(session_id)
        };
        if let Some(slot) = slot {
            // drop cmd_tx 会使命令循环的 rx.recv() 返回 None，从而退出循环
            // PtyState 在命令循环退出时被 drop，子进程随之终止
            drop(slot);
            info!(session_id, "交互 PTY 已销毁");
        }
    }
}
