use super::super::super::*;

impl TiangongState {
    /// 为本地用户消息准备统一入口上下文。
    ///
    /// 统一处理：
    /// - 解析活动会话
    /// - 固定用户消息并立即持久化
    /// - 清空输入草稿
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
        // 落盘前先同步归档附件为本地路径，确保磁盘持久化的 media.url 始终是
        // 可读的本地路径（而非 data URL / 远程地址）——即使消息投递失败、应用
        // 崩溃或用户提前退出，会话文件也不含失效引用。
        // core 侧 media-archive 插件会再次归档（幂等，is_archived_media_path 直接放行）。
        let content = content.into();
        let media = tiangong_media_archive::archive_input_media_assets(media);
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
        let session = self.store.session.sessions[idx].clone();
        self.store.session.input_draft.clear();
        self.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
        self.store.runtime.run.summary = "正在处理".to_string();
        self.store.runtime.run.last_session_id = Some(session.id.clone());
        let usage = session.total_usage();
        self.store.runtime.run.last_usage = (usage.total_tokens > 0).then_some(usage);
        self.store.runtime.run.updated_at = now_text();
        self.mark_pending_turn_for(session.id.clone());
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
        self.persist_session_and_app(session_id)
    }
}
