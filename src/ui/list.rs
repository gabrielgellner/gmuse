//! Shared track-list rendering.

use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use crate::store::{Store, TrackId};

/// Render tracks as `NN  Title            m:ss`.
///
/// The real format-string language is M8; this is a fixed layout that is
/// readable in the meantime.
pub fn track_items<'a>(store: &Store, ids: &[TrackId]) -> Vec<ListItem<'a>> {
    ids.iter()
        .map(|id| {
            let Some(track) = store.get(*id) else {
                return ListItem::new(Line::from("<missing>"));
            };
            let num = track
                .tracknumber
                .map(|n| format!("{n:>2}"))
                .unwrap_or_else(|| "  ".into());
            let dur = fmt_duration(track.duration);
            ListItem::new(Line::from(vec![
                Span::raw(format!("{num}  ")),
                Span::raw(track.display_title().to_string()),
                Span::raw("  "),
                Span::raw(dur),
            ]))
        })
        .collect()
}

pub fn fmt_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn durations_render_as_minutes_and_seconds() {
        assert_eq!(fmt_duration(Duration::from_secs(0)), "0:00");
        assert_eq!(fmt_duration(Duration::from_secs(9)), "0:09");
        assert_eq!(fmt_duration(Duration::from_secs(61)), "1:01");
        assert_eq!(fmt_duration(Duration::from_secs(3599)), "59:59");
        // Over an hour keeps counting in minutes, like cmus.
        assert_eq!(fmt_duration(Duration::from_secs(3600)), "60:00");
    }
}
