//! 父会话团队清单、独立子 Core 与团队工具协调。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

use serde_json::json;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::core_config::CoreConfig;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::permission::TrustMode;
use tiangong_core::session::{now_text, Message, MessageRole, Session};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};
use tiangong_types::{ContentBlock, StreamEvent};

use crate::child_runtime::{child_config, ChildPluginFactory, ChildRuntime, SharedFeedback};
use crate::constants::*;
use crate::manifest::{
    child_root, normalize_role, team_root, validate_role_identifier, AgentRecord, TeamManifest,
};
use crate::state::{AgentDescriptor, AgentStatus, FileLockManager};
use crate::tools::{child_tool_specs, error_result, ok_result};

const AGENT_DESCRIPTOR_MARKER: &str = "tiangong-agent-descriptor:";

fn active_teams() -> &'static Mutex<HashMap<String, Weak<Coordinator>>> {
    static ACTIVE_TEAMS: OnceLock<Mutex<HashMap<String, Weak<Coordinator>>>> = OnceLock::new();
    ACTIVE_TEAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) struct Coordinator {
    storage_root: PathBuf,
    child_plugins: Arc<dyn ChildPluginFactory>,
    manifest: Mutex<Option<TeamManifest>>,
    runtimes: Mutex<HashMap<String, Arc<ChildRuntime>>>,
    file_locks: Mutex<FileLockManager>,
    base_config: RwLock<CoreConfig>,
    workspace: RwLock<Option<PathBuf>>,
    feedback: SharedFeedback,
    stopping: AtomicBool,
}

/// 同步等待链被上游取消时，立即把取消继续传给正在等待的目标 Core。
struct CancelTargetOnDrop {
    runtime: Arc<ChildRuntime>,
    message_id: String,
    armed: bool,
}

impl CancelTargetOnDrop {
    fn new(runtime: Arc<ChildRuntime>, message_id: String) -> Self {
        Self {
            runtime,
            message_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelTargetOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.runtime.cancel_if_active(&self.message_id);
        }
    }
}

pub(crate) struct PendingDismissal {
    runtime: Arc<ChildRuntime>,
    descriptor: AgentDescriptor,
    role: String,
}

impl PendingDismissal {
    pub(crate) async fn finish(self) -> ToolResult {
        if let Err(error) = self.runtime.shutdown().await {
            return error_result(TOOL_DISMISS_AGENT, error);
        }
        ok_result(
            TOOL_DISMISS_AGENT,
            format!("{} 已解散", self.descriptor.label),
            format!("Agent '{}' 已关闭", self.descriptor.label),
            vec![self.role],
        )
    }
}

impl Coordinator {
    pub(crate) fn new(
        storage_root: PathBuf,
        child_plugins: Arc<dyn ChildPluginFactory>,
    ) -> Arc<Self> {
        Arc::new(Self {
            storage_root,
            child_plugins,
            manifest: Mutex::new(None),
            runtimes: Mutex::new(HashMap::new()),
            file_locks: Mutex::new(FileLockManager::new()),
            base_config: RwLock::new(CoreConfig::default()),
            workspace: RwLock::new(None),
            feedback: Arc::new(RwLock::new(None)),
            stopping: AtomicBool::new(false),
        })
    }

    pub(crate) fn set_feedback(&self, feedback: PluginFeedbackTx) {
        if let Ok(mut current) = self.feedback.write() {
            *current = Some(feedback);
        }
    }

    pub(crate) fn set_workspace(&self, workspace: Option<&Path>) {
        let next = workspace.map(Path::to_path_buf);
        let changed = self
            .workspace
            .write()
            .map(|mut current| {
                if *current == next {
                    false
                } else {
                    *current = next;
                    true
                }
            })
            .unwrap_or(false);
        if !changed {
            return;
        }
        for runtime in self.runtime_snapshot() {
            if let Err(error) = runtime.update_workspace(workspace) {
                tracing::warn!(agent_id = %runtime.descriptor().agent_id, %error, "同步子 Core 工作目录失败");
            }
        }
    }

    pub(crate) fn update_config(&self, config: &CoreConfig) {
        if let Ok(mut current) = self.base_config.write() {
            *current = config.clone();
        }
        for runtime in self.runtime_snapshot() {
            if let Err(error) = runtime.replace_base_config(config) {
                tracing::warn!(agent_id = %runtime.descriptor().agent_id, %error, "同步子 Core 配置失败");
            }
        }
    }

    pub(crate) fn initialize(self: &Arc<Self>, parent: &mut Session) {
        if let Err(error) = validate_path_segment(&parent.id, "父 Session ID") {
            self.notify_system_error(error);
            return;
        }
        let root = team_root(&self.storage_root, &parent.id);
        let manifest_existed = root.join("manifest.json").exists();
        let mut manifest = match TeamManifest::load(&root, &parent.id) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.notify_system_error(error);
                return;
            }
        };
        if !manifest_existed {
            migrate_legacy_manifest(&mut manifest, parent);
            if let Err(error) = manifest.persist(&root) {
                self.notify_system_error(error);
                return;
            }
        }
        let records = manifest.alive().cloned().collect::<Vec<_>>();
        if let Ok(mut current) = self.manifest.lock() {
            *current = Some(manifest);
        }
        if let Ok(mut teams) = active_teams().lock() {
            teams.insert(parent.id.clone(), Arc::downgrade(self));
        }
        for record in records {
            match self.start_record(parent, &record) {
                Ok(runtime) => {
                    if let Ok(mut runtimes) = self.runtimes.lock() {
                        runtimes.insert(record.descriptor.agent_id.clone(), runtime);
                    }
                    self.emit(StreamEvent::AgentCreated {
                        agent_id: record.descriptor.agent_id.clone(),
                        role: record.descriptor.role.clone(),
                        label: record.descriptor.label.clone(),
                    });
                    self.emit_status(&record.descriptor, "idle");
                }
                Err(error) => self.notify_system_error(format!(
                    "恢复子 Agent {} 失败：{error}",
                    record.descriptor.label
                )),
            }
        }
    }

    pub(crate) fn create_agent(
        self: &Arc<Self>,
        call: &ToolCall,
        parent: &mut Session,
    ) -> ToolResult {
        if self.stopping.load(Ordering::Acquire) {
            return error_result(TOOL_CREATE_AGENT, "Agent Team 正在关闭");
        }
        let role = match validate_role_identifier(argument_str(call, "role")) {
            Ok(role) => role,
            Err(error) => return error_result(TOOL_CREATE_AGENT, format!("role 无效：{error}")),
        };
        let label = argument_str(call, "label").trim().to_string();
        let system_prompt = argument_str(call, "system_prompt").trim().to_string();
        if label.is_empty() || system_prompt.is_empty() {
            return error_result(TOOL_CREATE_AGENT, "label 和 system_prompt 不能为空");
        }

        let (record, child_session) = {
            let mut guard = match self.manifest.lock() {
                Ok(guard) => guard,
                Err(_) => return error_result(TOOL_CREATE_AGENT, "团队清单状态锁定失败"),
            };
            let Some(manifest) = guard.as_mut() else {
                return error_result(TOOL_CREATE_AGENT, "团队尚未完成初始化");
            };
            if manifest.find_by_role(&role).is_some() {
                return error_result(TOOL_CREATE_AGENT, format!("角色 '{role}' 已被占用"));
            }
            if manifest.alive_count() >= MAX_AGENTS {
                return error_result(
                    TOOL_CREATE_AGENT,
                    format!("团队 Agent 数量已达上限（{MAX_AGENTS}）"),
                );
            }
            let mut child = Session::new(&label);
            let agent_id = child.id.clone();
            child.cwd = parent.cwd.clone();
            child.reasoning_effort = parent.reasoning_effort.clone();
            child.trust_mode = TrustMode::FullTrust;
            child.parent_session_id = Some(parent.id.clone());
            let record = AgentRecord {
                descriptor: AgentDescriptor {
                    agent_id: agent_id.clone(),
                    role: role.clone(),
                    label: label.clone(),
                    system_prompt,
                    status: AgentStatus::Idle,
                },
                topology_order: manifest.allocate_order(),
            };
            manifest.upsert(record.clone());
            let root = team_root(&self.storage_root, &parent.id);
            if let Err(error) = manifest.persist(&root) {
                manifest.mark_terminated(&agent_id);
                return error_result(TOOL_CREATE_AGENT, error);
            }
            (record, child)
        };

        let runtime = match self.start_runtime(parent, &record, child_session) {
            Ok(runtime) => runtime,
            Err(error) => {
                if let Err(persist_error) = self.mark_terminated(&record.descriptor.agent_id) {
                    tracing::warn!(%persist_error, "记录子 Agent 构造失败状态失败");
                }
                return error_result(TOOL_CREATE_AGENT, error);
            }
        };
        if let Ok(mut runtimes) = self.runtimes.lock() {
            runtimes.insert(record.descriptor.agent_id.clone(), runtime);
        }
        let descriptor_message = append_descriptor_message(parent, &record.descriptor);
        self.emit(StreamEvent::SessionMessageUpsert {
            message: descriptor_message,
            deferred_tool_injections: None,
        });
        self.emit(StreamEvent::AgentCreated {
            agent_id: record.descriptor.agent_id.clone(),
            role: role.clone(),
            label: label.clone(),
        });
        self.emit_status(&record.descriptor, "idle");
        ok_result(
            TOOL_CREATE_AGENT,
            format!("{label} (@{role}) 已加入团队"),
            format!(
                "Agent '{label}' 已创建，Session ID={}",
                record.descriptor.agent_id
            ),
            vec![role, label],
        )
    }

    pub(crate) async fn handle_tool(
        self: Arc<Self>,
        call: ToolCall,
        actor_id: String,
        feedback: PluginFeedbackTx,
    ) -> ToolResult {
        match call.name.as_str() {
            TOOL_SEND_MESSAGE => self.send_message(&call, &actor_id, feedback).await,
            TOOL_BROADCAST_MESSAGE => self.broadcast(&call, &actor_id, feedback).await,
            TOOL_NOTIFY_USER => self.notify_user(&call, &actor_id),
            TOOL_LOCK_FILE => self.lock_file(&call, &actor_id),
            TOOL_UNLOCK_FILE => self.unlock_file(&call, &actor_id),
            _ => error_result(&call.name, "未注册的 Agent Team 工具"),
        }
    }

    async fn send_message(
        self: &Arc<Self>,
        call: &ToolCall,
        actor_id: &str,
        feedback: PluginFeedbackTx,
    ) -> ToolResult {
        let target_role = normalize_role(argument_str(call, "to"));
        let content = argument_str(call, "content").trim().to_string();
        if target_role.is_empty() || content.is_empty() {
            return error_result(TOOL_SEND_MESSAGE, "to 和 content 不能为空");
        }
        if target_role == MAIN_ROLE {
            if self.is_parent_actor(actor_id) {
                return error_result(TOOL_SEND_MESSAGE, "父 Agent 不能向自己发送消息");
            }
            let from = self.descriptor(actor_id);
            let from_label = from
                .as_ref()
                .map(|descriptor| descriptor.label.clone())
                .unwrap_or_else(|| actor_id.to_string());
            self.emit(StreamEvent::AgentMessage {
                from_agent_id: actor_id.to_string(),
                from_agent_label: from_label.clone(),
                to_agent_id: self
                    .parent_session_id()
                    .unwrap_or_else(|| MAIN_ROLE.to_string()),
                to_agent_label: "Main Agent".to_string(),
                content: content.clone(),
            });
            if let Some(parent_feedback) = self.feedback() {
                parent_feedback.inject_tool(
                    "agent_team_message",
                    json!({
                        "from_agent_id": actor_id,
                        "from_agent_label": from_label,
                        "content": content,
                    }),
                );
            }
            return ok_result(
                TOOL_SEND_MESSAGE,
                "消息已异步发送给 Main Agent",
                "消息已送达 @main，不等待父 Core",
                vec![target_role],
            );
        }

        let target = match self.record_by_role(&target_role) {
            Some(record) => record,
            None => return error_result(TOOL_SEND_MESSAGE, format!("未找到角色 '{target_role}'")),
        };
        if target.descriptor.agent_id == actor_id {
            return error_result(TOOL_SEND_MESSAGE, "Agent 不能向自己发送消息");
        }
        if let Err(error) = self.ensure_wait_edge(actor_id, &target) {
            return error_result(TOOL_SEND_MESSAGE, error);
        }
        let Some(runtime) = self.runtime(&target.descriptor.agent_id) else {
            return error_result(TOOL_SEND_MESSAGE, "目标子 Core 不可用");
        };
        let sender = self.descriptor(actor_id);
        let sender_label = sender
            .as_ref()
            .map(|descriptor| descriptor.label.clone())
            .unwrap_or_else(|| "Main Agent".to_string());
        self.emit(StreamEvent::AgentMessage {
            from_agent_id: actor_id.to_string(),
            from_agent_label: sender_label,
            to_agent_id: target.descriptor.agent_id.clone(),
            to_agent_label: target.descriptor.label.clone(),
            content: content.clone(),
        });
        let prepared = vec![ContentBlock::text(format!(
            "[from:{} at {}]\n{}",
            sender
                .as_ref()
                .map(|descriptor| descriptor.role.as_str())
                .unwrap_or(MAIN_ROLE),
            now_text(),
            content
        ))];
        let message_id = stable_tool_message_id(actor_id, call, &target.descriptor.agent_id);
        let mut cancel_target = CancelTargetOnDrop::new(Arc::clone(&runtime), message_id.clone());
        let result = runtime
            .deliver_and_wait(message_id, prepared, feedback.clone())
            .await;
        cancel_target.disarm();
        match result {
            Ok(result) => {
                feedback.accumulate_token_usage(
                    result.usage,
                    format!("sub_agent:{}", target.descriptor.agent_id),
                );
                let output = result.assistant_text.unwrap_or_else(|| {
                    format!("{} 已完成本轮，但没有生成文本输出", target.descriptor.label)
                });
                if result.status == tiangong_types::TurnStatus::Success {
                    ok_result(
                        TOOL_SEND_MESSAGE,
                        format!("{} 已完成", target.descriptor.label),
                        output,
                        vec![target_role],
                    )
                } else {
                    error_result(TOOL_SEND_MESSAGE, result.error.unwrap_or(output))
                }
            }
            Err(error) => error_result(TOOL_SEND_MESSAGE, error),
        }
    }

    async fn broadcast(
        self: &Arc<Self>,
        call: &ToolCall,
        actor_id: &str,
        feedback: PluginFeedbackTx,
    ) -> ToolResult {
        let content = argument_str(call, "content").trim().to_string();
        if content.is_empty() {
            return error_result(TOOL_BROADCAST_MESSAGE, "content 不能为空");
        }
        let excluded = call
            .arguments
            .get("exclude")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(normalize_role)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let targets = self
            .alive_records()
            .into_iter()
            .filter(|record| record.descriptor.agent_id != actor_id)
            .filter(|record| !excluded.contains(&record.descriptor.role))
            .filter(|record| self.ensure_wait_edge(actor_id, record).is_ok())
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return error_result(TOOL_BROADCAST_MESSAGE, "没有可安全等待的目标 Agent");
        }
        let mut reports = Vec::new();
        for target in targets {
            let mut target_call = call.clone();
            target_call.name = TOOL_SEND_MESSAGE.to_string();
            target_call.arguments = json!({
                "to": target.descriptor.role,
                "content": content,
            });
            let result = self
                .send_message(&target_call, actor_id, feedback.clone())
                .await;
            reports.push(format!("{}: {}", target.descriptor.label, result.summary));
        }
        ok_result(
            TOOL_BROADCAST_MESSAGE,
            format!("广播已完成（{} 个 Agent）", reports.len()),
            reports.join("\n"),
            vec![content.chars().take(100).collect()],
        )
    }

    fn notify_user(&self, call: &ToolCall, actor_id: &str) -> ToolResult {
        let content = argument_str(call, "content").trim().to_string();
        if content.is_empty() {
            return error_result(TOOL_NOTIFY_USER, "content 不能为空");
        }
        let level = argument_str(call, "level").trim();
        let label = self
            .descriptor(actor_id)
            .map(|descriptor| descriptor.label)
            .unwrap_or_else(|| "Main Agent".to_string());
        self.emit(StreamEvent::AgentNotification {
            agent_id: actor_id.to_string(),
            agent_label: label,
            content: content.clone(),
            level: if level.is_empty() { "info" } else { level }.to_string(),
        });
        ok_result(
            TOOL_NOTIFY_USER,
            "通知已发送",
            "消息已推送给用户",
            vec![content.chars().take(100).collect()],
        )
    }

    fn lock_file(&self, call: &ToolCall, actor_id: &str) -> ToolResult {
        if self.is_parent_actor(actor_id) {
            return error_result(TOOL_LOCK_FILE, "Main Agent 不需要使用团队文件锁");
        }
        let path = match self.resolve_lock_path(argument_str(call, "path")) {
            Ok(path) => path,
            Err(error) => return error_result(TOOL_LOCK_FILE, error),
        };
        let now = chrono::Local::now().naive_local();
        let mut locks = match self.file_locks.lock() {
            Ok(locks) => locks,
            Err(_) => return error_result(TOOL_LOCK_FILE, "文件锁状态锁定失败"),
        };
        if let Err(error) = locks.try_lock(&path, actor_id, &now) {
            return error_result(TOOL_LOCK_FILE, error);
        }
        self.emit(StreamEvent::FileLockChanged {
            path: path.display().to_string(),
            holder_agent_id: Some(actor_id.to_string()),
            holder_agent_label: self.descriptor(actor_id).map(|descriptor| descriptor.label),
            action: "locked".to_string(),
        });
        ok_result(
            TOOL_LOCK_FILE,
            "文件锁已获取",
            format!("已锁定 {}", path.display()),
            vec![path.display().to_string()],
        )
    }

    fn unlock_file(&self, call: &ToolCall, actor_id: &str) -> ToolResult {
        let path = match self.resolve_lock_path(argument_str(call, "path")) {
            Ok(path) => path,
            Err(error) => return error_result(TOOL_UNLOCK_FILE, error),
        };
        let mut locks = match self.file_locks.lock() {
            Ok(locks) => locks,
            Err(_) => return error_result(TOOL_UNLOCK_FILE, "文件锁状态锁定失败"),
        };
        let holder_agent_id = locks.holder(&path).map(str::to_string);
        let result = if self.is_parent_actor(actor_id) {
            locks.force_unlock(&path);
            Ok(())
        } else {
            locks.unlock(&path, actor_id)
        };
        if let Err(error) = result {
            return error_result(TOOL_UNLOCK_FILE, error);
        }
        drop(locks);
        let holder_agent_label = holder_agent_id
            .as_deref()
            .and_then(|holder| self.descriptor(holder))
            .map(|descriptor| descriptor.label);
        self.emit(StreamEvent::FileLockChanged {
            path: path.display().to_string(),
            holder_agent_id,
            holder_agent_label,
            action: "unlocked".to_string(),
        });
        ok_result(
            TOOL_UNLOCK_FILE,
            "文件锁已释放",
            format!("已解锁 {}", path.display()),
            vec![path.display().to_string()],
        )
    }

    pub(crate) fn prepare_dismiss_agent(
        &self,
        call: &ToolCall,
        parent: &mut Session,
    ) -> Result<PendingDismissal, String> {
        let role = normalize_role(argument_str(call, "role"));
        let Some(record) = self.record_by_role(&role) else {
            return Err(format!("未找到角色 '{role}'"));
        };
        let Some(runtime) = self.runtime(&record.descriptor.agent_id) else {
            return Err("目标子 Core 不可用".to_string());
        };
        if !runtime.begin_closing() {
            return Err(format!("{} 正在运行或关闭", record.descriptor.label));
        }
        let removed = self
            .runtimes
            .lock()
            .ok()
            .and_then(|mut runtimes| runtimes.remove(&record.descriptor.agent_id));
        let Some(runtime) = removed else {
            self.rollback_target_closing(&runtime);
            return Err("目标子 Core 已被移除".to_string());
        };
        if let Err(error) = self.mark_terminated(&record.descriptor.agent_id) {
            self.rollback_target_closing(&runtime);
            if let Ok(mut runtimes) = self.runtimes.lock() {
                runtimes.insert(record.descriptor.agent_id.clone(), runtime);
            }
            return Err(error);
        }
        self.release_agent_locks(&record.descriptor);
        let status_message = append_agent_status_message(parent, &record.descriptor, "terminated");
        self.emit(StreamEvent::SessionMessageUpsert {
            message: status_message,
            deferred_tool_injections: None,
        });
        self.emit_status(&record.descriptor, "terminated");
        Ok(PendingDismissal {
            runtime,
            descriptor: record.descriptor,
            role,
        })
    }

    fn rollback_target_closing(&self, runtime: &ChildRuntime) {
        runtime.reopen();
    }

    /// 取消所有正在运行的子 Agent 当前执行，但保留团队结构。
    ///
    /// 与 [`shutdown`] 的区别：不设置 `stopping`、不清空 `runtimes`、不销毁子 Core。
    /// 用户取消主 turn 时调用，子 Agent 停止当前工作但团队仍可继续使用。
    pub(crate) fn cancel_all_running(&self) {
        for runtime in self.runtime_snapshot() {
            if runtime.is_running() {
                runtime.cancel();
            }
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(parent_session_id) = self.parent_session_id() {
            if let Ok(mut teams) = active_teams().lock() {
                let owns_entry = teams
                    .get(&parent_session_id)
                    .is_some_and(|team| std::ptr::eq(team.as_ptr(), self));
                if owns_entry {
                    teams.remove(&parent_session_id);
                }
            }
        }
        let runtimes = self
            .runtimes
            .lock()
            .map(|mut runtimes| std::mem::take(&mut *runtimes))
            .unwrap_or_default();
        for runtime in runtimes.values() {
            runtime.prepare_shutdown();
        }
        for (agent_id, runtime) in runtimes {
            if let Err(error) = runtime.shutdown().await {
                tracing::warn!(%agent_id, %error, "关闭子 Agent Core 失败");
            }
        }
    }

    pub(crate) fn roster_prompt(&self) -> String {
        let records = self.alive_records();
        if records.is_empty() {
            return "当前团队尚无子 Agent。".to_string();
        }
        let roster = records
            .into_iter()
            .map(|record| {
                format!(
                    "- @{}：{}（Session ID={}）",
                    record.descriptor.role, record.descriptor.label, record.descriptor.agent_id
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("当前团队成员：\n{roster}")
    }

    fn start_record(
        self: &Arc<Self>,
        parent: &Session,
        record: &AgentRecord,
    ) -> Result<Arc<ChildRuntime>, String> {
        validate_path_segment(&record.descriptor.agent_id, "Agent ID")?;
        let session = load_or_create_child_session(&self.storage_root, parent, record)?;
        self.start_runtime(parent, record, session)
    }

    fn start_runtime(
        self: &Arc<Self>,
        parent: &Session,
        record: &AgentRecord,
        session: Session,
    ) -> Result<Arc<ChildRuntime>, String> {
        let config = self
            .base_config
            .read()
            .map(|config| child_config(&config, &record.descriptor))
            .unwrap_or_else(|_| child_config(&CoreConfig::default(), &record.descriptor));
        ChildRuntime::start(
            record.descriptor.clone(),
            session,
            config,
            child_root(&self.storage_root, &parent.id, &record.descriptor.agent_id),
            self.storage_root.clone(),
            self.fresh_child_plugins(),
            Arc::new(ChildTeamClientPlugin::new(Arc::downgrade(self))),
            Arc::clone(&self.feedback),
        )
    }

    fn fresh_child_plugins(&self) -> Vec<Arc<dyn Plugin>> {
        // 子 Agent 与主 Agent 使用同一套插件工厂，直接获得独立但同构的插件实例。
        // ChildRuntime::start 会再剥离团队插件并注入团队客户端，这里无需额外过滤。
        self.child_plugins.create_plugins()
    }

    fn runtime(&self, agent_id: &str) -> Option<Arc<ChildRuntime>> {
        self.runtimes
            .lock()
            .ok()
            .and_then(|runtimes| runtimes.get(agent_id).cloned())
    }

    fn runtime_snapshot(&self) -> Vec<Arc<ChildRuntime>> {
        self.runtimes
            .lock()
            .map(|runtimes| runtimes.values().cloned().collect())
            .unwrap_or_default()
    }

    fn parent_session_id(&self) -> Option<String> {
        self.manifest.lock().ok().and_then(|manifest| {
            manifest
                .as_ref()
                .map(|manifest| manifest.parent_session_id.clone())
        })
    }

    fn is_parent_actor(&self, actor_id: &str) -> bool {
        self.parent_session_id().as_deref() == Some(actor_id)
    }

    fn descriptor(&self, agent_id: &str) -> Option<AgentDescriptor> {
        self.record(agent_id).map(|record| record.descriptor)
    }

    fn record(&self, agent_id: &str) -> Option<AgentRecord> {
        self.manifest
            .lock()
            .ok()
            .and_then(|manifest| manifest.as_ref()?.record(agent_id).cloned())
    }

    fn record_by_role(&self, role: &str) -> Option<AgentRecord> {
        self.manifest
            .lock()
            .ok()
            .and_then(|manifest| manifest.as_ref()?.find_by_role(role).cloned())
    }

    fn alive_records(&self) -> Vec<AgentRecord> {
        self.manifest
            .lock()
            .ok()
            .and_then(|manifest| {
                manifest
                    .as_ref()
                    .map(|manifest| manifest.alive().cloned().collect())
            })
            .unwrap_or_default()
    }

    fn ensure_wait_edge(&self, actor_id: &str, target: &AgentRecord) -> Result<(), String> {
        if self.is_parent_actor(actor_id) {
            return Ok(());
        }
        let source = self
            .record(actor_id)
            .ok_or_else(|| format!("未知消息发送方：{actor_id}"))?;
        if source.topology_order >= target.topology_order {
            return Err(format!(
                "拒绝可能形成循环等待的消息边：@{} → @{}",
                source.descriptor.role, target.descriptor.role
            ));
        }
        Ok(())
    }

    fn resolve_lock_path(&self, raw: &str) -> Result<PathBuf, String> {
        let workspace = self
            .workspace
            .read()
            .map_err(|_| "工作目录状态锁定失败".to_string())?
            .clone()
            .ok_or_else(|| "当前会话没有可用工作目录".to_string())?;
        let path = tiangong_toolkit::resolve_write_path_from_base(raw, &workspace)
            .map_err(|error| format!("文件路径无效：{error}"))?;
        Ok(path.canonicalize().unwrap_or(path))
    }

    fn mark_terminated(&self, agent_id: &str) -> Result<(), String> {
        let mut manifest = self
            .manifest
            .lock()
            .map_err(|_| "团队清单状态锁定失败".to_string())?;
        let manifest = manifest
            .as_mut()
            .ok_or_else(|| "团队尚未完成初始化".to_string())?;
        let before = manifest.clone();
        manifest.mark_terminated(agent_id);
        if let Err(error) =
            manifest.persist(&team_root(&self.storage_root, &manifest.parent_session_id))
        {
            *manifest = before;
            return Err(error);
        }
        Ok(())
    }

    fn release_agent_locks(&self, descriptor: &AgentDescriptor) {
        let released = self
            .file_locks
            .lock()
            .map(|mut locks| locks.release_all(&descriptor.agent_id))
            .unwrap_or_default();
        for path in released {
            self.emit(StreamEvent::FileLockChanged {
                path,
                holder_agent_id: Some(descriptor.agent_id.clone()),
                holder_agent_label: Some(descriptor.label.clone()),
                action: "unlocked".to_string(),
            });
        }
    }

    fn feedback(&self) -> Option<PluginFeedbackTx> {
        self.feedback
            .read()
            .ok()
            .and_then(|feedback| feedback.clone())
    }

    fn emit(&self, event: StreamEvent) {
        if let Some(feedback) = self.feedback() {
            if !feedback.send_turn_stream_event(event.clone()) {
                feedback.send_stream_event(event);
            }
        }
    }

    fn emit_status(&self, descriptor: &AgentDescriptor, status: &str) {
        self.emit(StreamEvent::AgentStatusChanged {
            agent_id: descriptor.agent_id.clone(),
            label: descriptor.label.clone(),
            status: status.to_string(),
        });
    }

    fn notify_system_error(&self, content: String) {
        self.emit(StreamEvent::AgentNotification {
            agent_id: "agent-team".to_string(),
            agent_label: "Agent Team".to_string(),
            content,
            level: "error".to_string(),
        });
    }

    pub(crate) fn cancel_registered(parent_session_id: &str, role: &str) -> bool {
        let coordinator = active_teams()
            .lock()
            .ok()
            .and_then(|teams| teams.get(parent_session_id).cloned())
            .and_then(|coordinator| coordinator.upgrade());
        coordinator.is_some_and(|coordinator| coordinator.cancel_agent(role))
    }

    fn cancel_agent(&self, role: &str) -> bool {
        let Some(record) = self.record_by_role(role) else {
            return false;
        };
        self.runtime(&record.descriptor.agent_id)
            .is_some_and(|runtime| runtime.is_running() && runtime.cancel())
    }
}

pub(crate) struct ChildTeamClientPlugin {
    coordinator: Weak<Coordinator>,
    feedback: RwLock<Option<PluginFeedbackTx>>,
}

impl ChildTeamClientPlugin {
    fn new(coordinator: Weak<Coordinator>) -> Self {
        Self {
            coordinator,
            feedback: RwLock::new(None),
        }
    }
}

impl ToolSpecProvider for ChildTeamClientPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        child_tool_specs()
    }
}

impl ToolOverrideHandler for ChildTeamClientPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &mut Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        if !child_tool_specs().iter().any(|spec| spec.name == call.name) {
            return Box::pin(async { None });
        }
        let coordinator = self.coordinator.upgrade();
        let feedback = self
            .feedback
            .read()
            .ok()
            .and_then(|feedback| feedback.clone())
            .map(PluginFeedbackTx::for_current_turn);
        let call = call.clone();
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let Some(coordinator) = coordinator else {
                return Some(error_result(&call.name, "所属 Agent Team 已关闭"));
            };
            let Some(feedback) = feedback else {
                return Some(error_result(&call.name, "Agent Team 反馈通道不可用"));
            };
            Some(coordinator.handle_tool(call, actor_id, feedback).await)
        })
    }
}

impl PromptSectionProvider for ChildTeamClientPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        vec![
            "团队协作：可用 send_message、broadcast_message、notify_user、lock_file、unlock_file。用户输入中的 @role 应调用 send_message，@all 应调用 broadcast_message。向 main 的消息只能异步发送；等待同级 Agent 时系统会拒绝可能形成循环的边。默认在父会话工作区中工作，不得读写或执行工作区外部的资源；修改工作区文件前必须先加锁。"
                .to_string(),
        ]
    }
}

impl Plugin for ChildTeamClientPlugin {
    fn id(&self) -> &str {
        CHILD_PLUGIN_ID
    }

    fn set_feedback_tx(&self, feedback: PluginFeedbackTx) {
        if let Ok(mut current) = self.feedback.write() {
            *current = Some(feedback);
        }
    }
}

fn load_or_create_child_session(
    storage_root: &Path,
    parent: &Session,
    record: &AgentRecord,
) -> Result<Session, String> {
    let root = child_root(storage_root, &parent.id, &record.descriptor.agent_id);
    let path = root
        .join("sessions")
        .join(format!("{}.json", record.descriptor.agent_id));
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!("子 Session 文件不得是符号链接：{}", path.display()));
        }
        Ok(_) => {
            let mut session = Session::load_from_storage(&root, &record.descriptor.agent_id)?;
            session.trust_mode = TrustMode::FullTrust;
            session.parent_session_id = Some(parent.id.clone());
            return Ok(session);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "检查子 Session 文件失败（{}）：{error}",
                path.display()
            ));
        }
    }
    let legacy_path = storage_root
        .join("sessions")
        .join(&parent.id)
        .join("agents")
        .join(format!("{}.json", record.descriptor.agent_id));
    let legacy_exists = match std::fs::symlink_metadata(&legacy_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "旧子 Session 文件不得是符号链接：{}",
                legacy_path.display()
            ));
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(format!(
                "检查旧子 Session 文件失败（{}）：{error}",
                legacy_path.display()
            ));
        }
    };
    if legacy_exists {
        let content = std::fs::read_to_string(&legacy_path)
            .map_err(|error| format!("读取旧子会话失败：{error}"))?;
        let mut session: Session =
            serde_json::from_str(&content).map_err(|error| format!("解析旧子会话失败：{error}"))?;
        session.id = record.descriptor.agent_id.clone();
        session.trust_mode = TrustMode::FullTrust;
        session.parent_session_id = Some(parent.id.clone());
        session.bind_storage_root(root);
        session.try_persist_to_disk()?;
        return Ok(session);
    }
    let mut child = Session::new(&record.descriptor.label);
    child.id = record.descriptor.agent_id.clone();
    child.cwd = parent.cwd.clone();
    child.reasoning_effort = parent.reasoning_effort.clone();
    child.trust_mode = TrustMode::FullTrust;
    child.parent_session_id = Some(parent.id.clone());
    child.bind_storage_root(root);
    Ok(child)
}

fn migrate_legacy_manifest(manifest: &mut TeamManifest, parent: &Session) {
    let terminated = parent
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::System)
        .filter_map(|message| {
            let text = message.text_content();
            text.contains("状态变更: terminated")
                .then(|| text.split("id=").nth(1).map(str::trim).map(str::to_string))
                .flatten()
        })
        .collect::<HashSet<_>>();
    for message in &parent.messages {
        if message.role != MessageRole::System {
            continue;
        }
        for block in &message.content {
            let ContentBlock::ModelInstruction { text } = block else {
                continue;
            };
            let Some(json) = text.strip_prefix(AGENT_DESCRIPTOR_MARKER) else {
                continue;
            };
            let Ok(mut descriptor) = serde_json::from_str::<AgentDescriptor>(json) else {
                continue;
            };
            if terminated.contains(&descriptor.agent_id)
                || descriptor.status == AgentStatus::Terminated
                || validate_path_segment(&descriptor.agent_id, "Agent ID").is_err()
            {
                continue;
            }
            let Ok(role) = validate_role_identifier(&descriptor.role) else {
                continue;
            };
            descriptor.role = role;
            descriptor.status = AgentStatus::Idle;
            let topology_order = manifest.allocate_order();
            manifest.upsert(AgentRecord {
                descriptor,
                topology_order,
            });
        }
    }
}

fn append_descriptor_message(parent: &mut Session, descriptor: &AgentDescriptor) -> Message {
    let mut message = Message::new(
        MessageRole::System,
        format!(
            "[Agent] {} ({}) 已加入团队 id={}",
            descriptor.label, descriptor.role, descriptor.agent_id
        ),
    );
    message.model_excluded = true;
    if let Ok(json) = serde_json::to_string(descriptor) {
        message
            .content
            .push(ContentBlock::model_instruction(format!(
                "{AGENT_DESCRIPTOR_MARKER}{json}"
            )));
    }
    parent.messages.push(message.clone());
    message
}

fn append_agent_status_message(
    parent: &mut Session,
    descriptor: &AgentDescriptor,
    status: &str,
) -> Message {
    let message_id = format!("agent-status:{}", descriptor.agent_id);
    let mut message = Message::new(
        MessageRole::System,
        format!(
            "[Agent] {} 状态变更: {status} id={}",
            descriptor.label, descriptor.agent_id
        ),
    );
    message.id = message_id.clone();
    message.model_excluded = true;
    if let Some(existing) = parent
        .messages
        .iter_mut()
        .find(|existing| existing.id == message_id)
    {
        *existing = message.clone();
    } else {
        parent.messages.push(message.clone());
    }
    message
}

fn stable_tool_message_id(actor_id: &str, call: &ToolCall, target_id: &str) -> String {
    format!("agent-team-tool:{actor_id}:{}:{target_id}", call.id)
}

fn argument_str<'a>(call: &'a ToolCall, key: &str) -> &'a str {
    call.arguments
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

fn validate_path_segment(value: &str, label: &str) -> Result<(), String> {
    let mut components = Path::new(value).components();
    if matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
    {
        Ok(())
    } else {
        Err(format!("{label} 必须是单个安全路径片段"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_factory() -> Arc<dyn ChildPluginFactory> {
        Arc::new(Vec::<Arc<dyn Plugin>>::new)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn create_agent_uses_session_id_as_agent_id_and_persists_under_teams() {
        let _guard = crate::test_support::storage_test_guard_async().await;
        let storage = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::new(storage.path().to_path_buf(), empty_factory());
        let mut parent = Session::new("parent");
        parent.cwd = storage.path().to_string_lossy().into_owned();
        coordinator.initialize(&mut parent);

        let call = ToolCall {
            id: "create-call".to_string(),
            name: TOOL_CREATE_AGENT.to_string(),
            arguments: json!({
                "role": "dev",
                "label": "Developer",
                "system_prompt": "实现功能"
            }),
        };
        let result = coordinator.create_agent(&call, &mut parent);
        assert!(result.ok, "{}", result.stderr);

        let record = coordinator.record_by_role("dev").unwrap();
        let descriptor_message = parent
            .messages
            .iter()
            .find(|message| {
                message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ModelInstruction { text } if text.starts_with(AGENT_DESCRIPTOR_MARKER)))
            })
            .expect("创建 Agent 应追加可恢复的描述消息");
        assert_eq!(
            descriptor_message.text_content(),
            format!(
                "[Agent] Developer (dev) 已加入团队 id={}",
                record.descriptor.agent_id
            )
        );
        let session_path = child_root(storage.path(), &parent.id, &record.descriptor.agent_id)
            .join("sessions")
            .join(format!("{}.json", record.descriptor.agent_id));
        assert!(session_path.is_file());
        let restored = Session::load_from_storage(
            session_path.parent().unwrap().parent().unwrap(),
            &record.descriptor.agent_id,
        )
        .unwrap();
        assert_eq!(restored.id, record.descriptor.agent_id);

        let dismiss_call = ToolCall {
            id: "dismiss-call".to_string(),
            name: TOOL_DISMISS_AGENT.to_string(),
            arguments: json!({ "role": "dev" }),
        };
        let result = coordinator
            .prepare_dismiss_agent(&dismiss_call, &mut parent)
            .expect("空闲 Agent 应允许解散")
            .finish()
            .await;
        assert!(result.ok, "{}", result.stderr);
        assert!(coordinator.runtime(&record.descriptor.agent_id).is_none());
        assert!(parent.messages.iter().any(|message| {
            message.id == format!("agent-status:{}", record.descriptor.agent_id)
                && message.text_content()
                    == format!(
                        "[Agent] {} 状态变更: terminated id={}",
                        record.descriptor.label, record.descriptor.agent_id
                    )
        }));

        coordinator.shutdown().await;
    }
}
