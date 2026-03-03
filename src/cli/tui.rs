use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::core::app_state::TiangongState;
use crate::core::runtime::RunStatus;
use crate::core::session::MessageRole;

mod commands;
mod components;
mod render;
mod transcript;

const TICK_RATE: Duration = Duration::from_millis(60);
const CONVERSATION_SCROLL_PAGE_STEP: u16 = 16;
const CONVERSATION_SCROLL_MOUSE_STEP: u16 = 3;
const PLAN_SCROLL_PAGE_STEP: u16 = 12;
const PLAN_SCROLL_MOUSE_STEP: u16 = 2;
const INPUT_HISTORY_MAX_LEN: usize = 200;

type CliTerminal = Terminal<CrosstermBackend<Stdout>>;

pub fn run_cli() -> Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = CliApp::new();

    let run_result = app.run(&mut terminal);
    let restore_result = restore_terminal(terminal);

    match (run_result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(err), Ok(())) => Err(err),
        (Ok(()), Err(err)) => Err(err),
        (Err(run_err), Err(restore_err)) => Err(anyhow!("{run_err}; 终端恢复失败：{restore_err}")),
    }
}

struct CliApp {
    state: TiangongState,
    input: String,
    input_cursor_char: usize,
    input_history: Vec<String>,
    input_history_cursor: Option<usize>,
    input_history_draft: Option<String>,
    status_message: String,
    selected_hint_idx: usize,
    history_modal: Option<HistoryModalState>,
    planning_modal: Option<PlanningModalState>,
    mcp_modal: Option<McpModalState>,
    draft_new_session: bool,
    conversation_scroll: u16,
    max_conversation_scroll: u16,
    follow_conversation_bottom: bool,
    conversation_panel_area: Option<Rect>,
    plan_scroll: u16,
    max_plan_scroll: u16,
    plan_panel_area: Option<Rect>,
    was_pending_turn: bool,
    should_quit: bool,
}

impl CliApp {
    fn new() -> Self {
        let mut app = Self {
            state: TiangongState::load_or_default(),
            input: String::new(),
            input_cursor_char: 0,
            input_history: Vec::new(),
            input_history_cursor: None,
            input_history_draft: None,
            status_message: "输入 / 查看命令提示".to_string(),
            selected_hint_idx: 0,
            history_modal: None,
            planning_modal: None,
            mcp_modal: None,
            draft_new_session: false,
            conversation_scroll: 0,
            max_conversation_scroll: 0,
            follow_conversation_bottom: true,
            conversation_panel_area: None,
            plan_scroll: 0,
            max_plan_scroll: 0,
            plan_panel_area: None,
            was_pending_turn: false,
            should_quit: false,
        };
        app.rebuild_input_history_from_active_session();
        app
    }

    fn run(&mut self, terminal: &mut CliTerminal) -> Result<()> {
        while !self.should_quit {
            self.state.poll_pending_turn();
            self.sync_status_after_poll();
            terminal.draw(|frame| self.render(frame))?;

            if event::poll(TICK_RATE)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key)?,
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn sync_status_after_poll(&mut self) {
        let is_pending = self.state.has_pending_turn();
        if self.was_pending_turn && !is_pending {
            match self.state.run.status {
                RunStatus::Completed => {
                    self.status_message = "本轮对话已完成".to_string();
                }
                RunStatus::Failed => {
                    self.status_message = self
                        .state
                        .run
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "本轮对话执行失败".to_string());
                }
                _ => {}
            }
        }
        self.was_pending_turn = is_pending;
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(());
        }

        if self.history_modal.is_some() {
            return self.handle_history_modal_key(key);
        }
        if self.planning_modal.is_some() {
            return self.handle_planning_modal_key(key);
        }
        if self.mcp_modal.is_some() {
            return self.handle_mcp_modal_key(key);
        }

        match key.code {
            KeyCode::Esc => {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.input_cursor_char = 0;
                    self.reset_input_history_navigation();
                    self.selected_hint_idx = 0;
                    self.status_message = "已清空输入".to_string();
                }
            }
            KeyCode::Up => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.navigate_input_history(true);
                } else if self.is_command_palette_active() {
                    self.move_hint_selection(-1);
                } else if self.should_use_plain_arrow_for_history() {
                    self.navigate_input_history(true);
                } else {
                    self.move_input_cursor_up();
                }
            }
            KeyCode::Down => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.navigate_input_history(false);
                } else if self.is_command_palette_active() {
                    self.move_hint_selection(1);
                } else if self.should_use_plain_arrow_for_history() {
                    self.navigate_input_history(false);
                } else {
                    self.move_input_cursor_down();
                }
            }
            KeyCode::Left => self.move_input_cursor_left(),
            KeyCode::Right => self.move_input_cursor_right(),
            KeyCode::PageUp if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_plan_up(PLAN_SCROLL_PAGE_STEP)
            }
            KeyCode::PageDown if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_plan_down(PLAN_SCROLL_PAGE_STEP)
            }
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_plan_to_top()
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_plan_to_bottom()
            }
            KeyCode::PageUp => self.scroll_conversation_up(CONVERSATION_SCROLL_PAGE_STEP),
            KeyCode::PageDown => self.scroll_conversation_down(CONVERSATION_SCROLL_PAGE_STEP),
            KeyCode::Home => self.scroll_conversation_to_top(),
            KeyCode::End => self.scroll_conversation_to_bottom(),
            KeyCode::Tab => self.move_hint_selection(1),
            KeyCode::BackTab => self.move_hint_selection(-1),
            KeyCode::Enter => self.submit_input()?,
            KeyCode::Backspace => {
                self.backspace_input_char();
                self.reset_input_history_navigation();
                self.selected_hint_idx = 0;
            }
            KeyCode::Delete => {
                self.delete_input_char();
                self.reset_input_history_navigation();
                self.selected_hint_idx = 0;
            }
            KeyCode::Char(ch) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    self.insert_input_char(ch);
                    self.reset_input_history_navigation();
                    self.selected_hint_idx = 0;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.history_modal.is_some() || self.planning_modal.is_some() || self.mcp_modal.is_some()
        {
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if mouse.modifiers.contains(KeyModifiers::CONTROL)
                    || self.is_mouse_over_plan_panel(mouse.column, mouse.row)
                {
                    self.scroll_plan_up(PLAN_SCROLL_MOUSE_STEP);
                } else {
                    self.scroll_conversation_up(CONVERSATION_SCROLL_MOUSE_STEP);
                }
            }
            MouseEventKind::ScrollDown => {
                if mouse.modifiers.contains(KeyModifiers::CONTROL)
                    || self.is_mouse_over_plan_panel(mouse.column, mouse.row)
                {
                    self.scroll_plan_down(PLAN_SCROLL_MOUSE_STEP);
                } else {
                    self.scroll_conversation_down(CONVERSATION_SCROLL_MOUSE_STEP);
                }
            }
            _ => {}
        }
    }

    fn handle_history_modal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.history_modal = None;
                self.status_message = "已关闭会话切换".to_string();
            }
            KeyCode::Enter => self.confirm_history_modal_selection()?,
            KeyCode::Up => self.move_history_modal_selection(-1),
            KeyCode::Down => self.move_history_modal_selection(1),
            KeyCode::PageUp => self.move_history_modal_selection(-8),
            KeyCode::PageDown => self.move_history_modal_selection(8),
            KeyCode::Home => self.move_history_modal_to_edge(true),
            KeyCode::End => self.move_history_modal_to_edge(false),
            KeyCode::Backspace => {
                if let Some(modal) = self.history_modal.as_mut() {
                    modal.query.pop();
                    modal.selected_idx = 0;
                }
            }
            KeyCode::Char(ch) => {
                if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    && let Some(modal) = self.history_modal.as_mut()
                {
                    modal.query.push(ch);
                    modal.selected_idx = 0;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_planning_modal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.planning_modal = None;
                self.status_message = "已关闭 planning 列表".to_string();
            }
            KeyCode::Up => self.move_planning_modal_selection(-1),
            KeyCode::Down => self.move_planning_modal_selection(1),
            KeyCode::PageUp => self.move_planning_modal_selection(-8),
            KeyCode::PageDown => self.move_planning_modal_selection(8),
            KeyCode::Home => self.move_planning_modal_to_edge(true),
            KeyCode::End => self.move_planning_modal_to_edge(false),
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.delete_selected_pending_planning_step()?;
            }
            KeyCode::Char('k') | KeyCode::Char('K') => {
                self.move_selected_pending_planning_step(true)?;
            }
            KeyCode::Char('j') | KeyCode::Char('J') => {
                self.move_selected_pending_planning_step(false)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_mcp_modal_key(&mut self, key: KeyEvent) -> Result<()> {
        let is_adding = self
            .mcp_modal
            .as_ref()
            .is_some_and(|modal| modal.add_input.is_some());

        match key.code {
            KeyCode::Esc => {
                if is_adding {
                    self.cancel_mcp_modal_add_mode();
                    self.status_message = "已取消新增 MCP server".to_string();
                } else {
                    self.mcp_modal = None;
                    self.status_message = "已关闭 MCP 管理".to_string();
                }
            }
            KeyCode::Enter => {
                if is_adding {
                    self.confirm_mcp_modal_add()?;
                }
            }
            KeyCode::Up if !is_adding => self.move_mcp_modal_selection(-1),
            KeyCode::Down if !is_adding => self.move_mcp_modal_selection(1),
            KeyCode::PageUp if !is_adding => self.move_mcp_modal_selection(-8),
            KeyCode::PageDown if !is_adding => self.move_mcp_modal_selection(8),
            KeyCode::Home if !is_adding => self.move_mcp_modal_to_edge(true),
            KeyCode::End if !is_adding => self.move_mcp_modal_to_edge(false),
            KeyCode::Backspace => {
                if is_adding {
                    self.backspace_mcp_modal_add_input();
                } else {
                    self.remove_selected_mcp_server()?;
                }
            }
            KeyCode::Delete if !is_adding => self.backspace_mcp_modal_query(),
            KeyCode::Char('a') | KeyCode::Char('A') if !is_adding => {
                self.enter_mcp_modal_add_mode();
                self.status_message =
                    "请输入新增参数：<name> <command> [args...] [--tags t1,t2] [--transport auto|stdio|http] [--endpoint url] [--auth-header token] [--header k=v] [--env k=v] [--cwd path] [--disabled]"
                        .to_string();
            }
            KeyCode::Char(' ') if !is_adding => {
                self.toggle_selected_mcp_server_enabled()?;
            }
            KeyCode::Char(ch) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    if is_adding {
                        self.push_mcp_modal_add_input_char(ch);
                    } else {
                        self.push_mcp_modal_query_char(ch);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn scroll_conversation_up(&mut self, lines: u16) {
        self.conversation_scroll = self.conversation_scroll.saturating_sub(lines);
        self.follow_conversation_bottom = false;
    }

    fn scroll_conversation_down(&mut self, lines: u16) {
        self.conversation_scroll = self
            .conversation_scroll
            .saturating_add(lines)
            .min(self.max_conversation_scroll);
        if self.conversation_scroll >= self.max_conversation_scroll {
            self.follow_conversation_bottom = true;
        }
    }

    fn scroll_conversation_to_top(&mut self) {
        self.conversation_scroll = 0;
        self.follow_conversation_bottom = false;
    }

    fn scroll_conversation_to_bottom(&mut self) {
        self.conversation_scroll = self.max_conversation_scroll;
        self.follow_conversation_bottom = true;
    }

    fn scroll_plan_up(&mut self, lines: u16) {
        self.plan_scroll = self.plan_scroll.saturating_sub(lines);
    }

    fn scroll_plan_down(&mut self, lines: u16) {
        self.plan_scroll = self
            .plan_scroll
            .saturating_add(lines)
            .min(self.max_plan_scroll);
    }

    fn scroll_plan_to_top(&mut self) {
        self.plan_scroll = 0;
    }

    fn scroll_plan_to_bottom(&mut self) {
        self.plan_scroll = self.max_plan_scroll;
    }

    fn is_mouse_over_plan_panel(&self, column: u16, row: u16) -> bool {
        let Some(area) = self.plan_panel_area else {
            return false;
        };
        point_in_rect(column, row, area)
    }

    fn move_input_cursor_left(&mut self) {
        self.input_cursor_char = self.input_cursor_char.saturating_sub(1);
    }

    fn move_input_cursor_right(&mut self) {
        self.input_cursor_char = self
            .input_cursor_char
            .saturating_add(1)
            .min(self.input_char_len());
    }

    fn move_input_cursor_up(&mut self) {
        self.input_cursor_char = 0;
    }

    fn move_input_cursor_down(&mut self) {
        self.input_cursor_char = self.input_char_len();
    }

    fn insert_input_char(&mut self, ch: char) {
        let byte_idx = self.byte_index_for_char_pos(self.input_cursor_char);
        self.input.insert(byte_idx, ch);
        self.input_cursor_char = self.input_cursor_char.saturating_add(1);
    }

    fn backspace_input_char(&mut self) {
        if self.input_cursor_char == 0 {
            return;
        }
        let end = self.byte_index_for_char_pos(self.input_cursor_char);
        let start = self.byte_index_for_char_pos(self.input_cursor_char.saturating_sub(1));
        self.input.replace_range(start..end, "");
        self.input_cursor_char = self.input_cursor_char.saturating_sub(1);
    }

    fn delete_input_char(&mut self) {
        if self.input_cursor_char >= self.input_char_len() {
            return;
        }
        let start = self.byte_index_for_char_pos(self.input_cursor_char);
        let end = self.byte_index_for_char_pos(self.input_cursor_char.saturating_add(1));
        self.input.replace_range(start..end, "");
    }

    pub(in crate::cli::tui) fn input_cursor_prefix_display_width(&self) -> u16 {
        let byte_idx = self.byte_index_for_char_pos(self.input_cursor_char);
        crate::cli::tui::transcript::text_display_width(&self.input[..byte_idx])
    }

    fn input_char_len(&self) -> usize {
        self.input.chars().count()
    }

    fn byte_index_for_char_pos(&self, char_pos: usize) -> usize {
        char_to_byte_index(&self.input, char_pos)
    }

    fn push_input_history(&mut self, raw: String) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        if self
            .input_history
            .last()
            .is_some_and(|existing| existing == trimmed)
        {
            self.reset_input_history_navigation();
            return;
        }
        self.input_history.push(trimmed.to_string());
        if self.input_history.len() > INPUT_HISTORY_MAX_LEN {
            let overflow = self.input_history.len() - INPUT_HISTORY_MAX_LEN;
            self.input_history.drain(0..overflow);
        }
        self.reset_input_history_navigation();
    }

    pub(super) fn rebuild_input_history_from_active_session(&mut self) {
        if self.draft_new_session {
            self.input_history.clear();
            self.reset_input_history_navigation();
            return;
        }

        let Some(session) = self.state.active_session() else {
            self.input_history.clear();
            self.reset_input_history_navigation();
            return;
        };

        let mut history = Vec::new();
        for message in &session.messages {
            if message.role != MessageRole::User {
                continue;
            }
            let trimmed = message.content.trim();
            if trimmed.is_empty() {
                continue;
            }
            if history
                .last()
                .is_some_and(|existing: &String| existing == trimmed)
            {
                continue;
            }
            history.push(trimmed.to_string());
        }

        if history.len() > INPUT_HISTORY_MAX_LEN {
            let overflow = history.len() - INPUT_HISTORY_MAX_LEN;
            history.drain(0..overflow);
        }
        self.input_history = history;
        self.reset_input_history_navigation();
    }

    fn reset_input_history_navigation(&mut self) {
        self.input_history_cursor = None;
        self.input_history_draft = None;
    }

    fn navigate_input_history(&mut self, older: bool) {
        if self.input_history.is_empty() {
            return;
        }

        if older {
            let next_idx = match self.input_history_cursor {
                Some(current) => current.saturating_sub(1),
                None => {
                    self.input_history_draft = Some(self.input.clone());
                    self.input_history.len().saturating_sub(1)
                }
            };
            self.input_history_cursor = Some(next_idx);
            if let Some(value) = self.input_history.get(next_idx) {
                self.input = value.clone();
                self.input_cursor_char = self.input_char_len();
                self.selected_hint_idx = 0;
            }
            return;
        }

        let Some(current) = self.input_history_cursor else {
            return;
        };
        if current + 1 < self.input_history.len() {
            let next_idx = current + 1;
            self.input_history_cursor = Some(next_idx);
            if let Some(value) = self.input_history.get(next_idx) {
                self.input = value.clone();
                self.input_cursor_char = self.input_char_len();
                self.selected_hint_idx = 0;
            }
            return;
        }

        self.input_history_cursor = None;
        self.input = self.input_history_draft.take().unwrap_or_default();
        self.input_cursor_char = self.input_char_len();
        self.selected_hint_idx = 0;
    }

    fn should_use_plain_arrow_for_history(&self) -> bool {
        self.input_history_cursor.is_some()
            || (self.input.is_empty() && !self.input_history.is_empty())
    }
}

fn point_in_rect(column: u16, row: u16, area: Rect) -> bool {
    let x_end = area.x.saturating_add(area.width);
    let y_end = area.y.saturating_add(area.height);
    column >= area.x && column < x_end && row >= area.y && row < y_end
}

#[derive(Debug, Clone, Default)]
struct HistoryModalState {
    query: String,
    selected_idx: usize,
}

#[derive(Debug, Clone, Default)]
struct PlanningModalState {
    selected_idx: usize,
}

#[derive(Debug, Clone, Default)]
struct McpModalState {
    query: String,
    selected_idx: usize,
    add_input: Option<String>,
}

struct CommandHint {
    command: String,
    description: String,
    selectable: bool,
}

impl CommandHint {
    fn new(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            selectable: true,
        }
    }

    fn new_note(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            selectable: false,
        }
    }
}

fn init_terminal() -> Result<CliTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(mut terminal: CliTerminal) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn char_to_byte_index(text: &str, char_pos: usize) -> usize {
    let clamped = char_pos.min(text.chars().count());
    if clamped == 0 {
        return 0;
    }
    text.char_indices()
        .nth(clamped)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| text.len())
}
