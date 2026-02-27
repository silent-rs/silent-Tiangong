use anyhow::{Result, anyhow};

use crate::core::planner::PlanStepStatus;

use super::{CliApp, CommandHint};

impl CliApp {
    pub(super) fn submit_input(&mut self) -> Result<()> {
        let raw = self.input.trim().to_string();

        if raw.is_empty() {
            return Ok(());
        }

        if raw.starts_with('/') {
            self.push_input_history(raw.clone());
            let command = self.resolve_command_to_execute(&raw);
            self.input.clear();
            self.input_cursor_char = 0;
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

        self.push_input_history(raw.clone());
        self.state.update_draft(raw);
        if let Err(err) = self.state.send_current_input() {
            self.status_message = format!("发送失败：{err}");
        } else {
            self.status_message = "正在请求模型...".to_string();
            self.follow_conversation_bottom = true;
        }
        self.input.clear();
        self.input_cursor_char = 0;
        self.reset_input_history_navigation();
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
                self.input_cursor_char = self.input.chars().count();
                self.selected_hint_idx = 0;
                self.status_message = "命令提示已打开".to_string();
                Ok(())
            }
            "/new" => {
                self.draft_new_session = true;
                self.history_modal = None;
                self.planning_modal = None;
                self.rebuild_input_history_from_active_session();
                self.status_message = "已打开新对话（发送首条消息后才会记录）".to_string();
                self.input.clear();
                self.input_cursor_char = 0;
                self.selected_hint_idx = 0;
                self.conversation_scroll = 0;
                self.max_conversation_scroll = 0;
                self.follow_conversation_bottom = true;
                Ok(())
            }
            "/cancel" => {
                if self.state.cancel_pending_turn()? {
                    self.status_message = "已取消当前任务".to_string();
                } else {
                    self.status_message = "当前没有可取消的任务".to_string();
                }
                Ok(())
            }
            _ if command == "/planing"
                || command.starts_with("/planing ")
                || command == "/plan"
                || command.starts_with("/plan ") =>
            {
                self.handle_planing_command(command)
            }
            _ if command == "/sessions" || command.starts_with("/sessions ") => {
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
            self.input_cursor_char = self.input.chars().count();
            self.selected_hint_idx = 0;
            self.status_message = format!("当前模型：{current}，输入后缀可筛选并切换");
            return Ok(());
        }

        self.state.select_model(arg)?;
        self.status_message = format!("模型已切换为：{arg}");
        Ok(())
    }

    fn handle_history_command(&mut self, command: &str) -> Result<()> {
        let query = command.trim_start_matches("/sessions").trim();
        self.planning_modal = None;
        self.history_modal = Some(super::HistoryModalState {
            query: query.to_string(),
            selected_idx: 0,
        });
        self.status_message = "会话切换已打开（Esc 关闭）".to_string();
        Ok(())
    }

    fn handle_planing_command(&mut self, command: &str) -> Result<()> {
        let args = if let Some(raw) = command.strip_prefix("/planing") {
            raw.trim()
        } else if let Some(raw) = command.strip_prefix("/plan") {
            raw.trim()
        } else {
            ""
        };

        if !args.is_empty() && args != "show" {
            return Err(anyhow!(
                "不支持的 /planing 命令，仅支持：/planing 或 /planing show"
            ));
        }

        self.history_modal = None;
        self.planning_modal = Some(super::PlanningModalState::default());
        self.clamp_planning_modal_selection();
        if let Some(first_pending_row) = self.plan_row_by_pending_index(1)
            && let Some(modal) = self.planning_modal.as_mut()
        {
            modal.selected_idx = first_pending_row;
        }
        self.status_message = "planning 列表已打开（D 删除 pending plan，K/J 调序）".to_string();
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
            self.status_message = "未匹配到会话".to_string();
            return Ok(());
        }
        let picked = matched[modal.selected_idx.min(matched.len() - 1)];
        let Some((session_id, title)) = self
            .state
            .sessions()
            .get(picked)
            .map(|session| (session.id.clone(), session.title.clone()))
        else {
            return Err(anyhow!("所选会话不存在"));
        };

        let had_pending_before = self.state.has_pending_turn();
        self.state.switch_session(&session_id);
        let auto_resumed = !had_pending_before && self.state.has_pending_turn();
        self.draft_new_session = false;
        self.rebuild_input_history_from_active_session();
        self.follow_conversation_bottom = true;
        self.history_modal = None;
        self.status_message = if auto_resumed {
            format!("已切换会话：{title}，检测到未完成 plan，已自动继续执行")
        } else {
            format!("已切换会话：{title}")
        };
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

    pub(super) fn move_planning_modal_selection(&mut self, step: i32) {
        let total = self.state.active_task_plans().len();
        if total == 0 {
            if let Some(modal) = self.planning_modal.as_mut() {
                modal.selected_idx = 0;
            }
            return;
        }

        let Some(current_idx) = self.planning_modal.as_ref().map(|modal| modal.selected_idx) else {
            return;
        };
        let current = current_idx.min(total - 1) as i32;
        let next = (current + step).clamp(0, total as i32 - 1) as usize;
        if let Some(modal) = self.planning_modal.as_mut() {
            modal.selected_idx = next;
        }
    }

    pub(super) fn move_planning_modal_to_edge(&mut self, to_start: bool) {
        let total = self.state.active_task_plans().len();
        if total == 0 {
            if let Some(modal) = self.planning_modal.as_mut() {
                modal.selected_idx = 0;
            }
            return;
        }

        if let Some(modal) = self.planning_modal.as_mut() {
            modal.selected_idx = if to_start { 0 } else { total - 1 };
        }
    }

    pub(super) fn delete_selected_pending_planning_step(&mut self) -> Result<()> {
        let Some(selected_row) = self.planning_modal.as_ref().map(|modal| modal.selected_idx)
        else {
            return Ok(());
        };
        let Some(pending_idx) = self.pending_index_by_plan_row(selected_row) else {
            self.status_message = "仅支持删除 pending plan".to_string();
            return Ok(());
        };

        if self.state.delete_pending_task_plan(pending_idx)? {
            self.status_message = format!("已删除 pending plan P{pending_idx}");
        } else {
            self.status_message = format!("删除失败，未找到 pending plan P{pending_idx}");
        }
        self.clamp_planning_modal_selection();
        Ok(())
    }

    pub(super) fn move_selected_pending_planning_step(&mut self, upward: bool) -> Result<()> {
        let Some(selected_row) = self.planning_modal.as_ref().map(|modal| modal.selected_idx)
        else {
            return Ok(());
        };
        let Some(from_pending_idx) = self.pending_index_by_plan_row(selected_row) else {
            self.status_message = "仅支持调序 pending plan".to_string();
            return Ok(());
        };

        let to_pending_idx = if upward {
            from_pending_idx.saturating_sub(1)
        } else {
            from_pending_idx.saturating_add(1)
        };
        if to_pending_idx == 0 {
            self.status_message = "已经是首个 pending plan".to_string();
            return Ok(());
        }

        if self
            .state
            .move_pending_task_plan(from_pending_idx, to_pending_idx)?
        {
            if let Some(new_row) = self.plan_row_by_pending_index(to_pending_idx)
                && let Some(modal) = self.planning_modal.as_mut()
            {
                modal.selected_idx = new_row;
            }
            self.status_message =
                format!("已调整 pending plan：P{from_pending_idx} -> P{to_pending_idx}");
        } else if upward {
            self.status_message = "已经是首个 pending plan".to_string();
        } else {
            self.status_message = "已经是最后一个 pending plan".to_string();
        }

        Ok(())
    }

    fn clamp_planning_modal_selection(&mut self) {
        let total = self.state.active_task_plans().len();
        if let Some(modal) = self.planning_modal.as_mut() {
            modal.selected_idx = if total == 0 {
                0
            } else {
                modal.selected_idx.min(total - 1)
            };
        }
    }

    fn pending_index_by_plan_row(&self, row_idx: usize) -> Option<usize> {
        let plans = self.state.active_task_plans();
        let mut pending_idx = 0usize;
        for (idx, plan) in plans.iter().enumerate() {
            if plan.status == PlanStepStatus::Pending {
                pending_idx += 1;
                if idx == row_idx {
                    return Some(pending_idx);
                }
            }
        }
        None
    }

    fn plan_row_by_pending_index(&self, pending_index_1_based: usize) -> Option<usize> {
        if pending_index_1_based == 0 {
            return None;
        }
        let plans = self.state.active_task_plans();
        let mut pending_idx = 0usize;
        for (idx, plan) in plans.iter().enumerate() {
            if plan.status == PlanStepStatus::Pending {
                pending_idx += 1;
                if pending_idx == pending_index_1_based {
                    return Some(idx);
                }
            }
        }
        None
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

        if raw == "/sessions" {
            return vec![CommandHint::new("/sessions", "切换会话")];
        }
        if raw.starts_with("/sessions ") {
            return vec![CommandHint::new_note(
                "/sessions <关键词>",
                "回车后在弹窗中按关键词筛选会话",
            )];
        }

        if raw == "/planing" || raw.starts_with("/planing ") || raw == "/plan" {
            return vec![CommandHint::new(
                "/planing",
                "打开 planning 列表弹窗（D删除，K/J调序）",
            )];
        }

        if raw == "/model" || raw.starts_with("/model ") {
            return self.model_command_hints(raw);
        }
        if raw == "/config" || raw.starts_with("/config ") {
            return self.config_command_hints(raw);
        }

        let mut hints = vec![
            CommandHint::new("/cancel", "取消当前执行中的任务"),
            CommandHint::new("/planing", "打开 planning 列表弹窗"),
            CommandHint::new("/model", "切换模型或查看可选模型"),
            CommandHint::new("/config", "查看或更新 Agent 配置"),
            CommandHint::new("/sessions", "切换会话"),
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
