use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::super::{CliApp, CommandHint};

const MAX_COMMAND_HINTS: usize = 8;

impl CliApp {
    pub(in crate::cli::tui) fn render_model_selector(
        &self,
        frame: &mut ratatui::Frame,
        input_area: Rect,
    ) {
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

    pub(in crate::cli::tui) fn model_command_hints(&self, raw: &str) -> Vec<CommandHint> {
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
}
