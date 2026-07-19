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
    pub fn edit_prepared_user_message_for_turn(
        &mut self,
        session_id: &str,
        message_id: &str,
        prepared: Vec<tiangong_types::ContentBlock>,
    ) -> Result<(Session, Session)> {
        let index = self
            .store
            .session
            .sessions
            .iter()
            .position(|s| s.id == session_id)
            .ok_or_else(|| anyhow!("目标会话不存在：{session_id}"))?;
        if self.has_pending_turn_for(session_id) {
            return Err(anyhow!("目标会话正在执行，暂时不能编辑重发"));
        }
        let original_session = self.store.session.sessions[index].clone();
        let session = &mut self.store.session.sessions[index];
        if !session.update_prepared_user_message(message_id, prepared) {
            return Err(anyhow!("消息不存在：{message_id}"));
        }
        session.truncate_after_message(message_id);
        session.updated_at = now_text();
        let mut runtime_session = self.store.session.sessions[index].clone();
        if runtime_session.cwd.trim().is_empty() {
            runtime_session.cwd = self.store.session.workspace_dir.clone();
        }
        self.mark_pending_message_for(session_id, message_id);
        if let Err(error) = self.persist_session_and_app(session_id) {
            self.store.session.sessions[index] = original_session.clone();
            self.remove_pending_message_for(session_id, message_id);
            let rollback_error = self.persist_session_and_app(session_id).err();
            return Err(match rollback_error {
                Some(rollback_error) => {
                    anyhow!("编辑状态持久化失败：{error}；恢复原状态也失败：{rollback_error}")
                }
                None => anyhow!("编辑状态持久化失败：{error}"),
            });
        }
        self.resync_session_metadata();
        Ok((original_session, runtime_session))
    }

    /// 把指定会话整体回滚到给定 session（编辑投递失败恢复用）。
    ///
    /// 收敛 `restore_edited_session` 对 `sessions_mut()` 的整体覆盖操纵
    /// （issue #245）。pending 标记与持久化由调用方负责。
    pub fn restore_session_snapshot(&mut self, session_id: &str, snapshot: Session) {
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            *session = snapshot;
        }
        self.resync_session_metadata();
    }

    /// 移除指定会话中某条用户消息（投递失败回滚用），并修正 summary_up_to
    /// 与 updated_at（issue #245：收敛 `restore_failed_user_message_state`
    /// 对 `sessions_mut()` 的直接操纵）。
    ///
    /// 仅在无活跃 turn 时调用（Core 投递失败、尚未启动 worker 的回滚路径）。
    /// 不存在该消息时静默跳过。pending 标记与持久化由调用方负责。
    pub fn remove_message_for_failed_turn(&mut self, session_id: &str, message_id: &str) {
        let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
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
        }
    }

    /// 把已准备好的用户消息内容块追加到指定会话，并在持久化失败时回滚
    /// 到追加前的状态（issue #245：收敛 send_message_inner 的事务，避免
    /// 调用方直接经 `sessions_mut()` 操纵内存 Session）。
    ///
    /// 调用方必须保证此刻该会话没有活跃 turn（worker 不在写盘）——通常由
    /// `session_send_lock` + `has_pending_turn_for` 守卫。本方法内部完成：
    /// 1. 追加消息 + 更新 updated_at + 标记 pending
    /// 2. 持久化；失败则恢复内存 Session、移除 pending 标记、重新持久化并报错
    /// 3. 成功后返回用于 ensure_core 的 runtime_session（cwd 回填 workspace_dir）
    pub fn append_prepared_user_message_for_turn(
        &mut self,
        session_id: &str,
        message_id: &str,
        prepared: Vec<tiangong_types::ContentBlock>,
    ) -> Result<Session> {
        let index = self
            .store
            .session
            .sessions
            .iter()
            .position(|s| s.id == session_id)
            .ok_or_else(|| anyhow!("目标会话不存在：{session_id}"))?;
        let original_session = self.store.session.sessions[index].clone();
        self.store.session.sessions[index]
            .append_prepared_user_message_with_id(message_id.to_string(), prepared);
        self.store.session.sessions[index].updated_at = now_text();
        self.mark_pending_message_for(session_id, message_id);
        if let Err(error) = self.persist_session_and_app(session_id) {
            self.store.session.sessions[index] = original_session;
            self.remove_pending_message_for(session_id, message_id);
            let rollback_error = self.persist_session_and_app(session_id).err();
            return Err(match rollback_error {
                Some(rollback_error) => {
                    anyhow!("消息状态持久化失败：{error}；恢复原状态也失败：{rollback_error}")
                }
                None => anyhow!("消息状态持久化失败：{error}"),
            });
        }
        self.resync_session_metadata();
        let mut runtime_session = self.store.session.sessions[index].clone();
        if runtime_session.cwd.trim().is_empty() {
            runtime_session.cwd = self.store.session.workspace_dir.clone();
        }
        Ok(runtime_session)
    }
}
