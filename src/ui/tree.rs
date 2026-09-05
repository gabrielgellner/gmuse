//! View 1: the artist / album / track tree.
//!
//! DESIGN §8 calls this out as the one custom widget the project needs. It is
//! not a generic tree — it is exactly three levels, and flattening it to rows
//! each frame is both simpler and faster than maintaining a parallel structure
//! that has to be kept in step with the library.
//!
//! **One pane, all three levels.** Tracks used to live in a second pane
//! showing whatever the tree had selected, which meant exactly one album's
//! tracks at a time. Inline, several artists and several of their albums can
//! be open at once and the whole thing reads as one buffer: `j`, `3j`, `gg`,
//! `G` and the relative gutter all work over it uniformly, which is the point.

use crate::cmd::Motion;
use crate::model::rows::CopyId;
use crate::shelf::{Shelf, SortKey};
use crate::ui::fold::Expansion;

/// One visible line.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Artist {
        key: SortKey,
        display: String,
        albums: usize,
        expanded: bool,
    },
    Album {
        artist: SortKey,
        key: SortKey,
        display: String,
        tracks: usize,
        expanded: bool,
    },
    Track {
        artist: SortKey,
        album: SortKey,
        id: CopyId,
    },
}

impl Row {
    /// The marker and indent. A track's *text* comes from the format template,
    /// so it is rendered rather than labelled — a row beside an artist row
    /// needs to look different, and the template is how that is said.
    pub fn label(&self) -> String {
        match self {
            Row::Artist {
                display, expanded, ..
            } => format!("{} {display}", if *expanded { "▾" } else { "▸" }),
            Row::Album {
                display, expanded, ..
            } => format!("  {} {display}", if *expanded { "▾" } else { "▸" }),
            Row::Track { .. } => "      ".to_string(),
        }
    }

    /// Whether this row can be opened or closed at all.
    pub fn is_leaf(&self) -> bool {
        matches!(self, Row::Track { .. })
    }

    /// Whether this row is a container that is currently open. A leaf is
    /// neither open nor closed, and answers `false`.
    pub fn is_expanded(&self) -> bool {
        match self {
            Row::Artist { expanded, .. } | Row::Album { expanded, .. } => *expanded,
            Row::Track { .. } => false,
        }
    }
}

/// Which pane of view 1 has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    Tree,
    Tracks,
}

/// How deep the tree goes: artists at depth 0, albums at depth 1. `zR` sets
/// the level here, and there is nothing below an album but tracks, which are
/// leaves and have no fold of their own.
const DEPTH: u16 = 2;

#[derive(Debug, Default)]
pub struct TreeState {
    /// Keyed on the ident path — `[artist]` for an artist, `[artist, album]`
    /// for an album — so expansion survives a re-sort or a rescan.
    folds: Expansion<SortKey>,
    pub selected: usize,
}

impl TreeState {
    /// Flatten the library into visible rows, honouring what is expanded.
    ///
    /// Display names come from the first track under each node, since §4 keeps
    /// display strings on the `Track` and only sort keys in the tree.
    pub fn rows(&self, lib: &Shelf) -> Vec<Row> {
        let mut rows = Vec::new();
        for (artist_key, albums) in lib.artists() {
            let display = lib.artist_name(artist_key).to_string();
            let expanded = self.folds.is_open(std::slice::from_ref(artist_key));
            rows.push(Row::Artist {
                key: artist_key.clone(),
                display,
                albums: albums.len(),
                expanded,
            });
            if !expanded {
                continue;
            }
            for (album_key, tracks) in albums {
                let display = lib.album_name(artist_key, album_key).to_string();
                let open = self.folds.is_open(&[artist_key.clone(), album_key.clone()]);
                rows.push(Row::Album {
                    artist: artist_key.clone(),
                    key: album_key.clone(),
                    display,
                    tracks: tracks.len(),
                    expanded: open,
                });
                if !open {
                    continue;
                }
                for id in tracks {
                    rows.push(Row::Track {
                        artist: artist_key.clone(),
                        album: album_key.clone(),
                        id: *id,
                    });
                }
            }
        }
        rows
    }

    /// The path of a row that can be opened. A track has none — it is a leaf.
    fn path_of(row: &Row) -> Option<Vec<SortKey>> {
        match row {
            Row::Artist { key, .. } => Some(vec![key.clone()]),
            Row::Album { artist, key, .. } => Some(vec![artist.clone(), key.clone()]),
            Row::Track { .. } => None,
        }
    }

    /// Where a row *sits*, leaves included: a track's is its album's, because
    /// that is the row the cursor should fall back to when the track's fold
    /// closes under it.
    fn seat_of(row: &Row) -> Vec<SortKey> {
        match row {
            Row::Artist { key, .. } => vec![key.clone()],
            Row::Album { artist, key, .. } => vec![artist.clone(), key.clone()],
            Row::Track { artist, album, .. } => vec![artist.clone(), album.clone()],
        }
    }

    /// Every track under the selected row: one track, one album, or everything
    /// by an artist. What `a` and `e` add, and what `x` plays the first of.
    pub fn tracks_for(&self, lib: &Shelf) -> Vec<CopyId> {
        let rows = self.rows(lib);
        let Some(row) = rows.get(self.selected) else {
            return Vec::new();
        };
        match row {
            Row::Artist { key, .. } => lib
                .artists()
                .find(|(k, _)| *k == key)
                .map(|(_, albums)| albums.values().flatten().copied().collect())
                .unwrap_or_default(),
            Row::Album { artist, key, .. } => lib
                .artists()
                .find(|(k, _)| *k == artist)
                .and_then(|(_, albums)| albums.get(key))
                .cloned()
                .unwrap_or_default(),
            Row::Track { id, .. } => vec![*id],
        }
    }

    /// The track on the selected row, if it is one.
    pub fn selected_track(&self, lib: &Shelf) -> Option<CopyId> {
        match self.rows(lib).get(self.selected) {
            Some(Row::Track { id, .. }) => Some(*id),
            _ => None,
        }
    }

    /// Open or close the node under the cursor — `za`, and `o`.
    ///
    /// Deliberately **not** `close()` then `open()`: `close` walks up when the
    /// node is already shut, which is right for `h` and wrong here — toggling
    /// a closed album would close its artist instead of opening the album.
    pub fn toggle(&mut self, lib: &Shelf) {
        let Some(path) = self.path_under(lib) else {
            return;
        };
        self.folds.toggle(&path);
    }

    /// `zo` — open the node under the cursor, and only it.
    pub fn open(&mut self, lib: &Shelf) {
        if let Some(path) = self.path_under(lib) {
            self.folds.open(&path);
        }
    }

    /// `zO` — open the node under the cursor and everything beneath it.
    pub fn open_deep(&mut self, lib: &Shelf) {
        let Some(path) = self.path_under(lib) else {
            return;
        };
        let mut paths = vec![path.clone()];
        paths.extend(self.descendants(lib, &path));
        self.folds.open_all(paths);
    }

    /// `zC` — close the node under the cursor and everything beneath it, so
    /// that reopening it does not spill the subtree you had expanded inside.
    pub fn close_deep(&mut self, lib: &Shelf) {
        let Some(path) = self.path_under(lib) else {
            return;
        };
        let mut paths = vec![path.clone()];
        paths.extend(self.descendants(lib, &path));
        self.keeping_cursor(lib, |t| t.folds.close_all(paths));
    }

    /// `zA` — whichever of `zO` and `zC` this node is not already.
    ///
    /// Reads the node's own state rather than its subtree's, as vim does: the
    /// question `zA` answers is "is this one open", and what is underneath it
    /// follows rather than votes.
    pub fn toggle_deep(&mut self, lib: &Shelf) {
        let Some(path) = self.path_under(lib) else {
            return;
        };
        if self.folds.is_open(&path) {
            self.close_deep(lib);
        } else {
            self.open_deep(lib);
        }
    }

    /// `zR` — every fold open. Depth 2 is the whole tree: artists at 0,
    /// albums at 1.
    pub fn open_everything(&mut self, lib: &Shelf) {
        self.keeping_cursor(lib, |t| t.folds.set_level(DEPTH));
    }

    /// `zM` — every fold closed.
    pub fn close_everything(&mut self, lib: &Shelf) {
        self.keeping_cursor(lib, |t| t.folds.set_level(0));
    }

    /// `zr` — one level more. From nothing that is artists, then albums.
    pub fn more(&mut self, lib: &Shelf) {
        self.keeping_cursor(lib, |t| t.folds.more());
    }

    /// `zm` — one level fewer.
    pub fn less(&mut self, lib: &Shelf) {
        self.keeping_cursor(lib, |t| t.folds.less());
    }

    /// How deep a row sits: artists 0, albums 1, tracks 2.
    fn depth_of(row: &Row) -> usize {
        match row {
            Row::Artist { .. } => 0,
            Row::Album { .. } => 1,
            Row::Track { .. } => 2,
        }
    }

    /// Move by the shape of the tree rather than by counting rows — #31.
    ///
    /// Every one of these is "scan from the cursor for the first row matching a
    /// predicate", which is why they are one function: what differs is the
    /// predicate and the direction, and writing eight walks would have been
    /// eight chances to get the edges wrong.
    ///
    /// Stops at the ends rather than wrapping, as vim's `}` and `]]` do. A
    /// motion that finds nothing leaves the cursor alone, so a count that
    /// overshoots lands on the last match instead of nowhere.
    pub fn go(&mut self, lib: &Shelf, motion: Motion, count: usize) {
        for _ in 0..count.max(1) {
            let rows = self.rows(lib);
            let Some(at) = self.step(&rows, motion) else {
                return;
            };
            self.selected = at;
        }
    }

    fn step(&self, rows: &[Row], motion: Motion) -> Option<usize> {
        let here = rows.get(self.selected)?;
        let depth = Self::depth_of(here);

        let scan = |want: usize, forward: bool| -> Option<usize> {
            if forward {
                (self.selected + 1..rows.len()).find(|i| Self::depth_of(&rows[*i]) == want)
            } else {
                (0..self.selected)
                    .rev()
                    .find(|i| Self::depth_of(&rows[*i]) == want)
            }
        };

        match motion {
            Motion::NextSibling => scan(depth, true),
            Motion::PrevSibling => scan(depth, false),
            Motion::NextSection => scan(0, true),
            Motion::PrevSection => scan(0, false),
            // The nearest row above that is shallower — its container.
            Motion::Parent => (0..self.selected)
                .rev()
                .find(|i| Self::depth_of(&rows[*i]) < depth),
            // The group is this row if it is a container, otherwise the one
            // holding it — so `]z` on an album goes to its last track, and on
            // a track to the last track beside it.
            //
            // Asked as "is this row a container", not as "is its depth below
            // 2". The depths happen to agree today because the tree is exactly
            // artist/album/track, and would stop agreeing the moment it is not
            // — which is a shape this file otherwise makes no assumption about.
            Motion::GroupEnd => {
                let start = if rows[self.selected].is_leaf() {
                    (0..self.selected)
                        .rev()
                        .find(|i| Self::depth_of(&rows[*i]) < depth)?
                } else {
                    self.selected
                };
                Self::group_end(rows, start).filter(|e| *e != self.selected)
            }
            // `][` and `[]`. The end of the section the cursor is in, and once
            // it is there, the end of the next one — which is what makes a
            // repeated `][` walk section ends the way a repeated `]]` walks
            // section starts.
            Motion::NextSectionEnd => {
                let here = Self::section_start(rows, self.selected)?;
                match Self::group_end(rows, here) {
                    Some(end) if end > self.selected => Some(end),
                    _ => {
                        let next = scan(0, true)?;
                        Self::group_end(rows, next).or(Some(next))
                    }
                }
            }
            // The row before this section begins is the last row of the one
            // before it, so this needs no second scan. Landing on it moves the
            // cursor into that section, and the next press walks back again.
            Motion::PrevSectionEnd => match Self::section_start(rows, self.selected)? {
                0 => None,
                start => Some(start - 1),
            },
            Motion::NextFold => (self.selected + 1..rows.len()).find(|i| !rows[*i].is_leaf()),
            // vim's `zk` is the **end** of the previous fold, not its start —
            // `:h zk`. Landing on the start instead made `zj` and `zk` a pair
            // that did not undo each other, and left no key for the end at all.
            //
            // "A closed fold is counted as one fold", so a closed container is
            // its own end; an open one ends where the depth drops back.
            Motion::PrevFold => (0..self.selected)
                .rev()
                .find(|i| Self::is_fold_end(rows, *i)),
        }
    }

    /// The last row belonging to the group that starts at `start`, or `None`
    /// when it holds nothing.
    fn group_end(rows: &[Row], start: usize) -> Option<usize> {
        let floor = Self::depth_of(&rows[start]);
        rows.iter()
            .enumerate()
            .skip(start + 1)
            .take_while(|(_, row)| Self::depth_of(row) > floor)
            .map(|(i, _)| i)
            .last()
    }

    /// The depth-0 row at or above `at` — the section it belongs to.
    fn section_start(rows: &[Row], at: usize) -> Option<usize> {
        (0..=at).rev().find(|i| Self::depth_of(&rows[*i]) == 0)
    }

    /// Whether a fold ends on this row, in vim's sense: a closed fold counts
    /// as one fold and ends at itself, and an open one ends on the last row
    /// before the depth drops back.
    fn is_fold_end(rows: &[Row], i: usize) -> bool {
        if !rows[i].is_leaf() && !rows[i].is_expanded() {
            return true;
        }
        match rows.get(i + 1) {
            Some(next) => Self::depth_of(next) < Self::depth_of(&rows[i]),
            None => Self::depth_of(&rows[i]) > 0,
        }
    }

    /// Every fold that must be open for `row` to be on screen.
    ///
    /// An artist needs none — level 0 shows them. An album needs its artist. A
    /// track needs both, which is why this is a list and not a parent link.
    fn ancestors_of(row: &Row) -> Vec<Vec<SortKey>> {
        match row {
            Row::Artist { .. } => Vec::new(),
            Row::Album { artist, .. } => vec![vec![artist.clone()]],
            Row::Track { artist, album, .. } => {
                vec![vec![artist.clone()], vec![artist.clone(), album.clone()]]
            }
        }
    }

    /// `zx` — close every fold, then reopen just the ones that were holding
    /// the cursor's row, and put the cursor back on it.
    ///
    /// vim defines `zx` as "reapply foldlevel, then `zv`". `zv` on its own is
    /// not bound here because it could never do anything: the cursor always
    /// sits on a row that `rows()` produced, so it is visible by construction.
    /// It is only meaningful *after* something has closed underneath it, which
    /// is exactly this.
    pub fn only(&mut self, lib: &Shelf) {
        let rows = self.rows(lib);
        let Some(row) = rows.get(self.selected) else {
            return;
        };
        let opens = Self::ancestors_of(row);
        let want = Self::mark_of(row);

        self.folds.set_level(0);
        self.folds.open_all(opens);

        if let Some(at) = self.rows(lib).iter().position(|r| Self::mark_of(r) == want) {
            self.selected = at;
        }
    }

    /// Put the cursor on `id`, opening whatever was hiding it.
    ///
    /// Nothing else is disturbed — the point is to find the playing track
    /// without losing the shape you had built up looking for it.
    pub fn reveal(&mut self, lib: &Shelf, id: CopyId) -> bool {
        let Some((artist, album)) = lib.artists().find_map(|(artist, albums)| {
            albums
                .iter()
                .find(|(_, ids)| ids.contains(&id))
                .map(|(album, _)| (artist.clone(), album.clone()))
        }) else {
            return false;
        };
        self.folds
            .open_all(vec![vec![artist.clone()], vec![artist, album]]);
        match self
            .rows(lib)
            .iter()
            .position(|r| matches!(r, Row::Track { id: got, .. } if *got == id))
        {
            Some(at) => {
                self.selected = at;
                true
            }
            None => false,
        }
    }

    /// What identifies a row across a fold change: a track by its id, a
    /// container by its path. Not the `Row` itself — that carries whether it is
    /// expanded, which is precisely what a fold change alters.
    fn mark_of(row: &Row) -> (Vec<SortKey>, Option<CopyId>) {
        match row {
            Row::Track { id, .. } => (Self::seat_of(row), Some(*id)),
            _ => (Self::seat_of(row), None),
        }
    }

    /// The openable path under the cursor, if the cursor is on one.
    fn path_under(&self, lib: &Shelf) -> Option<Vec<SortKey>> {
        self.rows(lib).get(self.selected).and_then(Self::path_of)
    }

    /// Every openable path strictly under `path`. Only albums can be under
    /// anything, so this is one level deep by construction.
    fn descendants(&self, lib: &Shelf, path: &[SortKey]) -> Vec<Vec<SortKey>> {
        let [artist] = path else { return Vec::new() };
        lib.artists()
            .find(|(k, _)| *k == artist)
            .map(|(_, albums)| {
                albums
                    .keys()
                    .map(|album| vec![artist.clone(), album.clone()])
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Run a fold change and land the cursor somewhere that still exists.
    ///
    /// A level change can close the row out from under you. vim puts the
    /// cursor on the start of the fold that swallowed it, which here means the
    /// deepest visible ancestor of where you were.
    fn keeping_cursor(&mut self, lib: &Shelf, change: impl FnOnce(&mut Self)) {
        let seat = self.rows(lib).get(self.selected).map(Self::seat_of);
        change(self);
        let rows = self.rows(lib);
        let Some(seat) = seat else {
            self.selected = self.selected.min(rows.len().saturating_sub(1));
            return;
        };
        let mut best = 0;
        let mut deepest = 0;
        for (i, row) in rows.iter().enumerate() {
            let at = Self::seat_of(row);
            if at.len() <= seat.len() && seat.starts_with(&at) && at.len() >= deepest {
                deepest = at.len();
                best = i;
            }
        }
        self.selected = best.min(rows.len().saturating_sub(1));
    }

    /// Close the node under the cursor.
    ///
    /// If it is **already closed** — or is a track, which is never open — the
    /// *parent* closes instead and the cursor moves to it. That is snacks'
    /// rule, and it is what lets repeated `h` walk up and out without anything
    /// needing a concept of "level". `zc` is bound to it too, which is what
    /// vim's `zc` does on a line inside a fold.
    pub fn close(&mut self, lib: &Shelf) -> bool {
        let rows = self.rows(lib);
        let Some(row) = rows.get(self.selected) else {
            return false;
        };
        let path = Self::path_of(row).filter(|p| self.folds.is_open(p));
        let target = match path {
            Some(p) => p,
            // Closed, or a leaf: go up.
            None => match row {
                Row::Artist { .. } => return false,
                Row::Album { artist, .. } => vec![artist.clone()],
                Row::Track { artist, album, .. } => vec![artist.clone(), album.clone()],
            },
        };
        self.folds.close(&target);
        // The row would otherwise vanish from under the cursor.
        if let Some(pos) = self
            .rows(lib)
            .iter()
            .position(|r| Self::path_of(r).as_deref() == Some(target.as_slice()))
        {
            self.selected = pos;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Tagged as Track;
    use crate::scan::Holdings as Scan;
    use std::path::PathBuf;

    fn store_with(rows: &[(&str, &str, &str, &str)]) -> Scan {
        let mut s = Scan::new();
        for (path, artist, album, title) in rows {
            s.upsert(Track {
                path: PathBuf::from(path),
                artist: Some((*artist).into()),
                album: Some((*album).into()),
                title: Some((*title).into()),
                ..Default::default()
            });
        }
        s
    }

    fn fixture() -> (Scan, Shelf) {
        let store = store_with(&[
            ("/m/b1.mp3", "Burial", "Untrue", "Archangel"),
            ("/m/b2.mp3", "Burial", "Untrue", "Shell of Light"),
            ("/m/b3.mp3", "Burial", "Burial", "Night Bus"),
            ("/m/o1.mp3", "Mr. Oizo", "The Church", "Ham"),
        ]);
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&store), true);
        (store, lib)
    }

    #[test]
    fn collapsed_shows_only_artists() {
        let (_store, lib) = fixture();
        let state = TreeState::default();
        let rows = state.rows(&lib);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label(), "▸ Burial");
        assert_eq!(rows[1].label(), "▸ Mr. Oizo");
    }

    #[test]
    fn expanding_reveals_albums_indented() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib);

        let rows = state.rows(&lib);
        assert_eq!(
            rows.iter().map(Row::label).collect::<Vec<_>>(),
            ["▾ Burial", "  ▸ Burial", "  ▸ Untrue", "▸ Mr. Oizo"]
        );
    }

    /// The point of one pane: tracks sit inline under an open album, and
    /// several albums across several artists can be open at once.
    #[test]
    fn opening_an_album_puts_its_tracks_inline() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib); // the artist
        state.selected = 1;
        state.toggle(&lib); // its first album

        let rows = state.rows(&lib);
        assert!(
            matches!(rows.get(2), Some(Row::Track { .. })),
            "no track inline: {:?}",
            rows.iter().map(Row::label).collect::<Vec<_>>()
        );

        // A second artist can be open at the same time.
        state.selected = rows.len() - 1;
        state.toggle(&lib);
        let both = state.rows(&lib);
        assert!(both.len() > rows.len(), "the second artist did not open");
        assert!(
            both.iter().any(|r| matches!(r, Row::Track { .. })),
            "the first artist's tracks closed when the second opened"
        );
    }

    #[test]
    fn selecting_an_album_narrows_the_track_pane() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib); // expand Burial

        // Artist row selected: everything by that artist.
        state.selected = 0;
        assert_eq!(state.tracks_for(&lib).len(), 3);

        // "Burial" the album: one track.
        state.selected = 1;
        assert_eq!(state.tracks_for(&lib).len(), 1);

        // "Untrue": two.
        state.selected = 2;
        assert_eq!(state.tracks_for(&lib).len(), 2);
    }

    #[test]
    fn collapsing_from_an_album_row_moves_the_cursor_to_the_artist() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib);
        state.selected = 2; // an album row

        // `close` walks up when the row is already shut, which is how `h`
        // collapses outward without needing a concept of "level".
        state.close(&lib);

        // The album rows are gone, so the selection must not be left dangling
        // over what is now a different artist.
        assert_eq!(state.selected, 0);
        assert_eq!(state.rows(&lib).len(), 2);
    }

    /// Toggling an album must open it, not close its artist — `toggle` and
    /// `close` differ precisely here.
    #[test]
    fn toggling_a_closed_album_opens_it_rather_than_closing_the_artist() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib);
        let with_albums = state.rows(&lib).len();
        state.selected = 1;

        state.toggle(&lib);
        assert!(
            state.rows(&lib).len() > with_albums,
            "the artist collapsed instead of the album opening"
        );
    }

    #[test]
    fn zr_walks_down_a_level_at_a_time_and_zm_walks_back() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        assert_eq!(state.rows(&lib).len(), 2, "2 artists");

        state.more(&lib);
        assert_eq!(state.rows(&lib).len(), 5, "+ 3 albums");

        state.more(&lib);
        assert_eq!(state.rows(&lib).len(), 9, "+ 4 tracks");

        state.less(&lib);
        assert_eq!(state.rows(&lib).len(), 5);
        state.less(&lib);
        assert_eq!(state.rows(&lib).len(), 2);
    }

    #[test]
    fn zm_at_the_top_stays_there_rather_than_wrapping() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.less(&lib);
        state.less(&lib);
        state.more(&lib);
        assert_eq!(
            state.rows(&lib).len(),
            5,
            "one zr after bottoming out should open the albums, not undo a debt"
        );
    }

    #[test]
    fn zr_opens_everything_and_zm_closes_it() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib);
        assert_eq!(state.rows(&lib).len(), 9);
        state.close_everything(&lib);
        assert_eq!(state.rows(&lib).len(), 2);
    }

    #[test]
    fn a_level_change_discards_what_was_opened_by_hand() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib); // open Burial by hand
        state.close_everything(&lib);
        assert_eq!(
            state.rows(&lib).len(),
            2,
            "zM has to close what was opened by hand too, or it is not `all`"
        );
    }

    /// Deliberately on the *second* artist: landing on row 0 is what a cursor
    /// that does not ride up would also do, so testing the first artist proves
    /// nothing.
    #[test]
    fn collapsing_past_the_cursor_lands_it_on_the_row_that_swallowed_it() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib);
        let rows = state.rows(&lib);
        let oizo_track = rows
            .iter()
            .position(|r| matches!(r, Row::Track { artist, .. } if artist.as_str() == "mr. oizo"))
            .expect("Mr. Oizo's track should be on screen with every fold open");
        state.selected = oizo_track;

        state.close_everything(&lib);
        let rows = state.rows(&lib);
        assert_eq!(
            rows.len(),
            2,
            "everything but the artists should have closed"
        );
        assert!(
            matches!(&rows[state.selected], Row::Artist { display, .. } if display == "Mr. Oizo"),
            "the cursor should ride up to its own artist, not to the top: {:?}",
            rows.get(state.selected)
        );
    }

    #[test]
    fn za_closes_what_it_opened() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        let shut = state.rows(&lib).len();

        state.toggle(&lib);
        assert!(state.rows(&lib).len() > shut);
        state.toggle(&lib);
        assert_eq!(
            state.rows(&lib).len(),
            shut,
            "za has to go both ways — that is the whole of what it is"
        );
    }

    #[test]
    fn zo_opens_only_the_node_under_the_cursor() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open(&lib);
        assert_eq!(
            state.rows(&lib).len(),
            4,
            "Burial's 2 albums, Mr. Oizo still shut"
        );
    }

    #[test]
    fn zo_opens_the_node_and_zo_again_does_not_close_it() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open(&lib);
        let opened = state.rows(&lib).len();
        state.open(&lib);
        assert_eq!(
            state.rows(&lib).len(),
            opened,
            "zo is not a toggle — that is za"
        );
    }

    #[test]
    fn zo_deep_opens_the_albums_too() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_deep(&lib);
        assert_eq!(
            state.rows(&lib).len(),
            7,
            "Burial, its 2 albums and their 3 tracks, plus Mr. Oizo"
        );
    }

    #[test]
    fn zc_deep_does_not_spill_the_subtree_when_reopened() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_deep(&lib);
        state.close_deep(&lib);
        assert_eq!(state.rows(&lib).len(), 2);

        state.open(&lib);
        assert_eq!(
            state.rows(&lib).len(),
            4,
            "reopening Burial should show its albums closed, not the tracks it had open"
        );
    }

    /// Rows as `depth:label`, so a test can say what the tree looks like
    /// without reproducing the render.
    fn shape(state: &TreeState, lib: &Shelf) -> Vec<String> {
        state
            .rows(lib)
            .iter()
            .map(|r| format!("{}:{}", TreeState::depth_of(r), r.label().trim()))
            .collect()
    }

    #[test]
    fn next_sibling_steps_over_everything_nested() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib);

        // On the first artist, with both its albums and three tracks open.
        assert_eq!(state.selected, 0);
        state.go(&lib, Motion::NextSibling, 1);

        let rows = state.rows(&lib);
        assert!(
            matches!(&rows[state.selected], Row::Artist { display, .. } if display == "Mr. Oizo"),
            "`}}` should have skipped Burial's albums and tracks: {:?}",
            shape(&state, &lib)
        );
    }

    #[test]
    fn a_sibling_is_at_the_cursors_own_level() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib);
        state.selected = 1; // Burial's first album

        state.go(&lib, Motion::NextSibling, 1);
        let rows = state.rows(&lib);
        assert!(
            matches!(&rows[state.selected], Row::Album { .. }),
            "from an album `}}` should reach an album: {:?}",
            shape(&state, &lib)
        );
    }

    #[test]
    fn a_section_is_always_an_artist() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib);
        state.selected = 2; // a track, three levels down

        state.go(&lib, Motion::NextSection, 1);
        let rows = state.rows(&lib);
        assert!(
            matches!(&rows[state.selected], Row::Artist { .. }),
            "`]]` from a track should reach an artist: {:?}",
            shape(&state, &lib)
        );
    }

    #[test]
    fn parent_walks_out_one_level_at_a_time() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib);
        state.selected = 2; // a track

        state.go(&lib, Motion::Parent, 1);
        assert!(matches!(
            &state.rows(&lib)[state.selected],
            Row::Album { .. }
        ));

        state.go(&lib, Motion::Parent, 1);
        assert!(matches!(
            &state.rows(&lib)[state.selected],
            Row::Artist { .. }
        ));

        // And stops rather than wrapping to the bottom.
        let at = state.selected;
        state.go(&lib, Motion::Parent, 1);
        assert_eq!(state.selected, at, "`-` on an artist has nowhere to go");
    }

    #[test]
    fn group_end_finds_the_last_track_of_the_album() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib);

        // Untrue has two tracks; land on the first of them.
        let rows = shape(&state, &lib);
        let first = rows
            .iter()
            .position(|r| r.starts_with("2:"))
            .expect("a track is on screen");
        state.selected = first;

        state.go(&lib, Motion::GroupEnd, 1);
        let now = shape(&state, &lib);
        assert!(
            now[state.selected].starts_with("2:"),
            "still a track: {now:?}"
        );
        assert!(
            now.get(state.selected + 1)
                .is_none_or(|r| !r.starts_with("2:")),
            "it should be the last track of its album: {now:?}"
        );
    }

    #[test]
    fn group_end_on_an_album_reaches_its_own_last_track() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib);
        state.selected = 1; // an album row

        state.go(&lib, Motion::GroupEnd, 1);
        let now = shape(&state, &lib);
        assert!(
            now[state.selected].starts_with("2:"),
            "`]z` on an album should reach a track under it: {now:?}"
        );
    }

    /// `zj` goes to the next fold's **start**, `zk` to the previous fold's
    /// **end** — `:h zj`, `:h zk`. They are deliberately not inverses, and an
    /// earlier version of this test asserted that they were, which is how the
    /// wrong `zk` shipped: on the shallow fixture "the previous fold's start"
    /// and "where I came from" are the same row.
    #[test]
    fn zj_goes_to_a_start_and_zk_to_an_end() {
        let (_store, lib) = deep_fixture();
        let mut state = opened(&lib);

        // Row 6 is `four`, the first track of Anna's second album.
        state.selected = 6;
        state.go(&lib, Motion::PrevFold, 1);
        assert_eq!(
            state.selected, 4,
            "`zk` should land on `three`, the last track of the album before — \
             not on either album's row"
        );

        state.go(&lib, Motion::NextFold, 1);
        assert_eq!(state.selected, 5, "`zj` should land on `Second`, a start");
    }

    /// "A closed fold is counted as one fold" — `:h zk`. So with the tree
    /// collapsed every artist row is both a start and an end, and `zk` walks
    /// them even though no depth ever changes.
    #[test]
    fn a_closed_fold_is_one_fold_and_ends_at_itself() {
        let (_store, lib) = deep_fixture();
        // Cleo, with everything closed.
        let mut state = TreeState {
            selected: 2,
            ..Default::default()
        };

        state.go(&lib, Motion::PrevFold, 1);
        assert_eq!(
            state.selected, 1,
            "`zk` should step to Bruno, the previous closed fold"
        );
    }

    /// `][` leaves a section from the bottom, and repeating it walks section
    /// ends the way repeating `]]` walks section starts.
    #[test]
    fn the_section_end_motions_walk_the_other_edge() {
        let (_store, lib) = deep_fixture();
        let mut state = opened(&lib);
        state.selected = 2; // `one`, inside Anna

        state.go(&lib, Motion::NextSectionEnd, 1);
        assert_eq!(
            state.selected, 7,
            "`][` should reach `five`, the last row under Anna — past the \
             album boundary at 5, which `]z` would have stopped at"
        );

        state.go(&lib, Motion::NextSectionEnd, 1);
        assert_eq!(
            state.selected, 10,
            "a second `][` should reach `six`, the end of the next artist"
        );

        state.go(&lib, Motion::PrevSectionEnd, 1);
        assert_eq!(
            state.selected, 7,
            "`[]` should come back to the previous artist's last row"
        );
    }

    #[test]
    fn a_count_that_overshoots_lands_on_the_last_match() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        // Two artists, closed.
        state.go(&lib, Motion::NextSection, 99);
        assert_eq!(
            state.selected, 1,
            "it should stop at the last artist rather than giving up"
        );
    }

    #[test]
    fn a_motion_with_nowhere_to_go_stays_put() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.go(&lib, Motion::PrevSibling, 1);
        assert_eq!(state.selected, 0);
    }

    /// A shape with enough of everything to tell the motions apart: an artist
    /// with **two** albums, one of them with **three** tracks, and three
    /// artists in all.
    ///
    /// The small fixture cannot distinguish them — with one album per artist,
    /// "last track of the album" and "last track of the artist" are the same
    /// row, and with two artists a count of two and a count of one land in the
    /// same place.
    fn deep_fixture() -> (Scan, Shelf) {
        let store = store_with(&[
            ("/m/a/1.mp3", "Anna", "First", "one"),
            ("/m/a/2.mp3", "Anna", "First", "two"),
            ("/m/a/3.mp3", "Anna", "First", "three"),
            ("/m/a/4.mp3", "Anna", "Second", "four"),
            ("/m/a/5.mp3", "Anna", "Second", "five"),
            ("/m/b/1.mp3", "Bruno", "Third", "six"),
            ("/m/c/1.mp3", "Cleo", "Fourth", "seven"),
        ]);
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&store), true);
        (store, lib)
    }

    /// Rows 0..10, all open:
    ///
    ///     0 Anna  1 First  2 one  3 two  4 three  5 Second  6 four  7 five
    ///     8 Bruno  9 Third  10 six  11 Cleo  12 Fourth  13 seven
    fn opened(lib: &Shelf) -> TreeState {
        let mut state = TreeState::default();
        state.open_everything(lib);
        state
    }

    /// From a track that is **not** the first of its album, so that "nearest
    /// row above at a smaller depth" is not satisfied by the row immediately
    /// above.
    #[test]
    fn parent_is_a_container_not_the_row_above() {
        let (_store, lib) = deep_fixture();
        let mut state = opened(&lib);
        state.selected = 3; // the second track of "First"

        state.go(&lib, Motion::Parent, 1);
        assert_eq!(
            state.selected,
            1,
            "`-` should reach the album, not the track above it: {:?}",
            shape(&state, &lib)
        );
    }

    /// The group is the **album**, not everything the artist holds — an album
    /// followed by another album of the same artist is what tells them apart.
    #[test]
    fn group_end_stops_at_the_album_not_the_artist() {
        let (_store, lib) = deep_fixture();
        let mut state = opened(&lib);
        state.selected = 2; // the first track of "First"

        state.go(&lib, Motion::GroupEnd, 1);
        assert_eq!(
            state.selected,
            4,
            "`]z` should stop at the last track of \"First\", not run into \
             \"Second\": {:?}",
            shape(&state, &lib)
        );
    }

    #[test]
    fn group_end_on_an_album_uses_that_album() {
        let (_store, lib) = deep_fixture();
        let mut state = opened(&lib);
        state.selected = 1; // the album "First"

        state.go(&lib, Motion::GroupEnd, 1);
        assert_eq!(
            state.selected,
            4,
            "`]z` on an album is its own last track: {:?}",
            shape(&state, &lib)
        );
    }

    /// Three artists, so two steps and one step differ.
    #[test]
    fn a_count_repeats_the_motion() {
        let (_store, lib) = deep_fixture();
        let mut state = TreeState::default();

        state.go(&lib, Motion::NextSection, 2);
        assert_eq!(state.selected, 2, "`2]]` should reach the third artist");
    }

    /// A motion with nowhere to go leaves the cursor **where it is**, which is
    /// only observable when that is not already the top.
    #[test]
    fn a_motion_with_no_match_does_not_move_the_cursor() {
        let (_store, lib) = deep_fixture();
        let mut state = opened(&lib);
        let last = state.rows(&lib).len() - 1;
        state.selected = last;

        state.go(&lib, Motion::NextSibling, 1);
        assert_eq!(state.selected, last, "it jumped instead of staying put");
    }

    /// `zj` skips tracks, which only shows when a track sits between the
    /// cursor and the next openable row.
    #[test]
    fn zj_skips_over_tracks_to_the_next_container() {
        let (_store, lib) = deep_fixture();
        let mut state = opened(&lib);
        state.selected = 2; // a track, with two more tracks below it

        state.go(&lib, Motion::NextFold, 1);
        assert_eq!(
            state.selected,
            5,
            "`zj` should reach the album \"Second\": {:?}",
            shape(&state, &lib)
        );
    }
}
