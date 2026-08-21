use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::model::TokenUsage;
use crate::permission::TrustMode;
use crate::planner::{PlanItem, PlanStepSource, PlanStepStatus};

pub use tiangong_types::{
    ContentBlock, DeferredToolInjection, MediaAsset, MediaKind, Message, MessagePhase, MessageRole,
    MessageToolCall, StoredAsset, now_text,
};

/// 同一进程内的持久化写入共用此锁，避免 Core 与宿主同时替换会话文件。
static PERSISTENCE_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 在目标文件所在目录写入临时文件，并原子替换目标文件。
///
/// 所有调用共享同一把进程内锁。临时文件与目标文件位于同一文件系统，
/// `NamedTempFile::persist` 会在 Windows、macOS 与 Linux 上替换已有目标文件。
pub fn atomic_replace_file(path: &Path, content: &[u8]) -> io::Result<()> {
    let _write_guard = PERSISTENCE_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let mut temp_file = tempfile::NamedTempFile::new_in(parent)?;
    temp_file.as_file_mut().write_all(content)?;
    temp_file.as_file().sync_all()?;
    temp_file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// 会话工作目录模式
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionCwdMode {
    /// 继承全局工作目录（桌面端默认）
    #[default]
    Inherit,
    /// 隔离模式：在 ~/.tiangong/workspaces/{session_id}/ 下创建独立目录
    Isolated,
    /// 用户手动指定
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
    /// 当前会话累计 token 用量。
    ///
    /// `RunSnapshot.last_usage` 是运行时快照字段，不能跨会话复用；会话级累计值
    /// 存在这里，供 GUI 切换会话时恢复原先统计。
    #[serde(default)]
    pub token_usage: TokenUsage,
    /// 最近一次主对话 LLM 请求的 prompt token 数，用于展示当前上下文大小。
    #[serde(default)]
    pub current_tokens: usize,
    /// 当前模型配置下触发上下文压缩的 token 阈值。
    #[serde(skip)]
    pub compression_threshold_tokens: usize,
    /// 当前模型配置下的上下文窗口上限。
    #[serde(skip)]
    pub context_limit_tokens: usize,
    /// 当前活跃 sub agent 的上下文 token 数
    #[serde(default)]
    pub active_agent_current_tokens: usize,
    /// 当前活跃 sub agent ID（None 表示主对话执行中）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_agent_id: Option<String>,
    /// agent_id → 最近一次上下文 token 数，用于 GUI 按 Agent Tab 切换展示。
    #[serde(default)]
    pub agent_current_tokens: HashMap<String, usize>,
    /// agent_id → 累计 token 用量，用于 GUI 按 Agent Tab 切换展示。
    #[serde(default)]
    pub agent_token_usage: HashMap<String, TokenUsage>,
    #[serde(default)]
    pub task_records: Vec<SessionTaskRecord>,
    #[serde(default)]
    pub task_plans: Vec<SessionTaskPlan>,
    /// 会话级工作目录，工具执行时以此为根目录
    #[serde(default)]
    pub cwd: String,
    /// 工作目录模式
    #[serde(default)]
    pub cwd_mode: SessionCwdMode,
    /// 会话级信任模式；应用级默认值只在新建会话时复制到这里。
    #[serde(default)]
    pub trust_mode: TrustMode,
    /// 会话级思考强度；为空时使用应用级默认值。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::model::deserialize_reasoning_effort_option_flexible"
    )]
    pub reasoning_effort: Option<crate::model::ReasoningEffort>,
    /// 早期对话的滚动摘要（用于无限上下文压缩）
    ///
    /// 当对话历史超过模型上下文阈值时，早期消息被 LLM 压缩为摘要存储在此。
    /// 构建 prompt 时注入为系统消息，原始 messages 保持完整供 UI 展示。
    /// 每次压缩会将旧摘要 + 新溢出消息折叠为新摘要，支持无限延伸。
    #[serde(default)]
    pub context_summary: Option<String>,
    /// 摘要覆盖到的消息索引（messages[0..summary_up_to] 已被摘要覆盖）
    #[serde(default)]
    pub summary_up_to: usize,
    /// 缓存的 system prompt 消息（role=System）。
    ///
    /// 在新对话、压缩对话、清空上下文时由外部调用 `rebuild_system_prompt()` 重建。
    /// `context()` 返回时会将其置于消息列表头部，由 `build_provider_messages()` 提取。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_message: Option<Message>,
    pub created_at: String,
    pub updated_at: String,
    /// 父会话 ID（Worker 子会话标注所属的父会话）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// 工具调用批次闭合前收到的外部工具输入；下一安全边界按顺序注入。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_tool_injections: Vec<DeferredToolInjection>,
    /// 当前 Session 独立的持久化根，仅用于运行时，不写入会话 JSON。
    #[serde(skip)]
    storage_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionTaskStatus {
    /// 排队等待执行
    Queued,
    #[default]
    Planning,
    Executing,
    /// 阻塞（等待外部依赖）
    Blocked,
    /// 后台运行
    Backgrounded,
    Completed,
    Failed,
    /// 已取消
    Cancelled,
}

/// Worker 执行结果记录（持久化到 SessionTaskRecord）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkerResultRecord {
    pub worker_id: String,
    pub worker_label: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionTaskRecord {
    pub task_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub user_input: String,
    pub status: SessionTaskStatus,
    pub summary: String,
    #[serde(default)]
    pub plan_snapshot: Option<String>,
    #[serde(default)]
    pub tool_result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// 本次任务所有 LLM 调用的累计 token 用量
    #[serde(default)]
    pub usage: Option<TokenUsage>,
    /// 开发阶段：记录该任务所有 LLM 调用的完整参数和响应
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub llm_calls: Vec<LlmCallRecord>,
    /// 多 Worker 模式下各 Worker 的执行结果
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worker_results: Vec<WorkerResultRecord>,
}

/// LLM 调用完整记录（开发调试用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmCallRecord {
    /// 调用阶段（如 intent-classify, direct-answer, react-round-1）
    pub stage: String,
    /// 发送给 LLM 的系统/用户 prompt
    pub prompt: String,
    /// 上下文消息数量
    pub context_count: usize,
    /// 注入的工具名列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_names: Vec<String>,
    /// LLM 回复文本
    pub response_text: String,
    /// 思考内容长度
    #[serde(default)]
    pub reasoning_len: usize,
    /// LLM 发起的工具调用
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<String>,
    /// Token 用量
    pub usage: TokenUsage,
    /// 调用时间戳
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPlanExecutionStep {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: PlanStepStatus,
    #[serde(default)]
    pub source: PlanStepSource,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTaskPlan {
    pub id: String,
    pub task_id: String,
    pub name: String,
    pub description: String,
    pub status: PlanStepStatus,
    #[serde(default)]
    pub execution_summary: Option<String>,
    #[serde(default)]
    pub execution_steps: Vec<SessionPlanExecutionStep>,
    pub created_at: String,
    pub updated_at: String,
}

impl Session {
    pub fn has_user_messages(&self) -> bool {
        self.messages.iter().any(|m| m.role == MessageRole::User)
    }

    pub fn new(title: impl Into<String>) -> Self {
        let now = now_text();
        Self {
            id: new_id(),
            title: title.into(),
            messages: Vec::new(),
            token_usage: TokenUsage::default(),
            current_tokens: 0,
            compression_threshold_tokens: 0,
            context_limit_tokens: 0,
            active_agent_current_tokens: 0,
            active_agent_id: None,
            agent_current_tokens: HashMap::new(),
            agent_token_usage: HashMap::new(),
            task_records: Vec::new(),
            task_plans: Vec::new(),
            cwd: String::new(),
            cwd_mode: SessionCwdMode::Inherit,
            trust_mode: TrustMode::default(),
            reasoning_effort: None,
            context_summary: None,
            summary_up_to: 0,
            system_prompt_message: None,
            created_at: now.clone(),
            updated_at: now,
            parent_session_id: None,
            deferred_tool_injections: Vec::new(),
            storage_root: None,
        }
    }

    /// 创建隔离模式的会话（用于 Connector 接入）
    pub fn new_isolated(title: impl Into<String>, storage_root: &std::path::Path) -> Self {
        let id = new_id();
        let now = now_text();
        // 在 {storage_root}/workspaces/{session_id}/ 下创建独立目录
        let workspace_dir = storage_root.join("workspaces").join(&id);
        let _ = std::fs::create_dir_all(&workspace_dir);
        Self {
            id,
            title: title.into(),
            messages: Vec::new(),
            token_usage: TokenUsage::default(),
            current_tokens: 0,
            compression_threshold_tokens: 0,
            context_limit_tokens: 0,
            active_agent_current_tokens: 0,
            active_agent_id: None,
            agent_current_tokens: HashMap::new(),
            agent_token_usage: HashMap::new(),
            task_records: Vec::new(),
            task_plans: Vec::new(),
            cwd: workspace_dir.to_string_lossy().to_string(),
            cwd_mode: SessionCwdMode::Isolated,
            trust_mode: TrustMode::default(),
            reasoning_effort: None,
            context_summary: None,
            summary_up_to: 0,
            system_prompt_message: None,
            created_at: now.clone(),
            updated_at: now,
            parent_session_id: None,
            deferred_tool_injections: Vec::new(),
            storage_root: None,
        }
    }

    /// 将该 Session 的持久化固定到指定根目录。
    pub fn bind_storage_root(&mut self, storage_root: impl Into<PathBuf>) {
        self.storage_root = Some(storage_root.into());
    }

    /// 绑定独立持久化根并返回 Session。
    pub fn with_storage_root(mut self, storage_root: impl Into<PathBuf>) -> Self {
        self.bind_storage_root(storage_root);
        self
    }

    /// 当前绑定的独立持久化根。
    pub fn bound_storage_root(&self) -> Option<&Path> {
        self.storage_root.as_deref()
    }

    /// 从指定存储根加载 Session，并保留该根作为后续持久化位置。
    pub fn load_from_storage(storage_root: &Path, session_id: &str) -> Result<Self, String> {
        let mut components = Path::new(session_id).components();
        let valid_id = matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none();
        if !valid_id {
            return Err("会话 ID 必须是单个安全路径片段".to_string());
        }

        let path = storage_root
            .join("sessions")
            .join(format!("{session_id}.json"));
        let content = std::fs::read_to_string(&path)
            .map_err(|error| format!("session 读取失败（{}）：{error}", path.display()))?;
        let mut session: Self = serde_json::from_str(&content)
            .map_err(|error| format!("session 反序列化失败（{}）：{error}", path.display()))?;
        if session.id != session_id {
            return Err(format!(
                "session 文件 ID 不匹配：期望 {session_id}，实际 {}",
                session.id
            ));
        }
        session.bind_storage_root(storage_root.to_path_buf());
        Ok(session)
    }

    /// 将 session 持久化到 `~/.tiangong/sessions/{id}.json`
    ///
    /// Core 在工具调用等关键节点调用此方法，确保中间数据不会因崩溃丢失。
    pub fn persist_to_disk(&self) {
        if let Err(err) = self.try_persist_to_disk() {
            tracing::warn!(error = %err, "session 持久化失败");
        }
    }

    /// 尝试将稳定会话状态持久化到磁盘，并把失败返回给调用方。
    /// 图片块中的瞬时 `data` 由类型合同保证永不序列化。
    pub fn try_persist_to_disk(&self) -> Result<(), String> {
        #[cfg(test)]
        if crate::core::test_support::is_persistence_persistently_failing(&self.id)
            || crate::core::test_support::take_persistence_failure_for_session(&self.id)
        {
            return Err(format!("测试注入的 session 持久化失败（{}）", self.id));
        }

        let storage_root = self.storage_root.as_ref().ok_or_else(|| {
            "session 未绑定 storage_root（创建或加载 Session 时必须绑定）".to_string()
        })?;
        let path = storage_root
            .join("sessions")
            .join(format!("{}.json", self.id));
        let content = serde_json::to_string_pretty(self)
            .map_err(|err| format!("session 序列化失败：{err}"))?;
        atomic_replace_file(&path, content.as_bytes())
            .map_err(|err| format!("session 持久化写入失败：{err}"))
    }

    pub fn append_message(&mut self, role: MessageRole, content: impl Into<String>) {
        self.append_message_with_reasoning(role, content, String::new());
    }

    pub fn append_message_with_media(
        &mut self,
        role: MessageRole,
        content: impl Into<String>,
        media: Vec<tiangong_types::MediaAsset>,
    ) {
        let mut blocks = vec![ContentBlock::text(content.into())];
        for asset in &media {
            blocks.push(asset.to_content_block());
        }
        self.messages.push(Message {
            id: new_id(),
            role,
            content: blocks,
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            elapsed_ms: None,
            turn_status: None,
            reasoning_elapsed_ms: None,
            text_elapsed_ms: None,
            duration_ms: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            phase: crate::session::MessagePhase::Normal,
            created_at: now_text(),
        });
    }

    pub fn append_message_with_reasoning(
        &mut self,
        role: MessageRole,
        content: impl Into<String>,
        reasoning_content: impl Into<String>,
    ) {
        self.messages.push(Message {
            id: new_id(),
            role,
            content: vec![ContentBlock::text(content.into())],
            reasoning_content: reasoning_content.into(),
            reasoning_signature: None,
            worker_id: None,
            elapsed_ms: None,
            turn_status: None,
            reasoning_elapsed_ms: None,
            text_elapsed_ms: None,
            duration_ms: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            phase: crate::session::MessagePhase::Normal,
            created_at: now_text(),
        });
    }

    /// 使用预生成的 ID 追加消息（流式场景：Delta/Reasoning 事件先于消息创建）
    pub fn append_message_with_id(
        &mut self,
        id: String,
        role: MessageRole,
        content: impl Into<String>,
        reasoning_content: impl Into<String>,
    ) {
        self.append_message_with_id_and_media(id, role, content, reasoning_content, Vec::new());
    }

    /// 使用预生成的 ID 追加带结构化媒体的消息。
    pub fn append_message_with_id_and_media(
        &mut self,
        id: String,
        role: MessageRole,
        content: impl Into<String>,
        reasoning_content: impl Into<String>,
        media: Vec<tiangong_types::MediaAsset>,
    ) {
        let mut blocks = vec![ContentBlock::text(content.into())];
        for asset in &media {
            blocks.push(asset.to_content_block());
        }
        self.messages.push(Message {
            id,
            role,
            content: blocks,
            reasoning_content: reasoning_content.into(),
            reasoning_signature: None,
            worker_id: None,
            elapsed_ms: None,
            turn_status: None,
            reasoning_elapsed_ms: None,
            text_elapsed_ms: None,
            duration_ms: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            phase: crate::session::MessagePhase::Normal,
            created_at: now_text(),
        });
    }

    /// 使用预生成 ID 原样追加宿主准备好的用户消息。
    pub fn append_prepared_user_message_with_id(&mut self, id: String, content: Vec<ContentBlock>) {
        self.messages.push(Message {
            id,
            role: MessageRole::User,
            content,
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            elapsed_ms: None,
            turn_status: None,
            reasoning_elapsed_ms: None,
            text_elapsed_ms: None,
            duration_ms: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            phase: crate::session::MessagePhase::Normal,
            created_at: now_text(),
        });
        self.updated_at = now_text();
    }

    /// 校验并事务性写入 Core 已接收的用户消息。
    ///
    /// 同 ID 的宿主镜像消息会先移除；只有完整 Session 成功落盘后才返回。失败时
    /// 恢复调用前的 Session。上一轮遗留工具调用由该轮取消收尾负责闭合。
    pub(crate) fn try_append_prepared_user_message_with_id(
        &mut self,
        id: String,
        content: Vec<ContentBlock>,
    ) -> Result<(), String> {
        tiangong_types::validate_ready_content_blocks(&content)?;

        if self
            .messages
            .iter()
            .any(|message| message.id == id && message.role != MessageRole::User)
        {
            return Err(format!("消息 ID {id} 已被非用户消息占用"));
        }

        let before = self.clone();
        self.messages
            .retain(|message| message.id != id || message.role != MessageRole::User);
        self.append_prepared_user_message_with_id(id, content);

        if let Err(error) = self.try_persist_to_disk() {
            *self = before;
            return Err(error);
        }

        Ok(())
    }

    /// 补齐未完成工具调用的失败结果，并在有变更时立即落盘。
    ///
    /// 补齐结果落盘失败时，删除新增结果和对应的悬空调用后再次落盘，避免后续
    /// 请求读取到不完整的工具调用协议。二次落盘仍失败时保留清理后的内存状态，
    /// 供 turn 最终持久化继续重试。
    pub(crate) fn close_unfinished_tool_calls_with_reason(
        &mut self,
        reason: &str,
    ) -> Vec<(String, String, String)> {
        let Some((assistant_index, assistant)) = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| !message.tool_calls.is_empty())
        else {
            return Vec::new();
        };
        let completed = self.messages[assistant_index + 1..]
            .iter()
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect::<std::collections::HashSet<_>>();
        let unfinished = assistant
            .tool_calls
            .iter()
            .filter(|call| !completed.contains(call.id.as_str()))
            .map(|call| (call.id.clone(), call.name.clone()))
            .collect::<Vec<_>>();
        if unfinished.is_empty() {
            return Vec::new();
        }

        let message_count_before_close = self.messages.len();
        let interrupted = unfinished
            .into_iter()
            .map(|(tool_call_id, tool_name)| {
                let output = reason.to_string();
                self.messages.push(
                    Message::tool_result(&tool_call_id, &tool_name, &output, true)
                        .with_phase(MessagePhase::React),
                );
                (tool_call_id, tool_name, output)
            })
            .collect::<Vec<_>>();
        self.updated_at = now_text();
        if let Err(close_error) = self.try_persist_to_disk() {
            self.messages.truncate(message_count_before_close);
            let interrupted_ids = interrupted
                .iter()
                .map(|(tool_call_id, _, _)| tool_call_id.as_str())
                .collect::<std::collections::HashSet<_>>();
            self.messages[assistant_index]
                .tool_calls
                .retain(|call| !interrupted_ids.contains(call.id.as_str()));
            self.updated_at = now_text();

            if let Err(remove_error) = self.try_persist_to_disk() {
                tracing::error!(
                    close_error = %close_error,
                    remove_error = %remove_error,
                    count = interrupted.len(),
                    "补齐未完成工具调用及删除悬空调用均持久化失败"
                );
                return Vec::new();
            }

            tracing::warn!(
                error = %close_error,
                count = interrupted.len(),
                "补齐未完成工具调用持久化失败，已删除悬空调用并重新落盘"
            );
            return Vec::new();
        }

        interrupted
    }

    pub(crate) fn has_unfinished_tool_calls(&self) -> bool {
        !self.unfinished_tool_calls().is_empty()
    }

    fn unfinished_tool_calls(&self) -> Vec<(String, String)> {
        let Some((assistant_index, assistant)) = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| !message.tool_calls.is_empty())
        else {
            return Vec::new();
        };
        let completed = self.messages[assistant_index + 1..]
            .iter()
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect::<std::collections::HashSet<_>>();
        assistant
            .tool_calls
            .iter()
            .filter(|call| !completed.contains(call.id.as_str()))
            .map(|call| (call.id.clone(), call.name.clone()))
            .collect()
    }

    /// 用宿主准备好的内容原样替换已有用户消息。
    pub fn update_prepared_user_message(
        &mut self,
        message_id: &str,
        content: Vec<ContentBlock>,
    ) -> bool {
        let Some(message) = self
            .messages
            .iter_mut()
            .find(|message| message.id == message_id && message.role == MessageRole::User)
        else {
            return false;
        };
        message.content = content;
        self.updated_at = now_text();
        true
    }

    pub(crate) fn clear_transient_content(&mut self) {
        for message in &mut self.messages {
            message.clear_transient_data();
        }
    }

    /// 清除指定消息的瞬时图片数据；稳定图片路径不受影响。
    pub fn clear_transient_content_for_message(&mut self, message_id: &str) {
        if let Some(message) = self
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
        {
            message.clear_transient_data();
        }
    }

    pub(crate) fn defer_tool_injection(&mut self, tool_name: String, payload: serde_json::Value) {
        self.deferred_tool_injections
            .push(DeferredToolInjection { tool_name, payload });
    }

    pub fn append_worker_message(
        &mut self,
        role: MessageRole,
        content: impl Into<String>,
        worker_id: &str,
    ) {
        self.append_worker_message_with_reasoning(role, content, String::new(), worker_id);
    }

    pub fn append_worker_message_with_reasoning(
        &mut self,
        role: MessageRole,
        content: impl Into<String>,
        reasoning_content: impl Into<String>,
        worker_id: &str,
    ) {
        self.messages.push(Message {
            id: new_id(),
            role,
            content: vec![ContentBlock::text(content.into())],
            reasoning_content: reasoning_content.into(),
            reasoning_signature: None,
            worker_id: Some(worker_id.to_string()),
            elapsed_ms: None,
            turn_status: None,
            reasoning_elapsed_ms: None,
            text_elapsed_ms: None,
            duration_ms: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            phase: crate::session::MessagePhase::Normal,
            created_at: now_text(),
        });
    }

    pub fn start_task(
        &mut self,
        task_id: String,
        user_message_id: String,
        assistant_message_id: String,
        user_input: String,
    ) {
        let now = now_text();
        self.task_records.push(SessionTaskRecord {
            task_id,
            user_message_id,
            assistant_message_id,
            user_input,
            status: SessionTaskStatus::Planning,
            summary: "正在生成执行计划".to_string(),
            plan_snapshot: None,
            tool_result: None,
            error: None,
            started_at: now.clone(),
            updated_at: now,
            finished_at: None,
            duration_ms: None,
            usage: None,
            llm_calls: Vec::new(),
            worker_results: Vec::new(),
        });
    }

    pub fn mark_task_executing(&mut self, task_id: &str, plan_snapshot: Option<String>) {
        let Some(record) = self
            .task_records
            .iter_mut()
            .find(|record| record.task_id == task_id)
        else {
            return;
        };
        record.status = SessionTaskStatus::Executing;
        record.summary = "正在执行任务".to_string();
        if let Some(plan_snapshot) = plan_snapshot {
            record.plan_snapshot = Some(plan_snapshot);
        }
        record.updated_at = now_text();
    }

    pub fn bind_task_assistant_message_id(&mut self, task_id: &str, assistant_message_id: String) {
        let Some(record) = self
            .task_records
            .iter_mut()
            .find(|record| record.task_id == task_id)
        else {
            return;
        };
        record.assistant_message_id = assistant_message_id;
        record.updated_at = now_text();
    }

    pub fn sync_task_plans(&mut self, task_id: &str, plans: &[PlanItem]) {
        for plan in plans {
            let now = now_text();
            if let Some(position) = self
                .task_plans
                .iter()
                .position(|item| item.id == plan.id && item.task_id == task_id)
            {
                let target = &mut self.task_plans[position];
                target.name = plan.name.clone();
                target.description = plan.description.clone();
                target.status = plan.status;
                target.execution_summary = plan.execution_summary.clone();
                target.updated_at = now.clone();

                let existing_steps = target.execution_steps.clone();
                let mut merged_steps = Vec::new();
                for step in &plan.execution_steps {
                    if let Some(found) = existing_steps.iter().find(|item| item.id == step.id) {
                        let mut step_record = found.clone();
                        step_record.name = step.name.clone();
                        step_record.description = step.description.clone();
                        step_record.status = step.status;
                        step_record.source = step.source;
                        step_record.updated_at = now.clone();
                        merged_steps.push(step_record);
                    } else {
                        merged_steps.push(SessionPlanExecutionStep {
                            id: step.id.clone(),
                            name: step.name.clone(),
                            description: step.description.clone(),
                            status: step.status,
                            source: step.source,
                            created_at: now.clone(),
                            updated_at: now.clone(),
                        });
                    }
                }
                target.execution_steps = merged_steps;
            } else {
                let mut target = SessionTaskPlan {
                    id: plan.id.clone(),
                    task_id: task_id.to_string(),
                    name: plan.name.clone(),
                    description: plan.description.clone(),
                    status: plan.status,
                    execution_summary: plan.execution_summary.clone(),
                    execution_steps: plan
                        .execution_steps
                        .iter()
                        .map(|step| SessionPlanExecutionStep {
                            id: step.id.clone(),
                            name: step.name.clone(),
                            description: step.description.clone(),
                            status: step.status,
                            source: step.source,
                            created_at: now.clone(),
                            updated_at: now.clone(),
                        })
                        .collect(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                };
                target.updated_at = now.clone();
                self.task_plans.push(target);
            }
        }
    }

    pub fn delete_pending_task_plan(&mut self, pending_index: usize) -> bool {
        let Some(pos) = self
            .pending_task_plan_positions()
            .get(pending_index)
            .copied()
        else {
            return false;
        };
        self.task_plans.remove(pos);
        true
    }

    pub fn move_pending_task_plan(&mut self, from_idx: usize, to_idx: usize) -> bool {
        let pending_positions = self.pending_task_plan_positions();
        if pending_positions.is_empty()
            || from_idx >= pending_positions.len()
            || to_idx >= pending_positions.len()
            || from_idx == to_idx
        {
            return false;
        }

        let mut pending = pending_positions
            .iter()
            .map(|idx| self.task_plans[*idx].clone())
            .collect::<Vec<_>>();
        let item = pending.remove(from_idx);
        pending.insert(to_idx, item);

        for (slot, item) in pending_positions.iter().zip(pending) {
            self.task_plans[*slot] = item;
        }
        true
    }

    fn pending_task_plan_positions(&self) -> Vec<usize> {
        self.task_plans
            .iter()
            .enumerate()
            .filter_map(|(idx, plan)| (plan.status == PlanStepStatus::Pending).then_some(idx))
            .collect()
    }

    #[allow(dead_code)]
    pub fn complete_task(
        &mut self,
        task_id: &str,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
        duration_ms: u64,
    ) {
        self.complete_task_with_usage(task_id, plan_snapshot, tool_result, duration_ms, None);
    }

    pub fn complete_task_with_usage(
        &mut self,
        task_id: &str,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
        duration_ms: u64,
        usage: Option<TokenUsage>,
    ) {
        let Some(record) = self
            .task_records
            .iter_mut()
            .find(|record| record.task_id == task_id)
        else {
            return;
        };
        record.status = SessionTaskStatus::Completed;
        record.summary = "执行完成".to_string();
        if let Some(plan_snapshot) = plan_snapshot {
            record.plan_snapshot = Some(plan_snapshot);
        }
        record.tool_result = tool_result;
        record.error = None;
        record.duration_ms = Some(duration_ms);
        record.usage = usage;
        let now = now_text();
        record.updated_at = now.clone();
        record.finished_at = Some(now);
    }

    pub fn fail_task(
        &mut self,
        task_id: &str,
        summary: impl Into<String>,
        error: Option<String>,
        duration_ms: u64,
    ) {
        self.fail_task_with_context(task_id, summary, error, duration_ms, None, None);
    }

    pub fn fail_task_with_context(
        &mut self,
        task_id: &str,
        summary: impl Into<String>,
        error: Option<String>,
        duration_ms: u64,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
    ) {
        self.fail_task_with_context_and_usage(
            task_id,
            summary,
            error,
            duration_ms,
            plan_snapshot,
            tool_result,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fail_task_with_context_and_usage(
        &mut self,
        task_id: &str,
        summary: impl Into<String>,
        error: Option<String>,
        duration_ms: u64,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
        usage: Option<TokenUsage>,
    ) {
        let Some(record) = self
            .task_records
            .iter_mut()
            .find(|record| record.task_id == task_id)
        else {
            return;
        };
        record.status = SessionTaskStatus::Failed;
        record.summary = summary.into();
        if let Some(plan_snapshot) = plan_snapshot {
            record.plan_snapshot = Some(plan_snapshot);
        }
        if tool_result.is_some() {
            record.tool_result = tool_result;
        }
        record.error = error;
        record.duration_ms = Some(duration_ms);
        record.usage = usage;
        let now = now_text();
        record.updated_at = now.clone();
        record.finished_at = Some(now);
    }

    pub fn recover_interrupted_tasks(&mut self) -> usize {
        let mut recovered = 0usize;
        for record in &mut self.task_records {
            if matches!(
                record.status,
                SessionTaskStatus::Planning | SessionTaskStatus::Executing
            ) {
                recovered += 1;
                record.status = SessionTaskStatus::Failed;
                record.summary = "任务因进程中断而恢复为失败".to_string();
                record.error = Some("执行中断：应用重启或异常退出".to_string());
                let now = now_text();
                record.updated_at = now.clone();
                record.finished_at = Some(now);
            }
        }
        recovered
    }

    /// 计算当前会话所有任务的累计 token 用量
    pub fn total_usage(&self) -> TokenUsage {
        if self.token_usage.total_tokens > 0 {
            return self.token_usage.clone();
        }

        let mut total = TokenUsage::default();
        for record in &self.task_records {
            if let Some(usage) = &record.usage {
                total.accumulate(usage);
            }
        }
        total
    }

    /// 重建 system prompt 消息
    ///
    /// 从 session 数据（title, cwd, context_summary）和外部配置构建完整的 system prompt，
    /// 存为 `Message { role: System }`，供 `context()` 返回。
    ///
    /// 应在以下时机调用：
    /// - 新对话首轮（system_prompt_message 为 None）
    /// - 压缩对话后
    /// - 清空上下文后
    pub fn rebuild_system_prompt(&mut self, config: &crate::prompt::SystemPromptConfig) {
        let msg = crate::prompt::sections::build_full_system_prompt(self, config);
        self.system_prompt_message = Some(msg);
    }

    /// 构建 LLM 请求上下文
    ///
    /// 返回 system_prompt_message（如有）+ `summary_up_to` 之后的对话消息。
    /// System 消息由 `build_provider_messages` 提取到 system prompt。
    pub fn context(&self) -> Vec<Message> {
        let mut context = Vec::new();
        if let Some(ref msg) = self.system_prompt_message {
            context.push(msg.clone());
        }
        context.extend(
            self.messages[self.summary_up_to..]
                .iter()
                // 持久化 System 消息是 UI/恢复日志；唯一模型系统提示由
                // system_prompt_message 提供，避免日志覆盖完整规则。
                // Notice 是系统发给用户的通知，按角色整体排除出模型上下文。
                .filter(|message| {
                    message.role != MessageRole::System && message.role != MessageRole::Notice
                })
                .cloned(),
        );
        context
    }

    pub fn recent_messages(&self, limit: usize) -> Vec<Message> {
        if self.messages.len() <= limit {
            return self.messages.clone();
        }
        self.messages[self.messages.len() - limit..].to_vec()
    }

    /// 更新指定消息的内容
    pub fn update_message_content(&mut self, message_id: &str, new_content: String) -> bool {
        if let Some(msg) = self.messages.iter_mut().find(|m| m.id == message_id) {
            msg.content = vec![ContentBlock::text(new_content)];
            true
        } else {
            false
        }
    }

    /// 更新指定消息的文本和媒体内容
    pub fn update_message_content_with_media(
        &mut self,
        message_id: &str,
        new_text: String,
        new_media: Vec<tiangong_types::MediaAsset>,
    ) -> bool {
        if let Some(msg) = self.messages.iter_mut().find(|m| m.id == message_id) {
            let mut blocks = vec![ContentBlock::text(new_text)];
            for asset in &new_media {
                blocks.push(asset.to_content_block());
            }
            msg.content = blocks;
            self.updated_at = now_text();
            true
        } else {
            false
        }
    }

    /// 截断指定消息之后的所有消息（保留该消息本身），返回移除数量
    pub fn truncate_after_message(&mut self, message_id: &str) -> usize {
        let Some(idx) = self.messages.iter().position(|m| m.id == message_id) else {
            return 0;
        };
        let remove_count = self.messages.len() - idx - 1;
        self.messages.truncate(idx + 1);
        remove_count
    }

    /// 获取最新用户消息的index
    pub fn latest_user_message_index(&self) -> Option<usize> {
        self.messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.role == MessageRole::User)
            .map(|(idx, _)| idx)
    }
}

fn new_id() -> String {
    scru128::new().to_string()
}

#[cfg(test)]
mod persistence_tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn atomic_replace_file_serializes_complete_replacements() -> io::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let target = temp_dir.path().join("sessions").join("session.json");
        atomic_replace_file(&target, b"initial")?;

        let payloads = (0..4)
            .map(|writer| format!("writer-{writer}:{}", "x".repeat(16 * 1024)).into_bytes())
            .collect::<Vec<_>>();
        let barrier = Arc::new(Barrier::new(payloads.len()));
        std::thread::scope(|scope| -> io::Result<()> {
            let mut writers = Vec::new();
            for payload in &payloads {
                let target = target.clone();
                let barrier = Arc::clone(&barrier);
                writers.push(scope.spawn(move || {
                    barrier.wait();
                    atomic_replace_file(&target, payload)
                }));
            }
            for writer in writers {
                writer.join().expect("原子写入线程不应 panic")?;
            }
            Ok(())
        })?;

        let persisted = std::fs::read(&target)?;
        assert!(payloads.contains(&persisted));
        let entries = std::fs::read_dir(target.parent().expect("目标文件应有父目录"))?
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(entries.len(), 1, "成功替换后不应遗留临时文件");
        assert_eq!(entries[0].path(), target);
        Ok(())
    }

    #[test]
    fn close_fallback_removes_only_unfinished_tool_calls() {
        let mut session = Session::new("tool-call-fallback");
        let mut assistant = Message::new(MessageRole::Assistant, "");
        assistant.tool_calls = vec![
            MessageToolCall {
                id: "completed-call".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            },
            MessageToolCall {
                id: "unfinished-call".to_string(),
                name: "write_file".to_string(),
                arguments: serde_json::json!({}),
            },
        ];
        session.messages.push(assistant);
        session.messages.push(Message::tool_result(
            "completed-call",
            "read_file",
            "done",
            false,
        ));

        let interrupted = session.close_unfinished_tool_calls_with_reason("interrupted");

        let remaining_calls = &session.messages[0].tool_calls;
        assert!(interrupted.is_empty());
        assert_eq!(remaining_calls.len(), 1);
        assert_eq!(remaining_calls[0].id, "completed-call");
        assert!(!session.has_unfinished_tool_calls());
        assert!(
            session
                .messages
                .iter()
                .all(|message| message.tool_call_id.as_deref() != Some("unfinished-call"))
        );
    }

    #[test]
    fn legacy_workspace_tabs_are_ignored_and_never_serialized_again() {
        let mut value = serde_json::to_value(Session::new("legacy-tabs")).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert(
            "tabs".to_string(),
            serde_json::json!([{
                "id": "terminal-1",
                "kind": "terminal",
                "title": "终端",
                "url": "",
                "created_at": "now"
            }]),
        );
        object.insert("active_tab_id".to_string(), serde_json::json!("terminal-1"));

        let restored: Session = serde_json::from_value(value).unwrap();
        let serialized = serde_json::to_value(restored).unwrap();
        assert!(serialized.get("tabs").is_none());
        assert!(serialized.get("active_tab_id").is_none());
    }

    #[test]
    fn derived_context_metrics_are_not_persisted_but_usage_is() {
        let mut session = Session::new("derived-context-metrics");
        session.compression_threshold_tokens = 190_000;
        session.context_limit_tokens = 200_000;
        session.current_tokens = 12_345;
        session.token_usage = TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: 120,
            prompt_cache_hit_tokens: Some(10),
            prompt_cache_miss_tokens: Some(90),
        };

        let mut value = serde_json::to_value(&session).unwrap();
        assert!(value.get("compression_threshold_tokens").is_none());
        assert!(value.get("context_limit_tokens").is_none());
        assert_eq!(value["current_tokens"], 12_345);
        assert_eq!(value["token_usage"]["total_tokens"], 120);

        value["compression_threshold_tokens"] = serde_json::json!(999_999);
        value["context_limit_tokens"] = serde_json::json!(1_000_000);
        let restored: Session = serde_json::from_value(value).unwrap();
        assert_eq!(restored.compression_threshold_tokens, 0);
        assert_eq!(restored.context_limit_tokens, 0);
        assert_eq!(restored.current_tokens, 12_345);
        assert_eq!(restored.token_usage.total_tokens, 120);
    }

    #[test]
    fn bound_storage_root_is_used_and_restored_on_load() {
        let root = tempfile::tempdir().unwrap();
        let session = Session::new("child").with_storage_root(root.path());
        session.try_persist_to_disk().unwrap();

        let path = root
            .path()
            .join("sessions")
            .join(format!("{}.json", session.id));
        assert!(path.is_file());
        assert!(
            serde_json::to_value(&session)
                .unwrap()
                .get("storage_root")
                .is_none()
        );

        let restored = Session::load_from_storage(root.path(), &session.id).unwrap();
        assert_eq!(restored.title, "child");
        assert_eq!(restored.bound_storage_root(), Some(root.path()));
    }

    #[test]
    fn load_from_storage_rejects_path_traversal() {
        let root = tempfile::tempdir().unwrap();
        let error = Session::load_from_storage(root.path(), "../outside").unwrap_err();
        assert!(error.contains("会话 ID"));
    }
}

#[cfg(test)]
mod ready_content_tests {
    use super::*;

    fn prepared_message(data: &str) -> Vec<ContentBlock> {
        vec![
            ContentBlock::text("分析图片"),
            ContentBlock::Image {
                asset: StoredAsset {
                    asset_id: "asset-1".to_string(),
                    local_path: "/tmp/asset-1.png".to_string(),
                    original_name: "asset-1.png".to_string(),
                    mime_type: "image/png".to_string(),
                    size: 4,
                    kind: MediaKind::Image,
                },
                data: Some(data.to_string()),
            },
        ]
    }

    #[test]
    fn transient_image_data_is_available_to_context_but_never_serialized() {
        let secret_data = "data:image/png;base64,THIS_MUST_NOT_PERSIST";
        let mut session = Session::new("ready-content");
        session.append_prepared_user_message_with_id(
            "message-1".to_string(),
            prepared_message(secret_data),
        );

        let json = serde_json::to_string(&session).unwrap();
        assert!(!json.contains("THIS_MUST_NOT_PERSIST"));
        assert!(json.contains("\"type\":\"image\""));
        assert!(json.contains("/tmp/asset-1.png"));

        let context = session.context();
        assert!(context[0].content.iter().any(|block| matches!(
            block,
            ContentBlock::Image { data: Some(data), .. } if data == secret_data
        )));
        assert!(
            !serde_json::to_string(&context)
                .unwrap()
                .contains("THIS_MUST_NOT_PERSIST")
        );
        session.clear_transient_content_for_message("message-1");
        assert!(matches!(
            &session.context()[0].content[1],
            ContentBlock::Image { data: None, .. }
        ));
    }
}

/// Core Session → 插件只读快照转换。
///
/// 由 WASM Adapter 在生命周期钩子里调用，序列化为 JSON 传给 WASM。
/// 不暴露 Core 内部状态（token 计数、任务记录、信任模式等）。
impl From<&Session> for tiangong_types::PluginSession {
    fn from(session: &Session) -> Self {
        // 工作区标识：取 cwd 的末尾目录名（平台无关，由宿主生成）。
        let workspace_id = session
            .cwd
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&session.cwd)
            .to_string();
        Self {
            id: session.id.clone(),
            title: session.title.clone(),
            cwd: session.cwd.clone(),
            workspace_id,
            parent_session_id: session.parent_session_id.clone(),
            reasoning_effort: session
                .reasoning_effort
                .map(|effort| effort.as_str().to_string()),
            messages: session.messages.clone(),
            context_summary: session.context_summary.clone(),
            created_at: session.created_at.clone(),
            updated_at: session.updated_at.clone(),
        }
    }
}
