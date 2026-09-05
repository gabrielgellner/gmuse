//! Track-list rows and their rendering.
//!
//! A list is a sequence of *tracks* to everything outside this module, and a
//! sequence of *rows* only when it is drawn. Album dividers occupy a screen
//! line but no index, so cursors, counts and motions all address tracks and
//! never have to know headers exist (issues #6, #7).

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use super::theme::{Role, Theme};
use crate::model::rows::Listing;
use crate::store::TrackId;

/// One drawn line.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// An album divider. Never addressable: motions skip it and numbering
    /// excludes it.
    Header(String),
    Track(TrackId),
}

/// The track indices a divider would be drawn above — where a new album
/// begins.
///
/// Derived from the same key [`rows`] uses, rather than recomputed, so that
/// `{` and `}` land exactly on the boundaries the eye can see. A list with no
/// dividers still has boundaries; `rows` only suppresses drawing them.
pub fn album_starts(lib: &dyn Listing, ids: &[TrackId]) -> Vec<usize> {
    boundaries(lib, ids, false)
}

/// The same for artists, which is what `[[` and `]]` step by.
pub fn artist_starts(lib: &dyn Listing, ids: &[TrackId]) -> Vec<usize> {
    boundaries(lib, ids, true)
}

fn boundaries(lib: &dyn Listing, ids: &[TrackId], artist_only: bool) -> Vec<usize> {
    let mut out = Vec::new();
    let mut last: Option<(&str, &str, Option<u32>)> = None;
    for (at, id) in ids.iter().enumerate() {
        let Some(listed) = lib.listed(*id) else {
            continue;
        };
        let key = (listed.file_under, listed.album, listed.disc);
        let changed = match last {
            Some((artist, _, _)) if artist_only => artist != key.0,
            Some((artist, album, disc)) => {
                artist != key.0 || album != key.1 || disc.unwrap_or(1) != key.2.unwrap_or(1)
            }
            None => true,
        };
        if changed {
            out.push(at);
        }
        last = Some(key);
    }
    out
}

/// Expand a track list into drawn rows, inserting a divider whenever the album
/// changes.
///
/// A list that is all one album gets no header — the pane title or the tree
/// selection already says which album it is, and a lone divider at the top is
/// just a wasted line.
pub fn rows(lib: &dyn Listing, ids: &[TrackId]) -> Vec<Row> {
    let (albums, artists) = lib.spread(ids);
    if albums < 2 {
        return ids.iter().copied().map(Row::Track).collect();
    }
    // Naming the artist only earns its place when the list spans more than
    // one: in a tree's track pane the artist is already established by the
    // selection, and repeating it on every divider is noise.
    let multi_artist = artists > 1;

    let mut out = Vec::with_capacity(ids.len() + albums);
    let mut last: Option<(&str, &str, Option<u32>)> = None;

    for id in ids {
        let Some(listed) = lib.listed(*id) else {
            out.push(Row::Track(*id));
            continue;
        };
        let key = (listed.file_under, listed.album, listed.disc);
        let changed = match last {
            // A new disc of the same album is also a boundary worth seeing —
            // it is exactly where a multi-disc album wants dividing.
            Some((artist, album, disc)) => {
                artist != key.0 || album != key.1 || disc.unwrap_or(1) != key.2.unwrap_or(1)
            }
            None => true,
        };
        if changed {
            let artist_changed = last.map(|(a, _, _)| a != key.0).unwrap_or(true);
            out.push(Row::Header(header_text(
                listed,
                multi_artist && artist_changed,
            )));
        }
        out.push(Row::Track(*id));
        last = Some(key);
    }
    out
}

/// The artist is only worth repeating when it changes — in a tree's track pane
/// it is fixed, and printing it on every divider is noise.
fn header_text(listed: crate::model::rows::Listed<'_>, include_artist: bool) -> String {
    let mut text = if include_artist {
        format!("{} — {}", listed.file_under, listed.album)
    } else {
        listed.album.to_string()
    };
    if let Some(disc) = listed.disc
        && disc > 1
    {
        text.push_str(&format!(" (disc {disc})"));
    }
    text
}

/// Which screen row holds the `n`th track.
///
/// The bridge between the two coordinate systems: callers count tracks, the
/// widget wants a row.
pub fn row_of_track(rows: &[Row], track_index: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, Row::Track(_)))
        .nth(track_index)
        .map(|(i, _)| i)
}

/// How to draw a list's rows.
#[derive(Debug, Clone, Copy, Default)]
pub struct RowStyle<'a> {
    /// Show the relative-distance gutter.
    pub relative: bool,
    /// Which **track** the cursor is on. Distances are measured in tracks, not
    /// screen rows, so a divider between two tracks never makes `3j` wrong.
    pub selected: usize,
    /// The track currently playing, marked wherever it appears — which may be
    /// several lists at once, since a track can be in the library, a playlist
    /// and the queue.
    pub playing: Option<TrackId>,
    /// The search pattern in force, so a row can paint what it matched.
    /// `None` once `<Esc>` has cleared it, as vim's `nohlsearch` does.
    pub needle: Option<&'a crate::text::Needle>,
}

/// The distance gutter for one row.
///
/// Vim's hybrid: absolute on the cursor line, relative elsewhere. The absolute
/// number is what a count for `G` takes, so the line you are on tells you the
/// number to type to come back to it.
///
/// Shared with the tree pane, which numbers artists and albums rather than
/// tracks — the arithmetic is the same and having two copies would let them
/// drift apart in width, which is what makes a gutter unreadable.
pub fn gutter(n: usize, selected: usize) -> String {
    if n == selected {
        format!("{:<4}", n + 1)
    } else {
        format!("{:>3} ", n.abs_diff(selected))
    }
}

/// Blank space the same width, so a row with no number still lines up.
pub const GUTTER_BLANK: &str = "    ";

/// Render rows as `NN  Title            m:ss`, with dividers styled apart.
pub fn items<'a>(
    theme: &Theme,
    lib: &dyn Listing,
    rows: &[Row],
    style: RowStyle,
) -> Vec<ListItem<'a>> {
    let mut track_index = 0usize;
    rows.iter()
        .map(|row| {
            // The gutter counts tracks; a divider gets blank space so the
            // column still lines up.
            let lead = if !style.relative {
                String::new()
            } else if matches!(row, Row::Header(_)) {
                GUTTER_BLANK.to_string()
            } else {
                let n = track_index;
                track_index += 1;
                gutter(n, style.selected)
            };
            (lead, row)
        })
        .map(|(lead, row)| match row {
            Row::Header(text) => ListItem::new(Line::from(vec![
                Span::raw(lead),
                Span::styled(format!("── {text} "), theme.get(Role::AlbumHeader)),
            ])),
            Row::Track(id) => {
                let Some(listed) = lib.listed(*id) else {
                    return ListItem::new(Line::from("<missing>"));
                };
                let num = listed
                    .track
                    .map(|n| format!("{n:>2}"))
                    .unwrap_or_else(|| "  ".into());
                // The playing track is marked in every list it appears in, so
                // you can see where you are without consulting the status bar.
                let body = if style.playing == Some(*id) {
                    theme.get(Role::Playing)
                } else {
                    Style::default()
                };
                let mut spans = vec![
                    Span::styled(lead, theme.get(Role::Gutter)),
                    Span::styled(format!("{num}  "), body),
                ];
                spans.extend(highlighted(listed.title, body, style.needle, theme));
                spans.push(Span::styled("  ", body));
                spans.push(Span::styled(fmt_duration(listed.duration), body));
                ListItem::new(Line::from(spans))
            }
        })
        .collect()
}

/// Split `text` into spans, painting the part a search matched.
///
/// One helper for every list, so the tree and the flat lists highlight
/// identically. It paints only what is **on screen**: the search compares
/// against artist, album and title together, so a match on the artist leaves a
/// track row unpainted — the row is still where the cursor went, and claiming
/// a highlight that is not there would be worse.
///
/// nvim's second role, `CurSearch`, is deliberately **not** copied: it exists
/// there because the cursor is a thin bar, and here the cursor row is already
/// painted end to end by `SelectionFocused` — which ratatui's `highlight_style`
/// applies over any span style anyway, so it could not be seen even if it were
/// wanted.
pub fn highlighted(
    text: &str,
    base: Style,
    needle: Option<&crate::text::Needle>,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let Some(range) = needle.and_then(|n| crate::text::match_range(text, n)) else {
        return vec![Span::styled(text.to_string(), base)];
    };
    let take =
        |from: usize, to: usize| -> String { text.chars().skip(from).take(to - from).collect() };
    let hit = theme.get(Role::SearchMatch);
    let mut spans = Vec::with_capacity(3);
    if range.start > 0 {
        spans.push(Span::styled(take(0, range.start), base));
    }
    spans.push(Span::styled(take(range.start, range.end), hit));
    let len = text.chars().count();
    if range.end < len {
        spans.push(Span::styled(take(range.end, len), base));
    }
    spans
}

pub fn fmt_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Scan, Track};
    use std::path::PathBuf;
    use std::time::Duration;

    fn store_with(rows: &[(&str, &str, Option<u32>, &str)]) -> (Scan, Vec<TrackId>) {
        let mut store = Scan::new();
        let mut ids = Vec::new();
        for (i, (artist, album, disc, title)) in rows.iter().enumerate() {
            ids.push(store.upsert(Track {
                path: PathBuf::from(format!("/m/{i}.mp3")),
                artist: Some((*artist).into()),
                album: Some((*album).into()),
                title: Some((*title).into()),
                discnumber: *disc,
                tracknumber: Some(i as u32 + 1),
                duration: Duration::from_secs(100),
                ..Default::default()
            }));
        }
        (store, ids)
    }

    fn headers(rows: &[Row]) -> Vec<&str> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Header(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_single_album_gets_no_divider() {
        let (store, ids) = store_with(&[("A", "One", None, "t1"), ("A", "One", None, "t2")]);
        let rows = rows(&store, &ids);
        assert!(headers(&rows).is_empty(), "a lone header is a wasted line");
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn albums_are_divided() {
        let (store, ids) = store_with(&[
            ("A", "One", None, "t1"),
            ("A", "One", None, "t2"),
            ("A", "Two", None, "t3"),
        ]);
        let rows = rows(&store, &ids);
        assert_eq!(headers(&rows), ["One", "Two"]);
        // Two headers plus three tracks.
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn the_artist_appears_only_when_it_changes() {
        let (store, ids) = store_with(&[
            ("A", "One", None, "t1"),
            ("B", "Two", None, "t2"),
            ("B", "Three", None, "t3"),
        ]);
        // In a tree track pane the artist is fixed, so repeating it every time
        // would be noise; across artists it is the useful half of the label.
        let rows = rows(&store, &ids);
        assert_eq!(headers(&rows), ["A — One", "B — Two", "Three"]);
    }

    #[test]
    fn a_new_disc_divides_within_an_album() {
        let (store, ids) = store_with(&[
            ("A", "One", Some(1), "t1"),
            ("A", "One", Some(2), "t2"),
            ("A", "Two", None, "t3"),
        ]);
        let rows = rows(&store, &ids);
        assert_eq!(headers(&rows), ["One", "One (disc 2)", "Two"]);
    }

    #[test]
    fn a_missing_disc_number_counts_as_disc_one() {
        let (store, ids) = store_with(&[
            ("A", "One", None, "t1"),
            ("A", "One", Some(1), "t2"),
            ("A", "Two", None, "t3"),
        ]);
        // Otherwise a partially tagged album would divide itself in half.
        let rows = rows(&store, &ids);
        assert_eq!(headers(&rows), ["One", "Two"]);
    }

    /// The bridge between the two coordinate systems. Everything outside the
    /// renderer counts tracks; only this maps to a screen row.
    #[test]
    fn row_of_track_skips_headers() {
        let (store, ids) = store_with(&[
            ("A", "One", None, "t1"),
            ("A", "Two", None, "t2"),
            ("A", "Two", None, "t3"),
        ]);
        let rows = rows(&store, &ids);
        // rows: [Header, t1, Header, t2, t3]
        assert_eq!(row_of_track(&rows, 0), Some(1));
        assert_eq!(row_of_track(&rows, 1), Some(3));
        assert_eq!(row_of_track(&rows, 2), Some(4));
        assert_eq!(row_of_track(&rows, 3), None, "past the end");
    }

    #[test]
    fn row_of_track_is_the_identity_without_headers() {
        let (store, ids) = store_with(&[("A", "One", None, "t1"), ("A", "One", None, "t2")]);
        let rows = rows(&store, &ids);
        for i in 0..2 {
            assert_eq!(row_of_track(&rows, i), Some(i));
        }
    }

    #[test]
    fn an_empty_list_has_no_rows() {
        let (store, _) = store_with(&[]);
        assert!(rows(&store, &[]).is_empty());
    }

    #[test]
    fn durations_render_as_minutes_and_seconds() {
        assert_eq!(fmt_duration(Duration::from_secs(0)), "0:00");
        assert_eq!(fmt_duration(Duration::from_secs(9)), "0:09");
        assert_eq!(fmt_duration(Duration::from_secs(61)), "1:01");
        assert_eq!(fmt_duration(Duration::from_secs(3599)), "59:59");
        assert_eq!(fmt_duration(Duration::from_secs(3600)), "60:00");
    }
}
