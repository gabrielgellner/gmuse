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

use crate::library::{Library, SortKey};
use crate::store::{Store, TrackId};
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
        id: TrackId,
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
    pub fn rows(&self, lib: &Library, store: &Store) -> Vec<Row> {
        let mut rows = Vec::new();
        for (artist_key, albums) in lib.artists() {
            let display = albums
                .values()
                .flatten()
                .next()
                .and_then(|id| store.get(*id))
                .map(|t| t.file_under().to_string())
                .unwrap_or_else(|| artist_key.as_str().to_string());
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
                let display = tracks
                    .first()
                    .and_then(|id| store.get(*id))
                    .map(|t| t.album_or_unknown().to_string())
                    .unwrap_or_else(|| album_key.as_str().to_string());
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
    pub fn tracks_for(&self, lib: &Library, store: &Store) -> Vec<TrackId> {
        let rows = self.rows(lib, store);
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
    pub fn selected_track(&self, lib: &Library, store: &Store) -> Option<TrackId> {
        match self.rows(lib, store).get(self.selected) {
            Some(Row::Track { id, .. }) => Some(*id),
            _ => None,
        }
    }

    /// Open or close the node under the cursor — `za`, and `o`.
    ///
    /// Deliberately **not** `close()` then `open()`: `close` walks up when the
    /// node is already shut, which is right for `h` and wrong here — toggling
    /// a closed album would close its artist instead of opening the album.
    pub fn toggle(&mut self, lib: &Library, store: &Store) {
        let Some(path) = self.path_under(lib, store) else {
            return;
        };
        self.folds.toggle(&path);
    }

    /// `zo` — open the node under the cursor, and only it.
    pub fn open(&mut self, lib: &Library, store: &Store) {
        if let Some(path) = self.path_under(lib, store) {
            self.folds.open(&path);
        }
    }

    /// `zO` — open the node under the cursor and everything beneath it.
    pub fn open_deep(&mut self, lib: &Library, store: &Store) {
        let Some(path) = self.path_under(lib, store) else {
            return;
        };
        let mut paths = vec![path.clone()];
        paths.extend(self.descendants(lib, &path));
        self.folds.open_all(paths);
    }

    /// `zC` — close the node under the cursor and everything beneath it, so
    /// that reopening it does not spill the subtree you had expanded inside.
    pub fn close_deep(&mut self, lib: &Library, store: &Store) {
        let Some(path) = self.path_under(lib, store) else {
            return;
        };
        let mut paths = vec![path.clone()];
        paths.extend(self.descendants(lib, &path));
        self.keeping_cursor(lib, store, |t| t.folds.close_all(paths));
    }

    /// `zR` — every fold open. Depth 2 is the whole tree: artists at 0,
    /// albums at 1.
    pub fn open_everything(&mut self, lib: &Library, store: &Store) {
        self.keeping_cursor(lib, store, |t| t.folds.set_level(DEPTH));
    }

    /// `zM` — every fold closed.
    pub fn close_everything(&mut self, lib: &Library, store: &Store) {
        self.keeping_cursor(lib, store, |t| t.folds.set_level(0));
    }

    /// `zr` — one level more. From nothing that is artists, then albums.
    pub fn more(&mut self, lib: &Library, store: &Store) {
        self.keeping_cursor(lib, store, |t| t.folds.more());
    }

    /// `zm` — one level fewer.
    pub fn less(&mut self, lib: &Library, store: &Store) {
        self.keeping_cursor(lib, store, |t| t.folds.less());
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
    pub fn only(&mut self, lib: &Library, store: &Store) {
        let rows = self.rows(lib, store);
        let Some(row) = rows.get(self.selected) else {
            return;
        };
        let opens = Self::ancestors_of(row);
        let want = Self::mark_of(row);

        self.folds.set_level(0);
        self.folds.open_all(opens);

        if let Some(at) = self
            .rows(lib, store)
            .iter()
            .position(|r| Self::mark_of(r) == want)
        {
            self.selected = at;
        }
    }

    /// Put the cursor on `id`, opening whatever was hiding it.
    ///
    /// Nothing else is disturbed — the point is to find the playing track
    /// without losing the shape you had built up looking for it.
    pub fn reveal(&mut self, lib: &Library, store: &Store, id: TrackId) -> bool {
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
            .rows(lib, store)
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
    fn mark_of(row: &Row) -> (Vec<SortKey>, Option<TrackId>) {
        match row {
            Row::Track { id, .. } => (Self::seat_of(row), Some(*id)),
            _ => (Self::seat_of(row), None),
        }
    }

    /// The openable path under the cursor, if the cursor is on one.
    fn path_under(&self, lib: &Library, store: &Store) -> Option<Vec<SortKey>> {
        self.rows(lib, store)
            .get(self.selected)
            .and_then(Self::path_of)
    }

    /// Every openable path strictly under `path`. Only albums can be under
    /// anything, so this is one level deep by construction.
    fn descendants(&self, lib: &Library, path: &[SortKey]) -> Vec<Vec<SortKey>> {
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
    fn keeping_cursor(&mut self, lib: &Library, store: &Store, change: impl FnOnce(&mut Self)) {
        let seat = self.rows(lib, store).get(self.selected).map(Self::seat_of);
        change(self);
        let rows = self.rows(lib, store);
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
    pub fn close(&mut self, lib: &Library, store: &Store) -> bool {
        let rows = self.rows(lib, store);
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
            .rows(lib, store)
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
    use crate::store::Track;
    use std::path::PathBuf;

    fn store_with(rows: &[(&str, &str, &str, &str)]) -> Store {
        let mut s = Store::new();
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

    fn fixture() -> (Store, Library) {
        let store = store_with(&[
            ("/m/b1.mp3", "Burial", "Untrue", "Archangel"),
            ("/m/b2.mp3", "Burial", "Untrue", "Shell of Light"),
            ("/m/b3.mp3", "Burial", "Burial", "Night Bus"),
            ("/m/o1.mp3", "Mr. Oizo", "The Church", "Ham"),
        ]);
        let lib = Library::rebuild(&store, true);
        (store, lib)
    }

    #[test]
    fn collapsed_shows_only_artists() {
        let (store, lib) = fixture();
        let state = TreeState::default();
        let rows = state.rows(&lib, &store);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label(), "▸ Burial");
        assert_eq!(rows[1].label(), "▸ Mr. Oizo");
    }

    #[test]
    fn expanding_reveals_albums_indented() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib, &store);

        let rows = state.rows(&lib, &store);
        assert_eq!(
            rows.iter().map(Row::label).collect::<Vec<_>>(),
            ["▾ Burial", "  ▸ Burial", "  ▸ Untrue", "▸ Mr. Oizo"]
        );
    }

    /// The point of one pane: tracks sit inline under an open album, and
    /// several albums across several artists can be open at once.
    #[test]
    fn opening_an_album_puts_its_tracks_inline() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib, &store); // the artist
        state.selected = 1;
        state.toggle(&lib, &store); // its first album

        let rows = state.rows(&lib, &store);
        assert!(
            matches!(rows.get(2), Some(Row::Track { .. })),
            "no track inline: {:?}",
            rows.iter().map(Row::label).collect::<Vec<_>>()
        );

        // A second artist can be open at the same time.
        state.selected = rows.len() - 1;
        state.toggle(&lib, &store);
        let both = state.rows(&lib, &store);
        assert!(both.len() > rows.len(), "the second artist did not open");
        assert!(
            both.iter().any(|r| matches!(r, Row::Track { .. })),
            "the first artist's tracks closed when the second opened"
        );
    }

    #[test]
    fn selecting_an_album_narrows_the_track_pane() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib, &store); // expand Burial

        // Artist row selected: everything by that artist.
        state.selected = 0;
        assert_eq!(state.tracks_for(&lib, &store).len(), 3);

        // "Burial" the album: one track.
        state.selected = 1;
        assert_eq!(state.tracks_for(&lib, &store).len(), 1);

        // "Untrue": two.
        state.selected = 2;
        assert_eq!(state.tracks_for(&lib, &store).len(), 2);
    }

    #[test]
    fn collapsing_from_an_album_row_moves_the_cursor_to_the_artist() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib, &store);
        state.selected = 2; // an album row

        // `close` walks up when the row is already shut, which is how `h`
        // collapses outward without needing a concept of "level".
        state.close(&lib, &store);

        // The album rows are gone, so the selection must not be left dangling
        // over what is now a different artist.
        assert_eq!(state.selected, 0);
        assert_eq!(state.rows(&lib, &store).len(), 2);
    }

    /// Toggling an album must open it, not close its artist — `toggle` and
    /// `close` differ precisely here.
    #[test]
    fn toggling_a_closed_album_opens_it_rather_than_closing_the_artist() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib, &store);
        let with_albums = state.rows(&lib, &store).len();
        state.selected = 1;

        state.toggle(&lib, &store);
        assert!(
            state.rows(&lib, &store).len() > with_albums,
            "the artist collapsed instead of the album opening"
        );
    }

    #[test]
    fn zr_walks_down_a_level_at_a_time_and_zm_walks_back() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        assert_eq!(state.rows(&lib, &store).len(), 2, "2 artists");

        state.more(&lib, &store);
        assert_eq!(state.rows(&lib, &store).len(), 5, "+ 3 albums");

        state.more(&lib, &store);
        assert_eq!(state.rows(&lib, &store).len(), 9, "+ 4 tracks");

        state.less(&lib, &store);
        assert_eq!(state.rows(&lib, &store).len(), 5);
        state.less(&lib, &store);
        assert_eq!(state.rows(&lib, &store).len(), 2);
    }

    #[test]
    fn zm_at_the_top_stays_there_rather_than_wrapping() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.less(&lib, &store);
        state.less(&lib, &store);
        state.more(&lib, &store);
        assert_eq!(
            state.rows(&lib, &store).len(),
            5,
            "one zr after bottoming out should open the albums, not undo a debt"
        );
    }

    #[test]
    fn zr_opens_everything_and_zm_closes_it() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib, &store);
        assert_eq!(state.rows(&lib, &store).len(), 9);
        state.close_everything(&lib, &store);
        assert_eq!(state.rows(&lib, &store).len(), 2);
    }

    #[test]
    fn a_level_change_discards_what_was_opened_by_hand() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib, &store); // open Burial by hand
        state.close_everything(&lib, &store);
        assert_eq!(
            state.rows(&lib, &store).len(),
            2,
            "zM has to close what was opened by hand too, or it is not `all`"
        );
    }

    /// Deliberately on the *second* artist: landing on row 0 is what a cursor
    /// that does not ride up would also do, so testing the first artist proves
    /// nothing.
    #[test]
    fn collapsing_past_the_cursor_lands_it_on_the_row_that_swallowed_it() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_everything(&lib, &store);
        let rows = state.rows(&lib, &store);
        let oizo_track = rows
            .iter()
            .position(|r| matches!(r, Row::Track { artist, .. } if artist.as_str() == "mr. oizo"))
            .expect("Mr. Oizo's track should be on screen with every fold open");
        state.selected = oizo_track;

        state.close_everything(&lib, &store);
        let rows = state.rows(&lib, &store);
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
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        let shut = state.rows(&lib, &store).len();

        state.toggle(&lib, &store);
        assert!(state.rows(&lib, &store).len() > shut);
        state.toggle(&lib, &store);
        assert_eq!(
            state.rows(&lib, &store).len(),
            shut,
            "za has to go both ways — that is the whole of what it is"
        );
    }

    #[test]
    fn zo_opens_only_the_node_under_the_cursor() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.open(&lib, &store);
        assert_eq!(
            state.rows(&lib, &store).len(),
            4,
            "Burial's 2 albums, Mr. Oizo still shut"
        );
    }

    #[test]
    fn zo_opens_the_node_and_zo_again_does_not_close_it() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.open(&lib, &store);
        let opened = state.rows(&lib, &store).len();
        state.open(&lib, &store);
        assert_eq!(
            state.rows(&lib, &store).len(),
            opened,
            "zo is not a toggle — that is za"
        );
    }

    #[test]
    fn zo_deep_opens_the_albums_too() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_deep(&lib, &store);
        assert_eq!(
            state.rows(&lib, &store).len(),
            7,
            "Burial, its 2 albums and their 3 tracks, plus Mr. Oizo"
        );
    }

    #[test]
    fn zc_deep_does_not_spill_the_subtree_when_reopened() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.open_deep(&lib, &store);
        state.close_deep(&lib, &store);
        assert_eq!(state.rows(&lib, &store).len(), 2);

        state.open(&lib, &store);
        assert_eq!(
            state.rows(&lib, &store).len(),
            4,
            "reopening Burial should show its albums closed, not the tracks it had open"
        );
    }
}
