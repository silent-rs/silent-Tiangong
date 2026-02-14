use anyhow::{Result, anyhow};

use super::{CliApp, CommandHint};

impl CliApp {
    pub(super) fn submit_input(&mut self) -> Result<()> {
        let raw = self.input.trim().to_string();

        if raw.is_empty() {
            return Ok(());
        }

        if raw.starts_with('/') {
            let command = self.resolve_command_to_execute(&raw);
            self.input.clear();
            self.selected_hint_idx = 0;
            if let Err(err) = self.handle_command(&command) {
                self.status_message = format!("命令执行失败：{err}");
            }
            return Ok(());
        }

        if self.state.has_pending_turn() {
            self.status_message = "当前请求进行中，请等待完成后再发送".to_string();
            return Ok(());
        }

        if self.draft_new_session {
            self.state.create_session();
            self.draft_new_session = false;
        }

        self.state.update_draft(raw);
        if let Err(err) = self.state.send_current_input() {
            self.status_message = format!("发送失败：{err}");
        } else {
            self.status_message = "正在请求模型...".to_string();
            self.follow_conversation_bottom = true;
        }
        self.input.clear();
        self.selected_hint_idx = 0;

        Ok(())
    }

    fn handle_command(&mut self, command: &str) -> Result<()> {
        match command {
            "/exit" | "/quit" => {
                self.should_quit = true;
                Ok(())
            }
            "/help" => {
                self.input = "/".to_string();
                self.selected_hint_idx = 0;
                self.status_message = "命令提示已打开".to_string();
                Ok(())
            }
            "/new" => {
                self.draft_new_session = true;
                self.history_modal = None;
                self.status_message = "已打开新对话（发送首条消息后才会记录）".to_string();
                self.input.clear();
                self.selected_hint_idx = 0;
                self.conversation_scroll = 0;
                self.max_conversation_scroll = 0;
                self.follow_conversation_bottom = true;
                Ok(())
            }
            _ if command == "/history" || command.starts_with("/history ") => {
                self.handle_history_command(command)
            }
            _ if command == "/model" || command.starts_with("/model ") => {
                self.handle_model_command(command)
            }
            _ if command == "/config" || command.starts_with("/config ") => {
                self.handle_config_command(command)
            }
            _ => {
                self.status_message = "未知命令，输入 /help 查看可用命令".to_string();
                Ok(())
            }
        }
    }

    fn handle_model_command(&mut self, command: &str) -> Result<()> {
        let arg = command.trim_start_matches("/model").trim();
        if arg.is_empty() {
            let current = self.state.current_model().to_string();
            self.input = "/model ".to_string();
            self.selected_hint_idx = 0;
            self.status_message = format!("当前模型：{current}，输入后缀可筛选并切换");
            return Ok(());
        }

        self.state.select_model(arg)?;
        self.status_message = format!("模型已切换为：{arg}");
        Ok(())
    }

    fn handle_history_command(&mut self, command: &str) -> Result<()> {
        let query = command.trim_start_matches("/history").trim();
        self.history_modal = Some(super::HistoryModalState {
            query: query.to_string(),
            selected_idx: 0,
        });
        self.status_message = "历史会话选择已打开（Esc 关闭）".to_string();
        Ok(())
    }

    fn handle_config_command(&mut self, command: &str) -> Result<()> {
        let args = command.trim_start_matches("/config").trim();
        if args.is_empty() || args == "show" {
            self.status_message = format!("当前配置：{}", self.state.agent_config_summary());
            return Ok(());
        }

        if args == "validate" {
            self.state.validate_agent_config()?;
            self.status_message = "配置校验通过".to_string();
            return Ok(());
        }

        if let Some(raw_set) = args.strip_prefix("set ") {
            let raw_set = raw_set.trim();
            if raw_set.is_empty() {
                return Err(anyhow!("缺少配置键，示例：/config set skills.enabled true"));
            }

            let mut parts = raw_set.splitn(2, char::is_whitespace);
            let key = parts
                .next()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("缺少配置键"))?;
            let value = parts
                .next()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("缺少配置值"))?;
            let message = self.state.update_agent_config_entry(key, value)?;
            self.status_message = message;
            return Ok(());
        }

        Err(anyhow!(
            "不支持的 /config 命令。可用：/config show、/config validate、/config set <key> <value>"
        ))
    }

    pub(super) fn confirm_history_modal_selection(&mut self) -> Result<()> {
        let Some(modal) = self.history_modal.as_ref() else {
            return Ok(());
        };
        let matched = self.history_match_indices(&modal.query);
        if matched.is_empty() {
            self.status_message = "未匹配到历史会话".to_string();
            return Ok(());
        }
        let picked = matched[modal.selected_idx.min(matched.len() - 1)];
        let Some((session_id, title)) = self
            .state
            .sessions()
            .get(picked)
            .map(|session| (session.id.clone(), session.title.clone()))
        else {
            return Err(anyhow!("所选历史会话不存在"));
        };

        self.state.switch_session(&session_id);
        self.draft_new_session = false;
        self.follow_conversation_bottom = true;
        self.history_modal = None;
        self.status_message = format!("已切换到历史会话：{title}");
        Ok(())
    }

    pub(super) fn move_history_modal_selection(&mut self, step: i32) {
        let Some((query, current_idx)) = self
            .history_modal
            .as_ref()
            .map(|modal| (modal.query.clone(), modal.selected_idx))
        else {
            return;
        };
        let matched = self.history_match_indices(&query);
        if matched.is_empty() {
            if let Some(modal) = self.history_modal.as_mut() {
                modal.selected_idx = 0;
            }
            return;
        }

        let current = current_idx.min(matched.len() - 1) as i32;
        let next = (current + step).clamp(0, matched.len() as i32 - 1);
        if let Some(modal) = self.history_modal.as_mut() {
            modal.selected_idx = next as usize;
        }
    }

    pub(super) fn move_history_modal_to_edge(&mut self, to_start: bool) {
        let Some(query) = self.history_modal.as_ref().map(|modal| modal.query.clone()) else {
            return;
        };
        let matched = self.history_match_indices(&query);
        if matched.is_empty() {
            if let Some(modal) = self.history_modal.as_mut() {
                modal.selected_idx = 0;
            }
            return;
        }
        if let Some(modal) = self.history_modal.as_mut() {
            modal.selected_idx = if to_start { 0 } else { matched.len() - 1 };
        }
    }

    pub(super) fn history_match_indices(&self, query: &str) -> Vec<usize> {
        let query = query.trim();
        self.state
            .sessions()
            .iter()
            .enumerate()
            .filter_map(|(idx, session)| {
                let index_text = (idx + 1).to_string();
                if query.is_empty()
                    || index_text.starts_with(query)
                    || session.title.contains(query)
                    || session.id.starts_with(query)
                {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    pub(super) fn move_hint_selection(&mut self, step: i8) {
        let hints = self.command_hints();
        let selectable = hints
            .iter()
            .enumerate()
            .filter_map(|(idx, hint)| hint.selectable.then_some(idx))
            .collect::<Vec<_>>();
        if selectable.is_empty() {
            return;
        }

        let current_pos = selectable
            .iter()
            .position(|idx| *idx == self.selected_hint_idx)
            .unwrap_or(0);
        let next_pos = if step >= 0 {
            (current_pos + 1) % selectable.len()
        } else if current_pos == 0 {
            selectable.len() - 1
        } else {
            current_pos - 1
        };
        self.selected_hint_idx = selectable[next_pos];
    }

    fn resolve_command_to_execute(&self, raw: &str) -> String {
        let hints = self.command_hints();
        let Some(idx) = self.selected_hint_position(&hints) else {
            return raw.to_string();
        };
        let hint = &hints[idx];
        if hint.selectable {
            hint.command.clone()
        } else {
            raw.to_string()
        }
    }

    pub(super) fn selected_hint_position(&self, hints: &[CommandHint]) -> Option<usize> {
        if hints.is_empty() {
            return None;
        }
        if self.selected_hint_idx < hints.len() && hints[self.selected_hint_idx].selectable {
            return Some(self.selected_hint_idx);
        }
        hints.iter().position(|hint| hint.selectable)
    }

    pub(super) fn command_hints(&self) -> Vec<CommandHint> {
        let raw = self.input.trim();
        if raw.is_empty() || !raw.starts_with('/') {
            return Vec::new();
        }

        if raw == "/history" {
            return vec![CommandHint::new("/history", "打开历史会话选择弹窗")];
        }
        if raw.starts_with("/history ") {
            return vec![CommandHint::new_note(
                "/history <关键词>",
                "回车后在弹窗中按关键词筛选历史会话",
            )];
        }

        if raw == "/model" || raw.starts_with("/model ") {
            return self.model_command_hints(raw);
        }
        if raw == "/config" || raw.starts_with("/config ") {
            return self.config_command_hints(raw);
        }

        let mut hints = vec![
            CommandHint::new("/model", "切换模型或查看可选模型"),
            CommandHint::new("/config", "查看或更新 Agent 配置"),
            CommandHint::new("/history", "恢复历史会话"),
            CommandHint::new("/new", "新建会话"),
            CommandHint::new("/help", "展示命令提示"),
            CommandHint::new("/exit", "退出 CLI"),
        ];

        if raw != "/" {
            hints.retain(|hint| hint.command.starts_with(raw));
        }

        if hints.is_empty() {
            hints.push(CommandHint::new_note(
                "/help",
                "未命中命令，继续输入后回车直接执行",
            ));
        }

        hints
    }

    fn config_command_hints(&self, raw: &str) -> Vec<CommandHint> {
        let mut hints = vec![
            CommandHint::new("/config show", "查看当前 Agent 配置摘要"),
            CommandHint::new("/config validate", "校验当前 Agent 配置"),
            CommandHint::new("/config set skills.enabled true", "开启或关闭 skills 功能"),
            CommandHint::new(
                "/config set skills.max_matches 3",
                "设置 skills 最大匹配数量",
            ),
            CommandHint::new(
                "/config set skills.dirs /path/a,/path/b",
                "设置 skills 目录列表（逗号分隔）",
            ),
            CommandHint::new("/config set mcp.enabled true", "开启或关闭 mcp 功能"),
            CommandHint::new("/config set mcp.timeout_ms 15000", "设置 mcp 超时（毫秒）"),
        ];

        if raw != "/config" {
            hints.retain(|hint| hint.command.starts_with(raw));
        }
        if hints.is_empty() {
            hints.push(CommandHint::new_note(
                "/config set <key> <value>",
                "支持键：skills.enabled、skills.max_matches、skills.dirs、mcp.enabled、mcp.timeout_ms",
            ));
        }

        hints
    }

    pub(super) fn is_command_palette_active(&self) -> bool {
        !self.command_hints().is_empty()
    }
}
