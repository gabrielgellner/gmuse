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
use crate::model::rows::CopyId;
use crate::model::rows::Listing;
use crate::query::format::NO_DURATION;

/// One drawn line.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// An album divider. Never addressable: motions skip it and numbering
    /// excludes it.
    Header(String),
    Track(CopyId),
}

/// The track indices a divider would be drawn above — where a new album
/// begins.
///
/// Derived from the same key [`rows`] uses, rather than recomputed, so that
/// `{` and `}` land exactly on the boundaries the eye can see. A list with no
/// dividers still has boundaries; `rows` only suppresses drawing them.
pub fn album_starts(lib: &dyn Listing, ids: &[CopyId]) -> Vec<usize> {
    boundaries(lib, ids, false)
}

/// The same for artists, which is what `[[` and `]]` step by.
pub fn artist_starts(lib: &dyn Listing, ids: &[CopyId]) -> Vec<usize> {
    boundaries(lib, ids, true)
}

fn boundaries(lib: &dyn Listing, ids: &[CopyId], artist_only: bool) -> Vec<usize> {
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
pub fn rows(lib: &dyn Listing, ids: &[CopyId]) -> Vec<Row> {
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
    pub playing: Option<CopyId>,
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

/// How a track row reads: playing, missing, or ordinary.
///
/// **One function, because there are two panes that draw track rows and they
/// had drifted.** The tree renders its own — a track row has to look different
/// from the artist row above it — and the greying #155 added went into
/// `items` alone, so view 1 was the one place a track you do not own looked
/// exactly like a track you do. That is the pane where a record is read album
/// by album, which is where the distinction is worth the most.
///
/// Playing outranks missing, because a row nothing fills cannot be the one
/// playing, and if it somehow is, saying so is the more urgent of the two.
pub fn body_style(theme: &Theme, listed: &crate::model::rows::Listed<'_>, playing: bool) -> Style {
    if playing {
        theme.get(Role::Playing)
    } else if !listed.held {
        theme.get(Role::Absent)
    } else {
        Style::default()
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
                let body = body_style(theme, &listed, style.playing == Some(*id));
                let mut spans = vec![
                    Span::styled(lead, theme.get(Role::Gutter)),
                    Span::styled(format!("{num}  "), body),
                ];
                spans.extend(highlighted(listed.title, body, style.needle, theme));
                spans.push(Span::styled("  ", body));
                // **A slot has no duration, and `0:00` claims one** (#155).
                // The library knows this track is on the record; it knows
                // nothing about how long it plays, because that is a property
                // of a file and there is no file. Printing a number reads as a
                // twelve-second rip rather than as an absence.
                spans.push(Span::styled(
                    if listed.held {
                        fmt_duration(listed.duration)
                    } else {
                        NO_DURATION.to_string()
                    },
                    body,
                ));
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
    use crate::fixture::Tagged as Track;
    use crate::scan::Holdings as Scan;
    use std::path::PathBuf;
    use std::time::Duration;

    fn store_with(
        rows: &[(&str, &str, Option<u32>, &str)],
    ) -> (crate::model::seam::Librarian, Vec<CopyId>) {
        let (l, rows, _) = store_and_scan(rows);
        (l, rows)
    }

    fn store_and_scan(
        rows: &[(&str, &str, Option<u32>, &str)],
    ) -> (crate::model::seam::Librarian, Vec<CopyId>, Scan) {
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
        // **The rows, not the copies** (#155): a fixture takes in files, and a
        // row is the appearance each one turned out to fill.
        let l = crate::fixture::from_holdings(&store);
        let rows = ids
            .into_iter()
            .filter_map(|copy| crate::fixture::row_of(&l, copy))
            .collect();
        (l, rows, store)
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

    /// **A track the library holds no file for has to look different** — #155.
    ///
    /// It did not, in the first build of this: the style was `DIM`, which is
    /// advisory and which plenty of terminals render identically to normal, so
    /// the only thing telling a missing track apart was a duration of `0:00` —
    /// and that is a number the library does not have. Two signals now, and
    /// neither is a modifier a terminal may ignore.
    #[test]
    fn a_track_nothing_fills_is_greyed_and_shows_no_duration() {
        use crate::facts::{Fact, Record};
        use crate::model::rows::Listing as _;

        let (l, ids, store) = store_and_scan(&[("A", "One", None, "t1")]);
        let theme = Theme::default();

        // A second track on the same disc, which the library knows about and
        // holds no file for.
        let cat_medium = *l.spine.media.keys().next().expect("the album has a disc");
        let unheld =
            crate::model::id::Id::<crate::model::spine::Recording>::derived(b"a track we lack");
        let mut clock = crate::model::clock::HybridClock::new(crate::model::id::Id::derived(b"t"));
        let records: std::collections::BTreeSet<Record> = [
            Fact::RecordingCatalogued {
                recording: unheld,
                title: "t2".into(),
                artist: crate::model::id::Id::of_artist(None, "A"),
            },
            Fact::TrackPlaced {
                track: crate::model::id::Id::derived(b"slot two"),
                medium: cat_medium,
                recording: unheld,
                position: 2,
                title_on_release: None,
            },
        ]
        .into_iter()
        .map(|fact| Record {
            stamp: clock.stamp(200),
            fact,
        })
        .collect();
        // Folded twice on purpose: the medium's id is not knowable until the
        // file has been catalogued, and the slot has to name it.
        let l = crate::fixture::from_holdings_and(&store, &records);

        let gap = l
            .rows()
            .into_iter()
            .find(|r| l.listed(*r).is_some_and(|x| x.title == "t2"))
            .expect("the catalogued row");
        let held = ids[0];

        let drawn = items(
            &theme,
            &l,
            &[Row::Track(held), Row::Track(gap)],
            RowStyle::default(),
        );
        let text = |item: &ListItem<'_>| format!("{item:?}");

        assert!(
            text(&drawn[1]).contains(NO_DURATION),
            "a slot has no duration, so it must not print one: {}",
            text(&drawn[1])
        );
        assert!(
            !text(&drawn[0]).contains(NO_DURATION),
            "and a row with a file still prints its length"
        );
        assert_ne!(
            theme.get(Role::Absent),
            Style::default(),
            "an unstyled `Absent` is a row that looks held"
        );
        // Asserted on the *title* span, because the gutter is grey on every
        // row and matching the whole line would pass on that alone.
        assert!(
            text(&drawn[1]).contains(r#"Span::from("t2").dark_gray()"#),
            "the missing row is painted, not merely modified: {}",
            text(&drawn[1])
        );
        assert!(
            !text(&drawn[0]).contains(r#"Span::from("t1").dark_gray()"#),
            "and a row with a file is left alone: {}",
            text(&drawn[0])
        );
    }

    /// **Both panes ask the same question and must get the same answer** —
    /// #155, and the reason `body_style` exists.
    ///
    /// The tree renders its own track rows, because a track has to look
    /// different from the artist row above it. So the greying went into
    /// `items` and view 1 — the pane where a record is read album by album,
    /// and where a missing track matters most — was the one place it looked
    /// exactly like a track you own.
    #[test]
    fn a_missing_row_reads_the_same_in_every_pane() {
        let (l, ids, _) = store_and_scan(&[("A", "One", None, "t1")]);
        let theme = Theme::default();
        let held = ids[0];
        let listed = {
            use crate::model::rows::Listing as _;
            l.listed(held).expect("the fixture row")
        };

        assert_eq!(
            body_style(&theme, &listed, false),
            Style::default(),
            "a row with a file is left alone"
        );
        assert_eq!(
            body_style(&theme, &listed, true),
            theme.get(Role::Playing),
            "and playing still outranks everything"
        );

        let mut missing = listed;
        missing.held = false;
        assert_eq!(
            body_style(&theme, &missing, false),
            theme.get(Role::Absent),
            "a row with no file is painted"
        );
        assert_eq!(
            body_style(&theme, &missing, true),
            theme.get(Role::Playing),
            "a row nothing fills cannot be playing, and if it is, say that"
        );
        assert_ne!(
            theme.get(Role::Absent),
            Style::default(),
            "an unstyled `Absent` is a row that looks held"
        );
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
