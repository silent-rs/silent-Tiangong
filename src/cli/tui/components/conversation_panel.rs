use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::core::session::Message;

use super::super::CliApp;
use super::super::transcript::format_transcript;

impl CliApp {
    pub(in crate::cli::tui) fn render_conversation_panel(
        &mut self,
        frame: &mut ratatui::Frame,
        area: Rect,
    ) {
        let conversation_inner_width = area.width.saturating_sub(2).max(1);
        let messages = self.active_messages_for_view();
        let transcript = format_transcript(
            &messages,
            conversation_inner_width,
            self.state.has_pending_turn(),
        );
        let logical_line_count = Paragraph::new(transcript.clone())
            .wrap(Wrap { trim: false })
            .line_count(conversation_inner_width)
            .max(1);
        let visible_rows = area.height.saturating_sub(2);
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
        frame.render_widget(conversation, area);
    }

    pub(in crate::cli::tui) fn active_messages_for_view(&self) -> Vec<Message> {
        if self.draft_new_session {
            Vec::new()
        } else {
            self.state
                .active_session()
                .map(|session| session.messages.clone())
                .unwrap_or_default()
        }
    }

    pub(in crate::cli::tui) fn active_session_title_for_view(&self) -> &str {
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
