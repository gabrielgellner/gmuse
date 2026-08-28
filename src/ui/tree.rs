//! View 1: the artist/album tree.
//!
//! DESIGN §8 calls this out as the one custom widget the project needs. It is
//! not a generic tree — it is exactly two levels, and flattening it to rows
//! each frame is both simpler and faster than maintaining a parallel structure
//! that has to be kept in step with the library.

use std::collections::HashSet;

use crate::library::{Library, SortKey};
use crate::store::{Store, TrackId};

/// One visible line in the left pane.
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
    },
}

impl Row {
    /// How the row renders, including the expansion marker and indent.
    pub fn label(&self) -> String {
        match self {
            Row::Artist {
                display, expanded, ..
            } => format!("{} {display}", if *expanded { "▾" } else { "▸" }),
            Row::Album { display, .. } => format!("    {display}"),
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

#[derive(Debug, Default)]
pub struct TreeState {
    expanded: HashSet<SortKey>,
    pub selected: usize,
    pub track_selected: usize,
    pub pane: Pane,
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
            let expanded = self.expanded.contains(artist_key);
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
                rows.push(Row::Album {
                    artist: artist_key.clone(),
                    key: album_key.clone(),
                    display,
                    tracks: tracks.len(),
                });
            }
        }
        rows
    }

    /// Tracks shown in the right pane for the current selection: one album, or
    /// everything by the artist when an artist row is selected.
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
        }
    }

    /// Toggle the artist on the selected row.
    ///
    /// Selecting an *album* row and pressing space collapses its artist, which
    /// is what cmus does and saves a trip back up to the artist line.
    pub fn toggle(&mut self, lib: &Library, store: &Store) {
        let rows = self.rows(lib, store);
        let Some(row) = rows.get(self.selected) else {
            return;
        };
        let (key, from_album_row) = match row {
            Row::Artist { key, .. } => (key.clone(), false),
            Row::Album { artist, .. } => (artist.clone(), true),
        };
        if self.expanded.contains(&key) {
            self.expanded.remove(&key);
            if from_album_row {
                // Move the cursor to the artist we just closed, or the row
                // vanishes from under the selection.
                if let Some(pos) = rows
                    .iter()
                    .position(|r| matches!(r, Row::Artist { key: k, .. } if *k == key))
                {
                    self.selected = pos;
                }
            }
        } else {
            self.expanded.insert(key);
        }
        self.track_selected = 0;
    }

    /// Open every artist. Useful on a small library, painful on a large one,
    /// which is why it is not the default.
    pub fn expand_all(&mut self, lib: &Library) {
        for (key, _) in lib.artists() {
            self.expanded.insert(key.clone());
        }
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.track_selected = 0;
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
            ["▾ Burial", "    Burial", "    Untrue", "▸ Mr. Oizo"]
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

        state.toggle(&lib, &store);

        // The album rows are gone, so the selection must not be left dangling
        // over what is now a different artist.
        assert_eq!(state.selected, 0);
        assert_eq!(state.rows(&lib, &store).len(), 2);
    }

    #[test]
    fn expand_all_then_collapse_all() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.expand_all(&lib);
        assert_eq!(state.rows(&lib, &store).len(), 5); // 2 artists + 3 albums
        state.collapse_all();
        assert_eq!(state.rows(&lib, &store).len(), 2);
    }
}
