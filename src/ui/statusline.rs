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

pub fn tabs(theme: &Theme, current: View) -> Paragraph<'static> {
    let mut spans = Vec::new();
    // `View::ALL`, not a second list — and numbered from the view's own id
    // rather than its position, because that id is what `3gt` and `:view 3`
    // mean. A positional number silently disagreed the moment a view was
    // added anywhere but the end.
    for view in View::ALL {
        let label = format!(" {}:{} ", *view as u8, view.title());
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
    // First, and unconditional while the mode is on. #33 calls a mode you
    // cannot see non-negotiable, and putting it behind the notice would mean
    // exactly that for four seconds after every message — which is precisely
    // when you are least sure what the keyboard is doing.
    //
    // Nothing is drawn in normal mode, as vim draws nothing for it.
    if ui.mode == crate::cmd::Mode::Transport {
        spans.push(Span::styled("-- TRANSPORT --  ", theme.get(Role::Mode)));
    }
    if !ui.status.is_empty() {
        spans.push(Span::styled(
            ui.status.clone(),
            theme.get(Role::StatusNotice),
        ));
    } else if let Some(id) = ui.playback.current
        && let Some(track) = {
            let files: &dyn crate::model::rows::Listing = ctx.library;
            files.listed(id)
        }
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
            ui.now_playing.render(&track.listed()),
            theme.get(Role::StatusText),
        ));
        let position = ctx.player.map(|p| p.position()).unwrap_or(Duration::ZERO);
        spans.push(Span::styled(
            format!(
                "  {} / {}",
                fmt_duration(position),
                fmt_duration(std::time::Duration::from_millis(track.duration_ms))
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

    // Keys typed so far, as vim's showcmd. Derived from state, so a completed
    // chord leaves nothing behind.
    if let Some(pending) = ui.pending_label() {
        spans.push(Span::styled(
            format!("  {pending}"),
            theme.get(Role::PromptSigil),
        ));
    }

    // **Before the underruns, because it explains the rest of the screen** —
    // #264. Everything a lost engine touches fails quietly and separately: rows
    // it has not cached draw as `<missing>`, `gp` has no target, and a rating
    // lands on whatever was playing when the connection dropped. Each reads as
    // its own bug. This is the one span that says they are all the same bug, and
    // that nothing on screen is current.
    if ctx.player.is_some_and(|p| p.engine_gone()) {
        spans.push(Span::styled(
            "  [engine gone — :q and restart]",
            theme.get(Role::StatusEngineGone),
        ));
    }

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
    // What the view is comes first: without it, "half my music is missing" is
    // a puzzle rather than a glance.
    if !ui.composed.is_empty() {
        out.push(format!("[{}]", ui.composed.label()));
    }
    // **The dimension, not the depth** — #234. `Inner` is "album" when grouped
    // by artist and album, and "year" when grouped by genre and year; a status
    // line saying `[inner]` would be honest and useless.
    out.push(format!(
        "[{}]",
        gmuse_library::next::scope_named(ui.playback.scope, ui.levels)
    ));
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
