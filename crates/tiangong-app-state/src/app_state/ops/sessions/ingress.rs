use super::super::super::*;

impl TiangongState {
    /// 为本地用户消息准备统一入口上下文。
    ///
    /// 统一处理：
    /// - 解析活动会话
    /// - 固定用户消息并立即持久化
    /// - 更新运行状态
    /// - 返回用于创建/复用 TiangongCore 的会话快照
    pub fn prepare_active_user_message_ingress(
        &mut self,
        content: impl Into<String>,
    ) -> Result<(String, String, Session)> {
        self.prepare_active_user_message_ingress_with_media(content, Vec::new())
    }

    pub fn prepare_active_user_message_ingress_with_media(
        &mut self,
        content: impl Into<String>,
        media: Vec<tiangong_types::MediaAsset>,
    ) -> Result<(String, String, Session)> {
        if !media.is_empty() {
            return Err(anyhow!(
                "旧消息入口不再接收媒体；请先通过宿主附件准备管线生成已就绪内容块"
            ));
        }
        let content = content.into();
        let idx = self.ensure_active_session_index();
        let message_id = scru128::new().to_string();
        let session_id = self.store.session.sessions[idx].id.clone();
        self.store.session.sessions[idx].append_message_with_id_and_media(
            message_id.clone(),
            MessageRole::User,
            content,
            String::new(),
            media,
        );
        self.store.session.sessions[idx].updated_at = now_text();
        self.persist_session_and_app(&session_id)?;
        self.resync_session_metadata();
        let session = self.store.session.sessions[idx].clone();
        self.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
        self.store.runtime.run.summary = "正在处理".to_string();
        self.store.runtime.run.last_session_id = Some(session.id.clone());
        let usage = session.total_usage();
        self.store.runtime.run.last_usage = (usage.total_tokens > 0).then_some(usage);
        self.store.runtime.run.updated_at = now_text();
        self.mark_pending_message_for(&session.id, &message_id);
        let mut runtime_session = session.clone();
        if runtime_session.cwd.trim().is_empty() {
            runtime_session.cwd = self.store.session.workspace_dir.clone();
        }
        Ok((session.id.clone(), message_id, runtime_session))
    }

    /// 为指定会话（而非全局活动会话）准备用户消息入口。
    ///
    /// 调用方先固定 session_id 再归档附件，避免归档期间活动会话切换导致串线。
    /// 草稿清理由调用方在相同 revision 的投递成功后完成，本方法不再无条件清空。
    /// 若目标会话不存在则明确失败，不回退到活动会话。
    pub fn prepare_user_message_ingress_for_session(
        &mut self,
        session_id: &str,
        content: impl Into<String>,
        media: Vec<tiangong_types::MediaAsset>,
    ) -> Result<(String, String, Session)> {
        if !media.is_empty() {
            return Err(anyhow!(
                "旧消息入口不再接收媒体；请先通过宿主附件准备管线生成已就绪内容块"
            ));
        }
        let content = content.into();
        let idx = self
            .store
            .session
            .sessions
            .iter()
            .position(|s| s.id == session_id)
            .ok_or_else(|| anyhow!("目标会话不存在：{session_id}"))?;
        let message_id = scru128::new().to_string();
        self.store.session.sessions[idx].append_message_with_id_and_media(
            message_id.clone(),
            MessageRole::User,
            content,
            String::new(),
            media,
        );
        self.store.session.sessions[idx].updated_at = now_text();
        self.persist_session_and_app(session_id)?;
        self.resync_session_metadata();
        let session = self.store.session.sessions[idx].clone();
        self.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
        self.store.runtime.run.summary = "正在处理".to_string();
        self.store.runtime.run.last_session_id = Some(session.id.clone());
        let usage = session.total_usage();
        self.store.runtime.run.last_usage = (usage.total_tokens > 0).then_some(usage);
        self.store.runtime.run.updated_at = now_text();
        self.mark_pending_message_for(&session.id, &message_id);
        let mut runtime_session = session.clone();
        if runtime_session.cwd.trim().is_empty() {
            runtime_session.cwd = self.store.session.workspace_dir.clone();
        }
        Ok((session.id.clone(), message_id, runtime_session))
    }

    /// 向指定会话注入外部事件消息，并立即持久化。
    pub fn append_session_message(
        &mut self,
        session_id: &str,
        role: MessageRole,
        content: impl Into<String>,
    ) -> Result<()> {
        let content = content.into();
        let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return Err(anyhow!("目标会话不存在：{session_id}"));
        };

        session.append_message(role, content);
        session.updated_at = now_text();
        self.resync_session_metadata();
        self.persist_session_and_app(session_id)
    }

    /// 编辑指定会话中的某条已存在用户消息（编辑重发），并截断其后所有消息，
    /// 在持久化失败时回滚（issue #245：收敛 `edit_and_resend` 对 `sessions_mut()`
    /// 的直接操纵）。
    ///
    /// 调用方必须保证此刻该会话没有活跃 turn（worker 不在写盘）。
    /// 返回 (编辑前的原始 session, 用于 ensure_core 的 runtime_session)。
    /// 原始 session 供调用方清理被截断消息引用的附件。
    /// 编辑重发：从磁盘 load session → 编辑消息 + 截断 → persist
    /// （issue #245：不经内存 Vec<Session>，简化无回滚）。
    pub fn edit_prepared_user_message_for_turn(
        &mut self,
        session_id: &str,
        message_id: &str,
        prepared: Vec<tiangong_types::ContentBlock>,
    ) -> Result<(Session, Session)> {
        if self.has_pending_turn_for(session_id) {
            return Err(anyhow!("目标会话正在执行，暂时不能编辑重发"));
        }
        let storage_root = crate::app_state::repository::storage_root();
        let mut session = Session::load_from_storage(&storage_root, session_id)
            .map_err(|error| anyhow!("加载会话失败：{error}"))?;
        let original = session.clone();
        if !session.update_prepared_user_message(message_id, prepared) {
            return Err(anyhow!("消息不存在：{message_id}"));
        }
        session.truncate_after_message(message_id);
        session.updated_at = now_text();
        session
            .try_persist_to_disk()
            .map_err(|error| anyhow!("编辑持久化失败：{error}"))?;
        self.mark_pending_message_for(session_id, message_id);
        if session.cwd.trim().is_empty() {
            session.cwd = self.store.session.workspace_dir.clone();
        }
        Ok((original, session))
    }

    /// 把指定会话整体写回磁盘（编辑投递失败恢复用，issue #245）。
    pub fn restore_session_snapshot(&mut self, _session_id: &str, snapshot: Session) {
        let _ = snapshot.try_persist_to_disk();
    }

    /// 从磁盘 load session → 移除指定消息 → persist（投递失败回滚，issue #245）。
    pub fn remove_message_for_failed_turn(&mut self, session_id: &str, message_id: &str) {
        let storage_root = crate::app_state::repository::storage_root();
        let Ok(mut session) = Session::load_from_storage(&storage_root, session_id) else {
            return;
        };
        if let Some(index) = session
            .messages
            .iter()
            .position(|message| message.id == message_id)
        {
            session.messages.remove(index);
            session.summary_up_to = session.summary_up_to.min(session.messages.len());
            session.updated_at = now_text();
            let _ = session.try_persist_to_disk();
        }
    }

    /// 从磁盘 load session → 追加用户消息 → persist（issue #245：不经内存 Vec<Session>）。
    /// 3. 成功后返回用于 ensure_core 的 runtime_session（cwd 回填 workspace_dir）
    /// 从磁盘 load session → 追加用户消息 → persist（issue #245：不经内存 Vec<Session>）。
    ///
    /// 简化设计：persist 失败直接返回错误，磁盘保持旧值（无需内存回滚）。
    pub fn append_prepared_user_message_for_turn(
        &mut self,
        session_id: &str,
        message_id: &str,
        prepared: Vec<tiangong_types::ContentBlock>,
    ) -> Result<Session> {
        let storage_root = crate::app_state::repository::storage_root();
        let mut session = Session::load_from_storage(&storage_root, session_id)
            .map_err(|error| anyhow!("加载会话失败：{error}"))?;
        session.append_prepared_user_message_with_id(message_id.to_string(), prepared);
        session.updated_at = now_text();
        session
            .try_persist_to_disk()
            .map_err(|error| anyhow!("消息持久化失败：{error}"))?;
        self.mark_pending_message_for(session_id, message_id);
        if session.cwd.trim().is_empty() {
            session.cwd = self.store.session.workspace_dir.clone();
        }
        Ok(session)
    }
}
