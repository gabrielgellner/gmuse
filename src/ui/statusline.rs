//! The tab strip and the status line.
//!
//! The format-string language of DESIGN §7 replaces this in M8; until then the
//! layout is fixed.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{Context, Ui, View};

const VIEWS: [View; 5] = [
    View::Tree,
    View::Sorted,
    View::Playlist,
    View::Queue,
    View::Browser,
];

pub fn tabs(current: View) -> Paragraph<'static> {
    let mut spans = Vec::new();
    for (i, view) in VIEWS.iter().enumerate() {
        let label = format!(" {}:{} ", i + 1, view.title());
        spans.push(if *view == current {
            Span::styled(
                label,
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(label, Style::default().fg(Color::DarkGray))
        });
    }
    Paragraph::new(Line::from(spans))
}

pub fn status<'a>(ui: &'a Ui, ctx: &Context<'_>) -> Paragraph<'a> {
    let left = if ui.status.is_empty() {
        format!(
            "{} tracks  {} artists  {} albums",
            ctx.library.len(),
            ctx.library.artist_count(),
            ctx.library.album_count()
        )
    } else {
        ui.status.clone()
    };
    Paragraph::new(Line::from(vec![
        Span::styled(left, Style::default().fg(Color::Gray)),
        Span::styled(
            "   1-5 views  j/k move  space expand  tab pane  q quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]))
}
