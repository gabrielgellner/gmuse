//! The tab strip and the status line.
//!
//! The format-string language of DESIGN §7 replaces the fixed layout here in
//! M8; until then this is hand-built.

use std::time::Duration;

use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::theme::{Role, Theme};
use super::{Context, Prompt, Ui, View, list::fmt_duration};
use crate::audio::State;

const VIEWS: [View; 7] = [
    View::Tree,
    View::Sorted,
    View::Playlist,
    View::Queue,
    View::Browser,
    View::Filters,
    View::Settings,
];

pub fn tabs(theme: &Theme, current: View) -> Paragraph<'static> {
    let mut spans = Vec::new();
    for (i, view) in VIEWS.iter().enumerate() {
        let label = format!(" {}:{} ", i + 1, view.title());
        spans.push(Span::styled(
            label,
            theme.get(if *view == current {
                Role::TabActive
            } else {
                Role::TabInactive
            }),
        ));
    }
    Paragraph::new(Line::from(spans))
}

/// The `:` or `/` line being typed.
pub fn prompt_line<'a>(theme: &Theme, prompt: &'a Prompt) -> Paragraph<'a> {
    Paragraph::new(Line::from(vec![
        Span::styled(prompt.sigil().to_string(), theme.get(Role::PromptSigil)),
        Span::raw(prompt.buffer.as_str()),
    ]))
}

/// The now-playing line: what is playing, where in it we are, and the modes
/// that decide what plays next.
pub fn status<'a>(ui: &'a Ui, ctx: &Context<'_>) -> Paragraph<'a> {
    let mut spans = Vec::new();

    let theme = &ui.theme;
    if !ui.status.is_empty() {
        spans.push(Span::styled(
            ui.status.clone(),
            theme.get(Role::StatusNotice),
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
            theme.get(Role::StatusPlaying),
        ));
        spans.push(Span::styled(
            format!("{} — {}", track.file_under(), track.display_title()),
            theme.get(Role::StatusText),
        ));
        let position = ctx.player.map(|p| p.position()).unwrap_or(Duration::ZERO);
        spans.push(Span::styled(
            format!(
                "  {} / {}",
                fmt_duration(position),
                fmt_duration(track.duration)
            ),
            theme.get(Role::StatusDim),
        ));
    } else {
        spans.push(Span::styled(
            format!(
                "{} tracks  {} artists  {} albums",
                ui.library.len(),
                ui.library.artist_count(),
                ui.library.album_count()
            ),
            theme.get(Role::StatusText),
        ));
    }

    spans.push(Span::raw("   "));
    spans.push(Span::styled(modes(ui), theme.get(Role::StatusModes)));

    // Ring starvation is the difference between "the OS hiccuped" and "we did
    // not feed the device fast enough". It costs one span and it is the only
    // way to tell those apart after the fact.
    let underruns = ctx.player.map(|p| p.underruns()).unwrap_or(0);
    if underruns > 0 {
        spans.push(Span::styled(
            format!("  [underruns {underruns}]"),
            theme.get(Role::StatusUnderrun),
        ));
    }

    Paragraph::new(Line::from(spans))
}

/// Mode indicators, in the order cmus shows them. Only what is on appears, so
/// the common case stays quiet.
fn modes(ui: &Ui) -> String {
    let mut out = Vec::new();
    // Which catalogs are in force comes first: without it, "half my music is
    // missing" is a puzzle rather than a glance.
    if let Some(label) = ui.catalogs.label() {
        out.push(format!("[{label}]"));
    }
    out.push(format!("[{}]", ui.playback.aaa_mode.label()));
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
