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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::core::app_state::TiangongState;
use crate::core::runtime::RunStatus;
use crate::core::session::{Message, MessageRole};

const TICK_RATE: Duration = Duration::from_millis(60);
const MAX_COMMAND_HINTS: usize = 8;
const CONVERSATION_SCROLL_LINE_STEP: u16 = 3;
const CONVERSATION_SCROLL_PAGE_STEP: u16 = 16;

type CliTerminal = Terminal<CrosstermBackend<Stdout>>;

pub fn run_chat() -> Result<()> {
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
            draft_new_session: true,
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

    fn submit_input(&mut self) -> Result<()> {
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
        self.history_modal = Some(HistoryModalState {
            query: query.to_string(),
            selected_idx: 0,
        });
        self.status_message = "历史会话选择已打开（Esc 关闭）".to_string();
        Ok(())
    }

    fn confirm_history_modal_selection(&mut self) -> Result<()> {
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

    fn move_history_modal_selection(&mut self, step: i32) {
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

    fn move_history_modal_to_edge(&mut self, to_start: bool) {
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

    fn history_match_indices(&self, query: &str) -> Vec<usize> {
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

    fn move_hint_selection(&mut self, step: i8) {
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

    fn selected_hint_position(&self, hints: &[CommandHint]) -> Option<usize> {
        if hints.is_empty() {
            return None;
        }
        if self.selected_hint_idx < hints.len() && hints[self.selected_hint_idx].selectable {
            return Some(self.selected_hint_idx);
        }
        hints.iter().position(|hint| hint.selectable)
    }

    fn command_hints(&self) -> Vec<CommandHint> {
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

        let mut hints = vec![
            CommandHint::new("/model", "切换模型或查看可选模型"),
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

    fn is_command_palette_active(&self) -> bool {
        !self.command_hints().is_empty()
    }

    fn model_command_hints(&self, raw: &str) -> Vec<CommandHint> {
        let mut hints = vec![CommandHint::new("/model", "查看当前模型与模型列表")];
        let query = raw.trim_start_matches("/model").trim();
        let current = self.state.current_model();
        let models = self.state.model_list();

        if models.is_empty() {
            hints.push(CommandHint::new(
                format!("/model {current}"),
                "模型列表为空，仍可输入完整模型名切换",
            ));
            return hints;
        }

        let mut matched = 0usize;
        for model in models {
            if query.is_empty() || model.contains(query) {
                let desc = if model == current {
                    "当前模型"
                } else {
                    "切换到该模型"
                };
                hints.push(CommandHint::new(format!("/model {model}"), desc));
                matched += 1;
                if matched >= 6 {
                    break;
                }
            }
        }

        if matched == 0 {
            hints.push(CommandHint::new_note(
                "/model <name>",
                "未匹配到模型，输入完整名称后回车切换",
            ));
        }

        hints
    }

    fn render(&mut self, frame: &mut ratatui::Frame) {
        let area = frame.area();
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(8),
                Constraint::Length(3),
            ])
            .split(area);

        let title = self.active_session_title_for_view().to_string();
        let status_text = if self.state.has_pending_turn() {
            "请求中"
        } else {
            "空闲"
        };
        let header_lines = vec![
            Line::from(vec![
                Span::styled("天工 CLI", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!(
                    "  会话: {title}  模型: {}  状态: {status_text}",
                    self.state.current_model()
                )),
            ]),
            Line::from(Span::styled(
                self.status_message.as_str(),
                Style::default().fg(Color::Gray),
            )),
        ];
        let header = Paragraph::new(header_lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("状态"));
        frame.render_widget(header, sections[0]);

        let messages = self.active_messages_for_view();
        let transcript = format_transcript(&messages);
        let conversation_inner_width = sections[1].width.saturating_sub(2).max(1);
        let logical_line_count = Paragraph::new(transcript.clone())
            .wrap(Wrap { trim: false })
            .line_count(conversation_inner_width)
            .max(1);
        let visible_rows = sections[1].height.saturating_sub(2);
        self.max_conversation_scroll = logical_line_count
            .saturating_sub(visible_rows as usize)
            .min(u16::MAX as usize) as u16;
        if self.follow_conversation_bottom
            || self.conversation_scroll > self.max_conversation_scroll
        {
            self.conversation_scroll = self.max_conversation_scroll;
        }

        let conversation = Paragraph::new(transcript)
            .wrap(Wrap { trim: false })
            .scroll((self.conversation_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title("对话"));
        frame.render_widget(conversation, sections[1]);

        let input = Paragraph::new(self.input.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("输入（Enter 发送，输入 / 查看命令）"),
            );
        frame.render_widget(input, sections[2]);

        let max_input_width = sections[2].width.saturating_sub(3);
        let input_char_count = text_display_width(self.input.as_str());
        let cursor_x = sections[2]
            .x
            .saturating_add(1)
            .saturating_add(input_char_count.min(max_input_width));
        let cursor_y = sections[2].y.saturating_add(1);
        frame.set_cursor_position((cursor_x, cursor_y));
        if self.history_modal.is_none() {
            self.render_command_palette(frame, sections[2]);
        }
        self.render_history_modal(frame);
    }

    fn render_command_palette(&self, frame: &mut ratatui::Frame, input_area: Rect) {
        let hints = self.command_hints();
        if hints.is_empty() {
            return;
        }

        let row_count = hints.len().min(MAX_COMMAND_HINTS);
        let height = row_count as u16 + 2;
        let y = input_area.y.saturating_sub(height);
        let panel = Rect {
            x: input_area.x,
            y,
            width: input_area.width,
            height,
        };

        let lines = hints
            .iter()
            .take(row_count)
            .enumerate()
            .map(|(idx, hint)| {
                let selected_idx = self.selected_hint_position(&hints);
                let command_style = if selected_idx == Some(idx) && hint.selectable {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    let color = if hint.selectable {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    };
                    Style::default().fg(color)
                };
                let marker = if selected_idx == Some(idx) && hint.selectable {
                    "› "
                } else {
                    "  "
                };
                let desc_color = if hint.selectable {
                    Color::Gray
                } else {
                    Color::DarkGray
                };
                Line::from(vec![
                    Span::styled(format!("{marker}{:<16}", hint.command), command_style),
                    Span::styled(hint.description.as_str(), Style::default().fg(desc_color)),
                ])
            })
            .collect::<Vec<_>>();

        let widget = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("命令提示"));

        frame.render_widget(Clear, panel);
        frame.render_widget(widget, panel);
    }

    fn render_history_modal(&self, frame: &mut ratatui::Frame) {
        let Some(modal) = self.history_modal.as_ref() else {
            return;
        };

        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(54, 108);
        let height = area.height.saturating_sub(6).clamp(12, 22);
        let modal_rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };

        let matched = self.history_match_indices(&modal.query);
        let selected = if matched.is_empty() {
            0
        } else {
            modal.selected_idx.min(matched.len() - 1)
        };

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(2),
            ])
            .split(modal_rect);

        frame.render_widget(Clear, modal_rect);
        frame.render_widget(
            Block::default().borders(Borders::ALL).title("历史会话"),
            modal_rect,
        );

        let query_text = if modal.query.is_empty() {
            "(全部)".to_string()
        } else {
            modal.query.clone()
        };
        let query_widget = Paragraph::new(Line::from(vec![
            Span::styled("筛选: ", Style::default().fg(Color::Gray)),
            Span::styled(query_text, Style::default().fg(Color::Cyan)),
        ]))
        .block(Block::default().borders(Borders::ALL).title("关键词"));
        frame.render_widget(query_widget, sections[0]);

        let list_capacity = sections[1].height.saturating_sub(2) as usize;
        let list_start = selected.saturating_sub(list_capacity / 2);

        let lines = if matched.is_empty() {
            vec![Line::from(Span::styled(
                "没有匹配会话，继续输入关键词筛选",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            matched
                .iter()
                .skip(list_start)
                .take(list_capacity.max(1))
                .enumerate()
                .map(|(offset, session_idx)| {
                    let session = &self.state.sessions()[*session_idx];
                    let idx = *session_idx + 1;
                    let is_selected = list_start + offset == selected;
                    let is_current = session.id == self.state.active_session_id();
                    let marker = if is_selected { "› " } else { "  " };
                    let current_tag = if is_current { " · 当前" } else { "" };
                    let text = format!(
                        "{marker}{idx:>2}. {} · {}条消息{}",
                        session.title,
                        session.messages.len(),
                        current_tag
                    );
                    if is_selected {
                        Line::from(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        Line::from(Span::raw(text))
                    }
                })
                .collect()
        };

        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("会话列表")),
            sections[1],
        );

        let footer = Paragraph::new("↑/↓选择  Enter切换  Esc关闭")
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL).title("操作"));
        frame.render_widget(footer, sections[2]);
    }
}

impl CliApp {
    fn active_messages_for_view(&self) -> Vec<Message> {
        if self.draft_new_session {
            Vec::new()
        } else {
            self.state
                .active_session()
                .map(|session| session.messages.clone())
                .unwrap_or_default()
        }
    }

    fn active_session_title_for_view(&self) -> &str {
        if self.draft_new_session {
            "新对话"
        } else {
            self.state
                .active_session()
                .map(|session| session.title.as_str())
                .unwrap_or("默认会话")
        }
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

fn format_transcript(messages: &[Message]) -> Text<'static> {
    if messages.is_empty() {
        return Text::raw("开始新对话吧。");
    }

    let mut lines = Vec::new();
    for message in messages {
        let role = match message.role {
            MessageRole::User => "你",
            MessageRole::Assistant => "天工",
            MessageRole::System => "系统",
        };
        let content = if message.content.trim().is_empty() {
            "..."
        } else {
            message.content.trim_end()
        };
        let user_style = Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(47, 63, 86))
            .add_modifier(Modifier::BOLD);

        let mut parts = content.lines();
        let first = parts.next().unwrap_or_default();
        if message.role == MessageRole::User {
            lines.push(Line::from(Span::styled(
                format!(" {role}: {first} "),
                user_style,
            )));
            for part in parts {
                lines.push(Line::from(Span::styled(format!("   {part} "), user_style)));
            }
        } else {
            lines.push(Line::from(Span::raw(format!("{role}: {first}"))));
            for part in parts {
                lines.push(Line::from(Span::raw(format!("   {part}"))));
            }
        }
        lines.push(Line::default());
    }
    Text::from(lines)
}

fn text_display_width(text: &str) -> u16 {
    const TAB_STOP: usize = 4;
    let mut col = 0usize;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let to_next_tab = TAB_STOP - (col % TAB_STOP);
                col = col.saturating_add(to_next_tab);
            }
            '\r' | '\n' => {}
            _ => {
                let w = UnicodeWidthChar::width(ch).unwrap_or(0);
                col = col.saturating_add(w);
            }
        }
    }
    col.min(u16::MAX as usize) as u16
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
