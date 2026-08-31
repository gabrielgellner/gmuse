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

use std::collections::HashSet;

use crate::library::{Library, SortKey};
use crate::store::{Store, TrackId};

/// What can be opened and closed. Tracks are leaves, so they are not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Node {
    Artist(SortKey),
    Album(SortKey, SortKey),
}

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

#[derive(Debug, Default)]
pub struct TreeState {
    expanded: HashSet<Node>,
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
            let expanded = self.expanded.contains(&Node::Artist(artist_key.clone()));
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
                let open = self
                    .expanded
                    .contains(&Node::Album(artist_key.clone(), album_key.clone()));
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

    /// The node a row belongs to, for opening and closing.
    fn node_of(row: &Row) -> Option<Node> {
        match row {
            Row::Artist { key, .. } => Some(Node::Artist(key.clone())),
            Row::Album { artist, key, .. } => Some(Node::Album(artist.clone(), key.clone())),
            Row::Track { .. } => None,
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

    /// Open or close the node under the cursor. A track has nothing to toggle.
    ///
    /// Deliberately **not** `close()` then `open()`: `close` walks up when the
    /// node is already shut, which is right for `h` and wrong here — toggling
    /// a closed album would close its artist instead of opening the album.
    pub fn toggle(&mut self, lib: &Library, store: &Store) {
        let rows = self.rows(lib, store);
        let Some(node) = rows.get(self.selected).and_then(Self::node_of) else {
            return;
        };
        if !self.expanded.remove(&node) {
            self.expanded.insert(node);
        }
    }

    /// Close the node under the cursor.
    ///
    /// If it is **already closed** — or is a track, which is never open — the
    /// *parent* closes instead and the cursor moves to it. That is snacks'
    /// rule, and it is what lets repeated `h` walk up and out without anything
    /// needing a concept of "level".
    pub fn close(&mut self, lib: &Library, store: &Store) -> bool {
        let rows = self.rows(lib, store);
        let Some(row) = rows.get(self.selected) else {
            return false;
        };
        let node = Self::node_of(row).filter(|n| self.expanded.contains(n));
        let target = match node {
            Some(n) => n,
            // Closed, or a leaf: go up.
            None => match row {
                Row::Artist { .. } => return false,
                Row::Album { artist, .. } => Node::Artist(artist.clone()),
                Row::Track { artist, album, .. } => Node::Album(artist.clone(), album.clone()),
            },
        };
        self.expanded.remove(&target);
        // The row would otherwise vanish from under the cursor.
        if let Some(pos) = self
            .rows(lib, store)
            .iter()
            .position(|r| Self::node_of(r).as_ref() == Some(&target))
        {
            self.selected = pos;
        }
        true
    }

    /// Open every artist. Useful on a small library, painful on a large one,
    /// which is why it is not the default.
    /// Artists only, not their albums: opening every album of every artist
    /// puts the whole library on screen as tracks, which is what view 2 is.
    pub fn expand_all(&mut self, lib: &Library) {
        for (key, _) in lib.artists() {
            self.expanded.insert(Node::Artist(key.clone()));
        }
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
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
    fn expand_all_then_collapse_all() {
        let (store, lib) = fixture();
        let mut state = TreeState::default();
        state.expand_all(&lib);
        assert_eq!(state.rows(&lib, &store).len(), 5); // 2 artists + 3 albums
        state.collapse_all();
        assert_eq!(state.rows(&lib, &store).len(), 2);
    }
}
