use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use tiangong_core::agent_config::AgentConfig;
use tiangong_core::runtime::{RunSnapshot, RunStatus};
use tiangong_core::session::{MessageRole, Session, SessionTaskPlan, now_text};

mod facade;
pub(crate) mod repository;
mod store;
pub(crate) mod support;
#[cfg(test)]
mod tests;

// Private imports
use self::repository::{
    default_app_storage_path, default_sessions_dir_path, default_workspace_dir,
    normalize_model_list, validate_agent_config,
};
pub use self::support::StreamEvent;
use self::support::{LegacyPersistedState, LoadedState, PersistedAppState};

// Public re-exports for Tauri API
pub use self::repository::AppRepository;
pub use self::repository::utils::storage_root;
pub use self::store::{
    AgentState, AppStore, PendingTurnStub, ProviderState, RuntimeState, SessionInputDraft,
    SessionState,
};
pub use self::support::{AppPaths, AppServices};

pub use tiangong_core_manager::CoreManager;

const DEFAULT_SESSION_TITLE: &str = "默认会话";

#[derive(Debug)]
pub struct TiangongState {
    pub store: AppStore,
    pub services: AppServices,
    /// 会话级 TiangongCore 管理器（issue #245）。
    ///
    /// 延迟注入：`load_or_default` 构造时尚无 host 依赖（app_handle/skill/mcp），
    /// 留空 OnceLock；host 构造完 state 后调用 [`Self::install_core_manager`]
    /// 填充。访问前未注入会 panic（与 `set_app_handle` 同模式）。
    pub core_manager: std::sync::OnceLock<CoreManager>,
}

impl Default for TiangongState {
    fn default() -> Self {
        Self::load_or_default()
    }
}

impl TiangongState {
    /// 注入 CoreManager（host 在构造完 state 后调用）。
    ///
    /// 重复注入会返回已有实例的引用（幂等，便于多入口安全调用）。
    pub fn install_core_manager(
        &self,
        config: tiangong_core::core_config::CoreConfigProvider,
        storage_root: impl Into<PathBuf>,
    ) -> &CoreManager {
        self.core_manager
            .get_or_init(|| CoreManager::new(config, storage_root))
    }

    /// 访问 CoreManager。未注入时 panic。
    pub fn core_manager(&self) -> &CoreManager {
        self.core_manager
            .get()
            .expect("CoreManager 尚未注入，需先调用 install_core_manager")
    }
}

impl TiangongState {
    pub fn input_draft(&self) -> &str {
        self.store
            .session
            .input_drafts
            .get(&self.store.session.active_session_id)
            .map(|draft| draft.text.as_str())
            .unwrap_or("")
    }

    pub fn session_input_draft(&self, session_id: &str) -> SessionInputDraft {
        self.store
            .session
            .input_drafts
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// 按显式会话 ID 更新草稿。旧 revision 的迟到写入只返回当前值，不覆盖新草稿。
    pub fn set_session_input_draft(
        &mut self,
        session_id: &str,
        draft: SessionInputDraft,
    ) -> Result<SessionInputDraft> {
        self.set_session_input_draft_with_outcome(session_id, draft)
            .map(|(draft, _applied)| draft)
    }

    /// 更新草稿并返回该写入是否真正被采用。调用方可据此决定是否提交
    /// 本次新归档的附件；迟到 revision 会返回 `false`。
    pub fn set_session_input_draft_with_outcome(
        &mut self,
        session_id: &str,
        mut draft: SessionInputDraft,
    ) -> Result<(SessionInputDraft, bool)> {
        if session_id.trim().is_empty() {
            return Err(anyhow!("草稿会话 ID 不能为空"));
        }
        let previous = self.store.session.input_drafts.get(session_id).cloned();
        if let Some(current) = previous.as_ref() {
            if draft.revision < current.revision {
                return Ok((current.clone(), false));
            }
            // 发送状态只能由后端发送事务维护，前端草稿更新不能覆盖它。
            draft.is_sending = current.is_sending;
        } else {
            draft.is_sending = false;
        }
        self.store
            .session
            .input_drafts
            .insert(session_id.to_string(), draft.clone());
        if let Err(error) = self.persist_app_only() {
            match previous {
                Some(previous) => {
                    self.store
                        .session
                        .input_drafts
                        .insert(session_id.to_string(), previous);
                }
                None => {
                    self.store.session.input_drafts.remove(session_id);
                }
            }
            return Err(error);
        }
        Ok((draft, true))
    }

    /// 用 revision 认领一次发送，防止同一会话并发准备同一版草稿。
    pub fn begin_session_send(&mut self, session_id: &str, revision: u64) -> Result<()> {
        let draft = self
            .store
            .session
            .input_drafts
            .entry(session_id.to_string())
            .or_default();
        if draft.revision < revision {
            return Err(anyhow!(
                "草稿尚未持久化到本次发送版本（期望 revision={revision}，当前 revision={}）",
                draft.revision
            ));
        }
        if draft.is_sending {
            return Err(anyhow!("当前会话已有消息正在准备发送"));
        }
        draft.is_sending = true;
        // is_sending 是纯运行时状态，序列化到 app.json 时本来就会强制置 false。
        // 这里不做无意义的磁盘写入，也避免写入失败后内存永久卡在发送中。
        Ok(())
    }

    /// 结束发送。成功时只有 revision 仍一致才清理；期间新输入保持不变。
    pub fn finish_session_send(
        &mut self,
        session_id: &str,
        revision: u64,
        success: bool,
    ) -> Result<SessionInputDraft> {
        let draft = self
            .store
            .session
            .input_drafts
            .entry(session_id.to_string())
            .or_default();
        draft.is_sending = false;
        let mut persistent_change = false;
        if success && draft.revision == revision {
            draft.text.clear();
            draft.attachments.clear();
            draft.revision = draft.revision.saturating_add(1);
            persistent_change = true;
        }
        let result = draft.clone();
        // 仅 is_sending 变化时无需写盘；app.json 中该字段始终强制为 false。
        // 已成功投递后清理草稿时，即使写盘失败也保留内存中的已清理
        // 状态，避免将已发送内容再次展示为可重试草稿。
        if persistent_change {
            self.persist_app_only()?;
        }
        Ok(result)
    }

    pub fn migrate_session_input_draft(
        &mut self,
        from_session_id: &str,
        to_session_id: &str,
    ) -> Result<SessionInputDraft> {
        if to_session_id.trim().is_empty() {
            return Err(anyhow!("目标会话 ID 不能为空"));
        }
        let previous_from = self
            .store
            .session
            .input_drafts
            .get(from_session_id)
            .cloned();
        let previous_to = self.store.session.input_drafts.get(to_session_id).cloned();
        let moved = self
            .store
            .session
            .input_drafts
            .remove(from_session_id)
            .unwrap_or_default();
        let result = match self.store.session.input_drafts.get(to_session_id) {
            Some(existing) if existing.revision > moved.revision => existing.clone(),
            _ => {
                self.store
                    .session
                    .input_drafts
                    .insert(to_session_id.to_string(), moved.clone());
                moved
            }
        };
        if let Err(error) = self.persist_app_only() {
            match previous_from {
                Some(previous) => {
                    self.store
                        .session
                        .input_drafts
                        .insert(from_session_id.to_string(), previous);
                }
                None => {
                    self.store.session.input_drafts.remove(from_session_id);
                }
            }
            match previous_to {
                Some(previous) => {
                    self.store
                        .session
                        .input_drafts
                        .insert(to_session_id.to_string(), previous);
                }
                None => {
                    self.store.session.input_drafts.remove(to_session_id);
                }
            }
            return Err(error);
        }
        Ok(result)
    }

    pub fn remove_session_input_draft(&mut self, session_id: &str) -> Result<()> {
        let previous = self.store.session.input_drafts.remove(session_id);
        if let Err(error) = self.persist_app_only() {
            if let Some(previous) = previous {
                self.store
                    .session
                    .input_drafts
                    .insert(session_id.to_string(), previous);
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn run_snapshot(&self) -> &RunSnapshot {
        &self.store.runtime.run
    }

    pub fn agent_config(&self) -> &AgentConfig {
        &self.store.agent.agent_config
    }

    /// 获取当前活跃的 Worker 列表（已迁移到 TiangongCore 管理）
    pub fn list_active_workers(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    pub fn set_trust_mode(&mut self, mode: tiangong_core::permission::TrustMode) -> Result<()> {
        let active_id = self.store.session.active_session_id.clone();
        self.set_session_trust_mode(&active_id, mode)
    }

    pub fn set_session_trust_mode(
        &mut self,
        session_id: &str,
        mode: tiangong_core::permission::TrustMode,
    ) -> Result<()> {
        self.set_session_trust_mode_in_memory(session_id, mode)?;
        if self.has_pending_turn_for(session_id) {
            // Core 在活跃轮次中独占会话文件；终态重载会保留宿主信任模式。
            self.persist_app_only()
        } else {
            self.persist_session_and_app(session_id)
        }
    }

    /// 只更新宿主内存与配置镜像，不写 Session 文件。
    ///
    /// Desktop 在 Core 存活时通过 Core 元数据命令持久化，避免宿主成为第二写入者。
    pub fn set_session_trust_mode_in_memory(
        &mut self,
        session_id: &str,
        mode: tiangong_core::permission::TrustMode,
    ) -> Result<()> {
        let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return Err(anyhow::anyhow!(
                "会话不存在，无法设置信任模式：{session_id}"
            ));
        };
        session.trust_mode = mode;
        if self.store.session.active_session_id == session_id {
            // 兼容旧的状态读取；真实来源是当前会话。
            self.store.agent.agent_config.trust_mode = mode;
            self.refresh_chat_endpoint();
        }
        self.resync_session_metadata();
        Ok(())
    }

    pub fn set_default_trust_mode(
        &mut self,
        mode: tiangong_core::permission::TrustMode,
    ) -> Result<()> {
        self.store.agent.agent_config.default_trust_mode = mode;
        self.persist_app_only()
    }

    pub fn set_custom_system_prompt(&mut self, prompt: String) -> Result<()> {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            // 清空：删除 custom-prompt.md 并清空旧字段
            tiangong_config::io::clear_custom_prompt()?;
            self.store.agent.agent_config.custom_system_prompt = String::new();
        } else {
            // 写入 custom-prompt.md 作为唯一事实来源，并清空 app.json 旧字段
            tiangong_config::io::save_custom_prompt(&prompt)?;
            self.store.agent.agent_config.custom_system_prompt = String::new();
        }
        self.refresh_chat_endpoint();
        self.persist_app_only()
    }

    /// 获取自定义 Prompt 内容（已按 custom-prompt.md > 旧字段 > 空 优先级加载）。
    pub fn custom_system_prompt(&self) -> &str {
        &self.store.agent.agent_config.custom_system_prompt
    }

    /// 获取自定义 Prompt 独立存储路径（~/.tiangong/custom-prompt.md）。
    pub fn custom_prompt_path(&self) -> std::path::PathBuf {
        tiangong_config::io::custom_prompt_path()
    }

    pub fn set_reasoning_effort(&mut self, effort: String) -> Result<()> {
        let active_id = self.store.session.active_session_id.clone();
        self.set_session_reasoning_effort(&active_id, effort)
    }

    pub fn set_session_reasoning_effort(&mut self, session_id: &str, effort: String) -> Result<()> {
        self.set_session_reasoning_effort_in_memory(session_id, effort)?;
        if self.has_pending_turn_for(session_id) {
            self.persist_app_only()
        } else {
            self.persist_session_and_app(session_id)
        }
    }

    /// 只更新宿主内存与兼容配置镜像，不写 Session 文件。
    pub fn set_session_reasoning_effort_in_memory(
        &mut self,
        session_id: &str,
        effort: String,
    ) -> Result<()> {
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.reasoning_effort = Some(effort.clone());
            if self.store.session.active_session_id == session_id {
                self.store.agent.agent_config.reasoning_effort = effort;
            }
            self.resync_session_metadata();
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "会话不存在，无法设置思考强度：{session_id}"
            ))
        }
    }

    /// 清除会话级思考强度覆盖（回滚到应用默认）。只更新内存镜像，不写 Session 文件。
    ///
    /// 收敛 rollback 路径对 `sessions_mut()` 的直接操纵（issue #245）。
    pub fn clear_session_reasoning_effort_in_memory(&mut self, session_id: &str) -> Result<()> {
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.reasoning_effort = None;
            self.resync_session_metadata();
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "会话不存在，无法清除思考强度：{session_id}"
            ))
        }
    }
}
