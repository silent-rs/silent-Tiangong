use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::core::app_state::TiangongState;
use crate::core::runtime::RunStatus;

mod commands;
mod components;
mod render;
mod transcript;

const TICK_RATE: Duration = Duration::from_millis(60);
const CONVERSATION_SCROLL_LINE_STEP: u16 = 3;
const CONVERSATION_SCROLL_PAGE_STEP: u16 = 16;

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
    status_message: String,
    selected_hint_idx: usize,
    history_modal: Option<HistoryModalState>,
    draft_new_session: bool,
    conversation_scroll: u16,
    max_conversation_scroll: u16,
    follow_conversation_bottom: bool,
    was_pending_turn: bool,
    should_quit: bool,
}

impl CliApp {
    fn new() -> Self {
        Self {
            state: TiangongState::load_or_default(),
            input: String::new(),
            status_message: "输入 / 查看命令提示".to_string(),
            selected_hint_idx: 0,
            history_modal: None,
            draft_new_session: false,
            conversation_scroll: 0,
            max_conversation_scroll: 0,
            follow_conversation_bottom: true,
            was_pending_turn: false,
            should_quit: false,
        }
    }

    fn run(&mut self, terminal: &mut CliTerminal) -> Result<()> {
        while !self.should_quit {
            self.state.poll_pending_turn();
            self.sync_status_after_poll();
            terminal.draw(|frame| self.render(frame))?;

            if event::poll(TICK_RATE)?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.handle_key(key)?;
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

        match key.code {
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Up => {
                if self.is_command_palette_active() {
                    self.move_hint_selection(-1);
                } else {
                    self.scroll_conversation_up(CONVERSATION_SCROLL_LINE_STEP);
                }
            }
            KeyCode::Down => {
                if self.is_command_palette_active() {
                    self.move_hint_selection(1);
                } else {
                    self.scroll_conversation_down(CONVERSATION_SCROLL_LINE_STEP);
                }
            }
            KeyCode::PageUp => self.scroll_conversation_up(CONVERSATION_SCROLL_PAGE_STEP),
            KeyCode::PageDown => self.scroll_conversation_down(CONVERSATION_SCROLL_PAGE_STEP),
            KeyCode::Home => self.scroll_conversation_to_top(),
            KeyCode::End => self.scroll_conversation_to_bottom(),
            KeyCode::Tab => self.move_hint_selection(1),
            KeyCode::BackTab => self.move_hint_selection(-1),
            KeyCode::Enter => self.submit_input()?,
            KeyCode::Backspace => {
                self.input.pop();
                self.selected_hint_idx = 0;
            }
            KeyCode::Char(ch) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    self.input.push(ch);
                    self.selected_hint_idx = 0;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_history_modal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.history_modal = None;
                self.status_message = "已关闭历史会话选择".to_string();
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
}

#[derive(Debug, Clone, Default)]
struct HistoryModalState {
    query: String,
    selected_idx: usize,
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
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(mut terminal: CliTerminal) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
