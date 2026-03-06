use anyhow::{Result, anyhow};

use crate::core::agent_config::McpTransportMode;
use crate::core::app_state::{RegisterMcpServerOptions, RegisterMcpServerRequest};
use crate::core::planner::PlanStepStatus;

use super::{CliApp, CommandHint, McpModalState, SkillModalState};

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
                self.mcp_modal = None;
                self.skill_modal = None;
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
            _ if command == "/mcp" || command.starts_with("/mcp ") => {
                self.handle_mcp_command(command)
            }
            _ if command == "/skill" || command.starts_with("/skill ") => {
                self.handle_skill_command(command)
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
        self.mcp_modal = None;
        self.skill_modal = None;
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
        self.mcp_modal = None;
        self.skill_modal = None;
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

    fn handle_mcp_command(&mut self, command: &str) -> Result<()> {
        let query = command.trim_start_matches("/mcp").trim();
        self.history_modal = None;
        self.planning_modal = None;
        self.skill_modal = None;
        self.mcp_modal = Some(McpModalState {
            query: query.to_string(),
            selected_idx: 0,
            add_input: None,
        });
        self.clamp_mcp_modal_selection();
        self.status_message =
            "MCP 管理已打开（空格启/禁用 Backspace删除 Delete删筛选字 A新增 Esc关闭）".to_string();
        Ok(())
    }

    fn handle_skill_command(&mut self, command: &str) -> Result<()> {
        let query = command.trim_start_matches("/skill").trim();
        if query == "init" || query.starts_with("init ") {
            let init_args = if query == "init" {
                ""
            } else {
                query.trim_start_matches("init").trim()
            };
            let parsed = parse_skill_init_args(init_args)?;
            let message = self.state.init_skill_scaffold(
                &parsed.path,
                parsed.name.as_deref(),
                parsed.id.as_deref(),
                parsed.force,
            )?;
            self.history_modal = None;
            self.planning_modal = None;
            self.mcp_modal = None;
            self.skill_modal = None;
            self.status_message = message;
            return Ok(());
        }

        self.history_modal = None;
        self.planning_modal = None;
        self.mcp_modal = None;
        self.skill_modal = Some(SkillModalState {
            query: query.to_string(),
            selected_idx: 0,
            add_input: None,
        });
        self.clamp_skill_modal_selection();
        self.status_message =
            "Skill 管理已打开（空格启/禁用 Backspace删除 Delete删筛选字 A新增 Esc关闭）"
                .to_string();
        Ok(())
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

    pub(super) fn move_mcp_modal_selection(&mut self, step: i32) {
        let Some((query, current_idx)) = self
            .mcp_modal
            .as_ref()
            .map(|modal| (modal.query.clone(), modal.selected_idx))
        else {
            return;
        };
        let matched = self.mcp_match_indices(&query);
        if matched.is_empty() {
            if let Some(modal) = self.mcp_modal.as_mut() {
                modal.selected_idx = 0;
            }
            return;
        }

        let current = current_idx.min(matched.len() - 1) as i32;
        let next = (current + step).clamp(0, matched.len() as i32 - 1);
        if let Some(modal) = self.mcp_modal.as_mut() {
            modal.selected_idx = next as usize;
        }
    }

    pub(super) fn move_mcp_modal_to_edge(&mut self, to_start: bool) {
        let Some(query) = self.mcp_modal.as_ref().map(|modal| modal.query.clone()) else {
            return;
        };
        let matched = self.mcp_match_indices(&query);
        if matched.is_empty() {
            if let Some(modal) = self.mcp_modal.as_mut() {
                modal.selected_idx = 0;
            }
            return;
        }
        if let Some(modal) = self.mcp_modal.as_mut() {
            modal.selected_idx = if to_start { 0 } else { matched.len() - 1 };
        }
    }

    pub(super) fn enter_mcp_modal_add_mode(&mut self) {
        if let Some(modal) = self.mcp_modal.as_mut() {
            modal.add_input = Some(String::new());
        }
    }

    pub(super) fn cancel_mcp_modal_add_mode(&mut self) {
        if let Some(modal) = self.mcp_modal.as_mut() {
            modal.add_input = None;
        }
    }

    pub(super) fn push_mcp_modal_query_char(&mut self, ch: char) {
        if let Some(modal) = self.mcp_modal.as_mut() {
            modal.query.push(ch);
            modal.selected_idx = 0;
        }
    }

    pub(super) fn backspace_mcp_modal_query(&mut self) {
        if let Some(modal) = self.mcp_modal.as_mut() {
            modal.query.pop();
            modal.selected_idx = 0;
        }
    }

    pub(super) fn push_mcp_modal_add_input_char(&mut self, ch: char) {
        if let Some(modal) = self.mcp_modal.as_mut()
            && let Some(add_input) = modal.add_input.as_mut()
        {
            add_input.push(ch);
        }
    }

    pub(super) fn backspace_mcp_modal_add_input(&mut self) {
        if let Some(modal) = self.mcp_modal.as_mut()
            && let Some(add_input) = modal.add_input.as_mut()
        {
            add_input.pop();
        }
    }

    pub(super) fn confirm_mcp_modal_add(&mut self) -> Result<()> {
        let raw_input = self
            .mcp_modal
            .as_ref()
            .and_then(|modal| modal.add_input.clone())
            .unwrap_or_default();
        let parsed = parse_mcp_add_args(raw_input.trim())?;
        let name = parsed.name.clone();
        let message = self.state.register_mcp_server(RegisterMcpServerRequest {
            name: parsed.name.clone(),
            command: parsed.command,
            args: parsed.command_args,
            tags: parsed.tags,
            enabled: parsed.enabled,
            options: RegisterMcpServerOptions {
                transport: parsed.transport,
                endpoint: parsed.endpoint,
                auth_header: parsed.auth_header,
                headers: parsed.headers,
                env: parsed.env,
                cwd: parsed.cwd,
            },
        })?;
        if let Some(modal) = self.mcp_modal.as_mut() {
            modal.add_input = None;
            modal.query.clear();
        }
        self.focus_mcp_modal_server(&name);
        self.status_message = message;
        Ok(())
    }

    pub(super) fn remove_selected_mcp_server(&mut self) -> Result<()> {
        let Some(server_name) = self.selected_mcp_server_name() else {
            self.status_message = "未选中 MCP server".to_string();
            return Ok(());
        };
        let message = self.state.remove_mcp_server(&server_name)?;
        self.clamp_mcp_modal_selection();
        self.status_message = message;
        Ok(())
    }

    pub(super) fn set_selected_mcp_server_enabled(&mut self, enabled: bool) -> Result<()> {
        let Some(server_name) = self.selected_mcp_server_name() else {
            self.status_message = "未选中 MCP server".to_string();
            return Ok(());
        };
        let message = self.state.set_mcp_server_enabled(&server_name, enabled)?;
        self.clamp_mcp_modal_selection();
        self.status_message = message;
        Ok(())
    }

    pub(super) fn toggle_selected_mcp_server_enabled(&mut self) -> Result<()> {
        let Some(server_name) = self.selected_mcp_server_name() else {
            self.status_message = "未选中 MCP server".to_string();
            return Ok(());
        };
        let Some(current_enabled) = self
            .state
            .mcp_servers()
            .iter()
            .find(|server| server.name == server_name)
            .map(|server| server.enabled)
        else {
            self.status_message = "未找到选中的 MCP server".to_string();
            return Ok(());
        };
        self.set_selected_mcp_server_enabled(!current_enabled)
    }

    pub(super) fn mcp_match_indices(&self, query: &str) -> Vec<usize> {
        let query = query.trim();
        self.state
            .mcp_servers()
            .iter()
            .enumerate()
            .filter_map(|(idx, server)| {
                let index_text = (idx + 1).to_string();
                let matched = query.is_empty()
                    || index_text.starts_with(query)
                    || server.name.contains(query)
                    || server.command.contains(query)
                    || server.endpoint.contains(query)
                    || server.tags.iter().any(|tag| tag.contains(query));
                matched.then_some(idx)
            })
            .collect()
    }

    fn selected_mcp_server_name(&self) -> Option<String> {
        let modal = self.mcp_modal.as_ref()?;
        let matched = self.mcp_match_indices(&modal.query);
        let idx = *matched.get(modal.selected_idx.min(matched.len().saturating_sub(1)))?;
        self.state
            .mcp_servers()
            .get(idx)
            .map(|server| server.name.clone())
    }

    fn clamp_mcp_modal_selection(&mut self) {
        let Some((query, selected_idx)) = self
            .mcp_modal
            .as_ref()
            .map(|modal| (modal.query.clone(), modal.selected_idx))
        else {
            return;
        };
        let total = self.mcp_match_indices(&query).len();
        if let Some(modal) = self.mcp_modal.as_mut() {
            modal.selected_idx = if total == 0 {
                0
            } else {
                selected_idx.min(total - 1)
            };
        }
    }

    fn focus_mcp_modal_server(&mut self, server_name: &str) {
        let Some(query) = self.mcp_modal.as_ref().map(|modal| modal.query.clone()) else {
            return;
        };
        let matched = self.mcp_match_indices(&query);
        if let Some(position) = matched
            .iter()
            .position(|idx| self.state.mcp_servers()[*idx].name == server_name)
        {
            if let Some(modal) = self.mcp_modal.as_mut() {
                modal.selected_idx = position;
            }
        } else {
            self.clamp_mcp_modal_selection();
        }
    }

    pub(super) fn move_skill_modal_selection(&mut self, step: i32) {
        let Some((query, current_idx)) = self
            .skill_modal
            .as_ref()
            .map(|modal| (modal.query.clone(), modal.selected_idx))
        else {
            return;
        };
        let matched = self.skill_match_indices(&query);
        if matched.is_empty() {
            if let Some(modal) = self.skill_modal.as_mut() {
                modal.selected_idx = 0;
            }
            return;
        }

        let current = current_idx.min(matched.len() - 1) as i32;
        let next = (current + step).clamp(0, matched.len() as i32 - 1);
        if let Some(modal) = self.skill_modal.as_mut() {
            modal.selected_idx = next as usize;
        }
    }

    pub(super) fn move_skill_modal_to_edge(&mut self, to_start: bool) {
        let Some(query) = self.skill_modal.as_ref().map(|modal| modal.query.clone()) else {
            return;
        };
        let matched = self.skill_match_indices(&query);
        if matched.is_empty() {
            if let Some(modal) = self.skill_modal.as_mut() {
                modal.selected_idx = 0;
            }
            return;
        }
        if let Some(modal) = self.skill_modal.as_mut() {
            modal.selected_idx = if to_start { 0 } else { matched.len() - 1 };
        }
    }

    pub(super) fn enter_skill_modal_add_mode(&mut self) {
        if let Some(modal) = self.skill_modal.as_mut() {
            modal.add_input = Some(String::new());
        }
    }

    pub(super) fn cancel_skill_modal_add_mode(&mut self) {
        if let Some(modal) = self.skill_modal.as_mut() {
            modal.add_input = None;
        }
    }

    pub(super) fn push_skill_modal_query_char(&mut self, ch: char) {
        if let Some(modal) = self.skill_modal.as_mut() {
            modal.query.push(ch);
            modal.selected_idx = 0;
        }
    }

    pub(super) fn backspace_skill_modal_query(&mut self) {
        if let Some(modal) = self.skill_modal.as_mut() {
            modal.query.pop();
            modal.selected_idx = 0;
        }
    }

    pub(super) fn push_skill_modal_add_input_char(&mut self, ch: char) {
        if let Some(modal) = self.skill_modal.as_mut()
            && let Some(add_input) = modal.add_input.as_mut()
        {
            add_input.push(ch);
        }
    }

    pub(super) fn backspace_skill_modal_add_input(&mut self) {
        if let Some(modal) = self.skill_modal.as_mut()
            && let Some(add_input) = modal.add_input.as_mut()
        {
            add_input.pop();
        }
    }

    pub(super) fn confirm_skill_modal_add(&mut self) -> Result<()> {
        let raw_input = self
            .skill_modal
            .as_ref()
            .and_then(|modal| modal.add_input.clone())
            .unwrap_or_default();
        let parsed = parse_skill_add_args(raw_input.trim())?;
        let message = if parsed.convert {
            self.state
                .install_local_skill_with_options(&parsed.path, parsed.enabled, true)?
        } else {
            self.state
                .install_local_skill(&parsed.path, parsed.enabled)?
        };
        let skill_id = self
            .state
            .installed_skills()
            .last()
            .map(|skill| skill.id.clone())
            .unwrap_or_default();
        if let Some(modal) = self.skill_modal.as_mut() {
            modal.add_input = None;
            modal.query.clear();
        }
        if !skill_id.is_empty() {
            self.focus_skill_modal(&skill_id);
        } else {
            self.clamp_skill_modal_selection();
        }
        self.status_message = message;
        Ok(())
    }

    pub(super) fn remove_selected_skill(&mut self) -> Result<()> {
        let Some(skill_id) = self.selected_skill_id() else {
            self.status_message = "未选中 skill".to_string();
            return Ok(());
        };
        let message = self.state.remove_skill(&skill_id)?;
        self.clamp_skill_modal_selection();
        self.status_message = message;
        Ok(())
    }

    pub(super) fn set_selected_skill_enabled(&mut self, enabled: bool) -> Result<()> {
        let Some(skill_id) = self.selected_skill_id() else {
            self.status_message = "未选中 skill".to_string();
            return Ok(());
        };
        let message = self.state.set_skill_enabled(&skill_id, enabled)?;
        self.clamp_skill_modal_selection();
        self.status_message = message;
        Ok(())
    }

    pub(super) fn toggle_selected_skill_enabled(&mut self) -> Result<()> {
        let Some(skill_id) = self.selected_skill_id() else {
            self.status_message = "未选中 skill".to_string();
            return Ok(());
        };
        let Some(current_enabled) = self
            .state
            .installed_skills()
            .iter()
            .find(|skill| skill.id == skill_id)
            .map(|skill| skill.enabled)
        else {
            self.status_message = "未找到选中的 skill".to_string();
            return Ok(());
        };
        self.set_selected_skill_enabled(!current_enabled)
    }

    pub(super) fn skill_match_indices(&self, query: &str) -> Vec<usize> {
        let query = query.trim();
        self.state
            .installed_skills()
            .iter()
            .enumerate()
            .filter_map(|(idx, skill)| {
                let index_text = (idx + 1).to_string();
                let matched = query.is_empty()
                    || index_text.starts_with(query)
                    || skill.id.contains(query)
                    || skill.name.contains(query)
                    || skill.version.contains(query)
                    || skill.source.value.contains(query);
                matched.then_some(idx)
            })
            .collect()
    }

    fn selected_skill_id(&self) -> Option<String> {
        let modal = self.skill_modal.as_ref()?;
        let matched = self.skill_match_indices(&modal.query);
        let idx = *matched.get(modal.selected_idx.min(matched.len().saturating_sub(1)))?;
        self.state
            .installed_skills()
            .get(idx)
            .map(|skill| skill.id.clone())
    }

    fn clamp_skill_modal_selection(&mut self) {
        let Some((query, selected_idx)) = self
            .skill_modal
            .as_ref()
            .map(|modal| (modal.query.clone(), modal.selected_idx))
        else {
            return;
        };
        let total = self.skill_match_indices(&query).len();
        if let Some(modal) = self.skill_modal.as_mut() {
            modal.selected_idx = if total == 0 {
                0
            } else {
                selected_idx.min(total - 1)
            };
        }
    }

    fn focus_skill_modal(&mut self, skill_id: &str) {
        let Some(query) = self.skill_modal.as_ref().map(|modal| modal.query.clone()) else {
            return;
        };
        let matched = self.skill_match_indices(&query);
        if let Some(position) = matched
            .iter()
            .position(|idx| self.state.installed_skills()[*idx].id == skill_id)
        {
            if let Some(modal) = self.skill_modal.as_mut() {
                modal.selected_idx = position;
            }
        } else {
            self.clamp_skill_modal_selection();
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
        if raw == "/mcp" || raw.starts_with("/mcp ") {
            return self.mcp_command_hints(raw);
        }
        if raw == "/skill" || raw.starts_with("/skill ") {
            return self.skill_command_hints(raw);
        }
        if raw == "/config" || raw.starts_with("/config ") {
            return self.config_command_hints(raw);
        }

        let mut hints = vec![
            CommandHint::new("/cancel", "取消当前执行中的任务"),
            CommandHint::new("/planing", "打开 planning 列表弹窗"),
            CommandHint::new("/model", "切换模型或查看可选模型"),
            CommandHint::new("/mcp", "注册和管理 MCP server"),
            CommandHint::new("/skill", "安装和管理 Skill"),
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

    fn mcp_command_hints(&self, raw: &str) -> Vec<CommandHint> {
        if raw == "/mcp" {
            vec![CommandHint::new("/mcp", "打开 MCP 管理弹窗")]
        } else {
            Vec::new()
        }
    }

    fn skill_command_hints(&self, raw: &str) -> Vec<CommandHint> {
        if raw.starts_with("/skill init") {
            return vec![
                CommandHint::new(
                    "/skill init ./my-skill",
                    "初始化 Skill 脚手架（SKILL.md/skill.toml）",
                ),
                CommandHint::new_note(
                    "带参数示例",
                    "/skill init ./my-skill --name MySkill --id my-skill --force",
                ),
            ];
        }

        let mut hints = vec![
            CommandHint::new("/skill", "打开 Skill 管理弹窗"),
            CommandHint::new("/skill fs", "打开弹窗并按关键词筛选"),
            CommandHint::new(
                "/skill init ./my-skill",
                "初始化 Skill 脚手架（SKILL.md/skill.toml）",
            ),
            CommandHint::new_note("新增示例", "/path/to/skill [--disabled] [--convert]"),
            CommandHint::new_note(
                "弹窗内快捷键",
                "空格启/禁用 Backspace删除 Delete删筛选字 A新增 Esc关闭",
            ),
        ];

        if raw != "/skill" {
            hints.retain(|hint| hint.command.starts_with(raw));
        }
        if hints.is_empty() {
            hints.push(CommandHint::new_note(
                "/skill <关键词>",
                "打开 Skill 管理弹窗并筛选目标 skill",
            ));
        }

        hints
    }

    pub(super) fn is_command_palette_active(&self) -> bool {
        !self.command_hints().is_empty()
    }
}

struct ParsedMcpAddArgs {
    name: String,
    command: String,
    command_args: Vec<String>,
    tags: Vec<String>,
    enabled: bool,
    transport: Option<McpTransportMode>,
    endpoint: Option<String>,
    auth_header: Option<String>,
    headers: Vec<(String, String)>,
    env: Vec<(String, String)>,
    cwd: Option<String>,
}

struct ParsedSkillAddArgs {
    path: String,
    enabled: bool,
    convert: bool,
}

struct ParsedSkillInitArgs {
    path: String,
    name: Option<String>,
    id: Option<String>,
    force: bool,
}

fn parse_mcp_add_args(raw: &str) -> Result<ParsedMcpAddArgs> {
    let tokens = raw.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err(anyhow!(
            "参数不足，示例：browser npx -y @modelcontextprotocol/server-browser --tags web,browser 或 remote --transport http --endpoint https://example.com/mcp"
        ));
    }

    let name = tokens[0].trim();
    if name.is_empty() {
        return Err(anyhow!("MCP server 名称不能为空"));
    }

    let mut enabled = true;
    let mut tags = Vec::new();
    let mut transport = None;
    let mut endpoint = None;
    let mut auth_header = None;
    let mut headers = Vec::new();
    let mut env = Vec::new();
    let mut cwd = None;
    let mut non_flag_tokens = Vec::new();
    let mut idx = 1usize;
    while idx < tokens.len() {
        match tokens[idx] {
            "--disabled" => {
                enabled = false;
                idx += 1;
            }
            "--enabled" => {
                enabled = true;
                idx += 1;
            }
            "--tags" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--tags 缺少参数，示例：--tags web,browser"))?;
                tags.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(ToString::to_string),
                );
                idx += 2;
            }
            "--transport" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--transport 缺少参数，可选：auto|stdio|http"))?;
                transport = Some(parse_transport(value)?);
                idx += 2;
            }
            "--endpoint" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--endpoint 缺少参数"))?;
                endpoint = Some(value.trim().to_string());
                idx += 2;
            }
            "--auth-header" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--auth-header 缺少参数"))?;
                auth_header = Some(value.trim().to_string());
                idx += 2;
            }
            "--header" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--header 缺少参数，示例：--header X-Tenant=demo"))?;
                headers.push(parse_key_value(value, "--header")?);
                idx += 2;
            }
            "--env" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--env 缺少参数，示例：--env NODE_ENV=production"))?;
                env.push(parse_key_value(value, "--env")?);
                idx += 2;
            }
            "--cwd" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--cwd 缺少参数"))?;
                cwd = Some(value.trim().to_string());
                idx += 2;
            }
            token => {
                non_flag_tokens.push(token.to_string());
                idx += 1;
            }
        }
    }

    let command = non_flag_tokens.first().cloned().unwrap_or_default();
    let command_args = non_flag_tokens.into_iter().skip(1).collect::<Vec<_>>();
    Ok(ParsedMcpAddArgs {
        name: name.to_string(),
        command,
        command_args,
        tags,
        enabled,
        transport,
        endpoint,
        auth_header,
        headers,
        env,
        cwd,
    })
}

fn parse_skill_add_args(raw: &str) -> Result<ParsedSkillAddArgs> {
    let tokens = raw.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err(anyhow!(
            "参数不足，示例：/path/to/skill 或 /path/to/skill --disabled --convert"
        ));
    }

    let path = tokens[0].trim();
    if path.is_empty() {
        return Err(anyhow!("skill 路径不能为空"));
    }
    let mut enabled = true;
    let mut convert = false;
    for token in tokens.iter().skip(1) {
        match *token {
            "--disabled" => enabled = false,
            "--enabled" => enabled = true,
            "--convert" => convert = true,
            other => {
                return Err(anyhow!(
                    "不支持的参数：{other}，仅支持 --enabled/--disabled/--convert"
                ));
            }
        }
    }

    Ok(ParsedSkillAddArgs {
        path: path.to_string(),
        enabled,
        convert,
    })
}

fn parse_skill_init_args(raw: &str) -> Result<ParsedSkillInitArgs> {
    let tokens = raw.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err(anyhow!(
            "参数不足，示例：/skill init ./my-skill [--name MySkill] [--id my-skill] [--force]"
        ));
    }

    let path = tokens[0].trim();
    if path.is_empty() {
        return Err(anyhow!("skill 初始化目录不能为空"));
    }

    let mut name = None;
    let mut id = None;
    let mut force = false;
    let mut idx = 1usize;
    while idx < tokens.len() {
        match tokens[idx] {
            "--name" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--name 缺少参数"))?;
                let value = value.trim();
                if value.is_empty() {
                    return Err(anyhow!("--name 不能为空"));
                }
                name = Some(value.to_string());
                idx += 2;
            }
            "--id" => {
                let value = tokens
                    .get(idx + 1)
                    .ok_or_else(|| anyhow!("--id 缺少参数"))?;
                let value = value.trim();
                if value.is_empty() {
                    return Err(anyhow!("--id 不能为空"));
                }
                id = Some(value.to_string());
                idx += 2;
            }
            "--force" => {
                force = true;
                idx += 1;
            }
            other => {
                return Err(anyhow!("不支持的参数：{other}，仅支持 --name/--id/--force"));
            }
        }
    }

    Ok(ParsedSkillInitArgs {
        path: path.to_string(),
        name,
        id,
        force,
    })
}

fn parse_transport(raw: &str) -> Result<McpTransportMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(McpTransportMode::Auto),
        "stdio" => Ok(McpTransportMode::Stdio),
        "http" => Ok(McpTransportMode::Http),
        _ => Err(anyhow!("--transport 仅支持 auto|stdio|http")),
    }
}

fn parse_key_value(raw: &str, flag: &str) -> Result<(String, String)> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| anyhow!("{flag} 参数格式错误，需 key=value：{raw}"))?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() || value.is_empty() {
        return Err(anyhow!("{flag} 参数格式错误，key/value 不能为空：{raw}"));
    }
    Ok((key.to_string(), value.to_string()))
}
