//! Track-list rows and their rendering.
//!
//! A list is a sequence of *tracks* to everything outside this module, and a
//! sequence of *rows* only when it is drawn. Album dividers occupy a screen
//! line but no index, so cursors, counts and motions all address tracks and
//! never have to know headers exist (issues #6, #7).

use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use super::theme::{Role, Theme};
use crate::store::{Store, Track, TrackId};

/// One drawn line.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// An album divider. Never addressable: motions skip it and numbering
    /// excludes it.
    Header(String),
    Track(TrackId),
}

/// Expand a track list into drawn rows, inserting a divider whenever the album
/// changes.
///
/// A list that is all one album gets no header — the pane title or the tree
/// selection already says which album it is, and a lone divider at the top is
/// just a wasted line.
pub fn rows(store: &Store, ids: &[TrackId]) -> Vec<Row> {
    let (albums, artists) = distinct(store, ids);
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
        let Some(track) = store.get(*id) else {
            out.push(Row::Track(*id));
            continue;
        };
        let key = (
            track.file_under(),
            track.album_or_unknown(),
            track.discnumber,
        );
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
                track,
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
fn header_text(track: &Track, include_artist: bool) -> String {
    let mut text = if include_artist {
        format!("{} — {}", track.file_under(), track.album_or_unknown())
    } else {
        track.album_or_unknown().to_string()
    };
    if let Some(disc) = track.discnumber
        && disc > 1
    {
        text.push_str(&format!(" (disc {disc})"));
    }
    text
}

/// How many distinct albums and artists a list covers.
fn distinct(store: &Store, ids: &[TrackId]) -> (usize, usize) {
    let mut albums: Vec<(&str, &str)> = Vec::new();
    let mut artists: Vec<&str> = Vec::new();
    for id in ids {
        if let Some(t) = store.get(*id) {
            let album = (t.file_under(), t.album_or_unknown());
            if !albums.contains(&album) {
                albums.push(album);
            }
            if !artists.contains(&t.file_under()) {
                artists.push(t.file_under());
            }
        }
    }
    (albums.len(), artists.len())
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

/// Render rows as `NN  Title            m:ss`, with dividers styled apart.
pub fn items<'a>(theme: &Theme, store: &Store, rows: &[Row]) -> Vec<ListItem<'a>> {
    rows.iter()
        .map(|row| match row {
            Row::Header(text) => ListItem::new(Line::from(Span::styled(
                format!("── {text} "),
                theme.get(Role::AlbumHeader),
            ))),
            Row::Track(id) => {
                let Some(track) = store.get(*id) else {
                    return ListItem::new(Line::from("<missing>"));
                };
                let num = track
                    .tracknumber
                    .map(|n| format!("{n:>2}"))
                    .unwrap_or_else(|| "  ".into());
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{num}  ")),
                    Span::raw(track.display_title().to_string()),
                    Span::raw("  "),
                    Span::raw(fmt_duration(track.duration)),
                ]))
            }
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
    use std::path::PathBuf;
    use std::time::Duration;

    fn store_with(rows: &[(&str, &str, Option<u32>, &str)]) -> (Store, Vec<TrackId>) {
        let mut store = Store::new();
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
