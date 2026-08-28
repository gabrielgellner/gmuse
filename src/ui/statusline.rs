//! The tab strip and the status line.
//!
//! The format-string language of DESIGN §7 replaces the fixed layout here in
//! M8; until then this is hand-built.

use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{Context, Ui, View, list::fmt_duration};
use crate::audio::State;

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

/// The now-playing line: what is playing, where in it we are, and the modes
/// that decide what plays next.
pub fn status<'a>(ui: &'a Ui, ctx: &Context<'_>) -> Paragraph<'a> {
    let mut spans = Vec::new();

    if !ui.status.is_empty() {
        spans.push(Span::styled(
            ui.status.clone(),
            Style::default().fg(Color::Yellow),
        ));
    } else if let Some(id) = ui.playback.current
        && let Some(track) = ctx.store.get(id)
    {
        let state = ctx.player.map(|p| p.state()).unwrap_or(State::Stopped);
        spans.push(Span::styled(
            match state {
                State::Playing => "▶ ",
                State::Paused => "⏸ ",
                State::Stopped => "■ ",
            },
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(
            format!("{} — {}", track.file_under(), track.display_title()),
            Style::default().fg(Color::White),
        ));
        let position = ctx.player.map(|p| p.position()).unwrap_or(Duration::ZERO);
        spans.push(Span::styled(
            format!(
                "  {} / {}",
                fmt_duration(position),
                fmt_duration(track.duration)
            ),
            Style::default().fg(Color::Gray),
        ));
    } else {
        spans.push(Span::styled(
            format!(
                "{} tracks  {} artists  {} albums",
                ctx.library.len(),
                ctx.library.artist_count(),
                ctx.library.album_count()
            ),
            Style::default().fg(Color::Gray),
        ));
    }

    spans.push(Span::raw("   "));
    spans.push(Span::styled(modes(ui), Style::default().fg(Color::Magenta)));

    Paragraph::new(Line::from(spans))
}

/// Mode indicators, in the order cmus shows them. Only what is on appears, so
/// the common case stays quiet.
fn modes(ui: &Ui) -> String {
    let mut out = vec![format!("[{}]", ui.playback.aaa_mode.label())];
    if ui.playback.shuffle {
        out.push("[shuffle]".into());
    }
    if ui.playback.repeat {
        out.push("[repeat]".into());
    }
    if ui.playback.repeat_current {
        out.push("[repeat-1]".into());
    }
    if let Some(vol) = ui.volume_hint {
        out.push(format!("[vol {vol:.0}%]"));
    }
    out.join(" ")
}
