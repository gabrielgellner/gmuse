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

use crate::browse::Spine;
use crate::cmd::Motion;
use crate::model::rows::CopyId;
use crate::shelf::{Shelf, SortKey};
use crate::ui::fold::Expansion;

/// One visible line.
/// **What identifies a row across a rebuild** — a container by its path, a track by
/// its id. Named because two methods and a search all pass it around (#131).
pub type Mark = (Vec<SortKey>, Option<CopyId>);

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
        /// **How long the record is, when this is short of it** — #137.
        ///
        /// `None` when the album is whole, when nothing counted it, or when
        /// the group's copies come off more than one pressing. Absent is the
        /// common case by a long way: 5,577 of 6,485 files here sit on records
        /// held complete, and a row reading `12 of 12` would be noise on all
        /// of them.
        of: Option<u16>,
    },
    /// **One disc of a record that is more than one** — #169.
    ///
    /// Drawn only where the album's held copies span several. A five-disc
    /// *Let It Be* has five track 1s, and without this they sit adjacent under
    /// identical numbering — which reads as the library holding one song five
    /// times, and is exactly what #168's shadow rows looked like from a
    /// completely different cause.
    ///
    /// The ordering was never wrong: `place_of` returns `(disc, track)` and
    /// the tree has always sorted by it. What was missing is a row saying
    /// where one disc ends.
    Disc {
        artist: SortKey,
        album: SortKey,
        key: SortKey,
        number: u16,
        format: Option<String>,
        tracks: usize,
        expanded: bool,
    },
    Track {
        artist: SortKey,
        album: SortKey,
        /// The disc this track's row sits under, where one is drawn — #169.
        /// `None` on a single-disc record, which is nearly all of them.
        disc: Option<SortKey>,
        id: CopyId,
        /// **This track is the whole of its group, so the group is not drawn**
        /// — #135. It sits where its album row would have been.
        ///
        /// The album still exists in the model; what it does not get is a row
        /// of its own, because a row that opens to reveal exactly one thing
        /// costs a keystroke and tells you nothing. In a well-formed library
        /// this is rare — 3 of 495 albums here. In a library of singles it is
        /// nearly all of them: 466 of 490.
        alone: bool,
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
                display,
                expanded,
                tracks,
                of,
                ..
            } => {
                let mark = if *expanded { "▾" } else { "▸" };
                match of {
                    Some(total) => format!("  {mark} {display}  {tracks} of {total}"),
                    None => format!("  {mark} {display}"),
                }
            }
            // **At the album's indent when it stands in for one**, so the
            // artist's children stay in one column whether they are albums or
            // the tracks that replaced them.
            //
            // **And the caret column always says something.** A track standing
            // in for its group sits exactly where an album row would, so
            // nothing distinguished "opens to reveal its tracks" from "is a
            // track" — and `l` on one looks inviting and does nothing. In a
            // library of singles that is not an edge case: 466 of 490 albums.
            //
            // `▸` closed, `▾` open, `·` nothing to open. One column, three
            // states, and the widths are unchanged so no row shifts.
            Row::Disc {
                number,
                format,
                expanded,
                ..
            } => {
                let mark = if *expanded { "▾" } else { "▸" };
                // `Disc 2 · CD` where the authority said what it was, and
                // `Disc 2` where nobody did — carried because MusicBrainz has
                // it and a bare number tells you less than the sleeve does.
                //
                // **No count**, matching the album row above it: a bare `1`
                // beside a header reads as a track number, and the row it sits
                // under does not print one either.
                let what = format
                    .as_deref()
                    .map_or_else(String::new, |f| format!(" · {f}"));
                format!("    {mark} Disc {number}{what}")
            }
            // Three depths now: standing in for an album (#135), under an
            // album, and under a disc. The caret column still says the same
            // three things — `▸` closed, `▾` open, `·` nothing to open — and
            // only the indent moves.
            Row::Track { alone, disc, .. } => match (alone, disc) {
                (true, _) => "  · ",
                (false, None) => "    · ",
                (false, Some(_)) => "      · ",
            }
            .to_string(),
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
            Row::Artist { expanded, .. }
            | Row::Album { expanded, .. }
            | Row::Disc { expanded, .. } => *expanded,
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

/// How deep the tree goes: artists at depth 0, albums at depth 1, and a disc
/// at depth 2 where a record has more than one (#169). `zR` sets the level
/// here, and there is nothing below a disc but tracks, which are leaves and
/// have no fold of their own.
///
/// **Three even though almost nothing is three deep.** 1,003 of this library's
/// 1,029 albums hold their copies on one disc and grow no disc row, so `zR`
/// reaches their tracks at level 2 and the extra level costs them nothing —
/// `set_level` opens folds that exist, and a level with no folds in it is not
/// a level anybody has to step through.
const DEPTH: u16 = 3;

/// The depth a disc row sits at: `[artist, album, disc]` is three keys, which
/// is depth 2. Folds from here down open by themselves (#169) — a header that
/// exists to say where a disc starts should not also hide it.
const DISC_DEPTH: u16 = 2;

#[derive(Debug)]
pub struct TreeState {
    /// Keyed on the ident path — `[artist]` for an artist, `[artist, album]`
    /// for an album, `[artist, album, disc]` for a disc — so expansion
    /// survives a re-sort or a rescan.
    folds: Expansion<SortKey>,
    pub selected: usize,
}

/// By hand, because the fold state is not the plain default: the disc level
/// opens by itself and something has to say so.
impl Default for TreeState {
    fn default() -> Self {
        let mut folds = Expansion::default();
        folds.opening_from(DISC_DEPTH);
        Self { folds, selected: 0 }
    }
}

impl TreeState {
    /// Flatten the library into visible rows, honouring what is expanded.
    ///
    /// Display names come from the first track under each node, since §4 keeps
    /// display strings on the `Track` and only sort keys in the tree.
    pub fn rows(&self, lib: &Shelf) -> Vec<Row> {
        self.flatten(lib, false)
    }

    /// Every row, as if nothing were folded — #131.
    ///
    /// **What `/` searches.** A closed fold still holds its rows, exactly as a
    /// closed fold in vim still holds its lines: `/malibu` with the artists
    /// shut found nothing at all, because the album row it wanted did not
    /// exist yet. The search looks here and the reveal opens the way to what
    /// it found.
    pub fn all_rows(&self, lib: &Shelf) -> Vec<Row> {
        self.flatten(lib, true)
    }

    fn flatten(&self, lib: &Shelf, unfolded: bool) -> Vec<Row> {
        let mut rows = Vec::new();
        for (artist_key, albums) in lib.artists() {
            let display = lib.artist_name(artist_key).to_string();
            let expanded = unfolded || self.folds.is_open(std::slice::from_ref(artist_key));
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
                // **A group of one is not a level** (#135). The album row
                // would open onto exactly this track, so the track takes its
                // place — and there is no fold, because there is nothing left
                // to open.
                //
                // Applied in both listings, unfolded included: a level that is
                // not drawn does not exist to be revealed, and a search that
                // found one could not put the cursor anywhere.
                if let [only] = tracks[..] {
                    rows.push(Row::Track {
                        artist: artist_key.clone(),
                        album: album_key.clone(),
                        disc: None,
                        id: only,
                        alone: true,
                    });
                    continue;
                }
                let display = lib.album_name(artist_key, album_key).to_string();
                let open = unfolded || self.folds.is_open(&[artist_key.clone(), album_key.clone()]);
                rows.push(Row::Album {
                    artist: artist_key.clone(),
                    key: album_key.clone(),
                    display,
                    tracks: tracks.len(),
                    expanded: open,
                    of: lib.album_total(artist_key, album_key),
                });
                if !open {
                    continue;
                }
                // **A disc is a level only when there is more than one** —
                // #169, and #135's rule one level up: a header that opens onto
                // everything beneath it costs a keystroke and says nothing.
                // Measured, 1,003 of this library's 1,029 albums hold their
                // copies on one disc and grow no row at all.
                if lib.album_discs(artist_key, album_key) < 2 {
                    for id in tracks {
                        rows.push(Row::Track {
                            artist: artist_key.clone(),
                            album: album_key.clone(),
                            disc: None,
                            id: *id,
                            alone: false,
                        });
                    }
                    continue;
                }
                // The tracks arrive sorted by `(disc, track)` already, so
                // grouping is a run over them rather than a re-sort — and the
                // header cannot disagree with the order for the same reason.
                let mut at = 0;
                while at < tracks.len() {
                    let number = lib.disc_of(tracks[at]);
                    let end = tracks[at..]
                        .iter()
                        .position(|id| lib.disc_of(*id) != number)
                        .map_or(tracks.len(), |n| at + n);
                    let run = &tracks[at..end];
                    at = end;
                    let Some(number) = number else {
                        // A copy nothing placed has no disc to sit under, so
                        // it stays where an ordinary track would.
                        for id in run {
                            rows.push(Row::Track {
                                artist: artist_key.clone(),
                                album: album_key.clone(),
                                disc: None,
                                id: *id,
                                alone: false,
                            });
                        }
                        continue;
                    };
                    // Zero-padded, because a `SortKey` orders as text and
                    // `disc 10` would otherwise sit between 1 and 2.
                    let key = crate::model::sortkey::sort_key(&format!("{number:03}"), false);
                    let path = [artist_key.clone(), album_key.clone(), key.clone()];
                    let disc_open = unfolded || self.folds.is_open(&path);
                    rows.push(Row::Disc {
                        artist: artist_key.clone(),
                        album: album_key.clone(),
                        key: key.clone(),
                        number,
                        format: lib.disc_format(artist_key, album_key, number),
                        tracks: run.len(),
                        expanded: disc_open,
                    });
                    if !disc_open {
                        continue;
                    }
                    for id in run {
                        rows.push(Row::Track {
                            artist: artist_key.clone(),
                            album: album_key.clone(),
                            disc: Some(key.clone()),
                            id: *id,
                            alone: false,
                        });
                    }
                }
            }
        }
        rows
    }

    /// **Open the way to a row and put the cursor on it** — #131.
    ///
    /// What a search needs, and different from [`Self::reveal`], which finds a
    /// *track* by its id. This is handed a row it already found in the
    /// unfolded list and has only to make it visible.
    ///
    /// The tree is three levels whatever `levels` groups by, so this is the
    /// same operation for `[artist, album]` and for `[genre, album]`.
    pub fn reveal_row(&mut self, lib: &Shelf, row: &Row) {
        match row {
            Row::Artist { .. } => {}
            Row::Album { artist, .. } => self.folds.open(std::slice::from_ref(artist)),
            Row::Disc { artist, album, .. } => {
                self.folds.open(std::slice::from_ref(artist));
                self.folds.open(&[artist.clone(), album.clone()]);
            }
            // A track standing in for its group has only the artist above it.
            Row::Track {
                artist,
                alone: true,
                ..
            } => self.folds.open(std::slice::from_ref(artist)),
            Row::Track { artist, album, .. } => {
                self.folds.open(std::slice::from_ref(artist));
                self.folds.open(&[artist.clone(), album.clone()]);
            }
        }
        // **Found again by mark, not by equality**: a `Row` carries whether it
        // is expanded, and that is exactly what has just changed.
        let want = Self::mark_of_inner(row);
        if let Some(at) = self
            .rows(lib)
            .iter()
            .position(|r| Self::mark_of_inner(r) == want)
        {
            self.selected = at;
        }
    }

    /// The fold state, to put back if a search is abandoned — #131.
    pub fn folds(&self) -> Expansion<SortKey> {
        self.folds.clone()
    }

    /// **The disc rule is re-applied**, because a fold state restored from a
    /// session or a `:view` was built without it and would draw every disc
    /// shut.
    pub fn set_folds(&mut self, mut folds: Expansion<SortKey>) {
        folds.opening_from(DISC_DEPTH);
        self.folds = folds;
    }

    /// The path of a row that can be opened. A track has none — it is a leaf.
    fn path_of(row: &Row) -> Option<Vec<SortKey>> {
        match row {
            Row::Artist { key, .. } => Some(vec![key.clone()]),
            Row::Album { artist, key, .. } => Some(vec![artist.clone(), key.clone()]),
            Row::Disc {
                artist, album, key, ..
            } => Some(vec![artist.clone(), album.clone(), key.clone()]),
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
            Row::Disc {
                artist, album, key, ..
            } => vec![artist.clone(), album.clone(), key.clone()],
            // **Where the cursor falls back to** — the artist, for a track
            // that replaced its album row, because that album's fold is not a
            // place any more (#135).
            Row::Track {
                artist,
                alone: true,
                ..
            } => vec![artist.clone()],
            // **Under its disc where there is one**, so `h` from a track on
            // disc 2 closes that disc rather than the whole record.
            Row::Track {
                artist,
                album,
                disc: Some(disc),
                ..
            } => vec![artist.clone(), album.clone(), disc.clone()],
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
            // A disc plays the disc, which is what `x` on one should mean.
            Row::Disc {
                artist, album, key, ..
            } => lib
                .artists()
                .find(|(k, _)| *k == artist)
                .and_then(|(_, albums)| albums.get(album))
                .map(|tracks| {
                    tracks
                        .iter()
                        .copied()
                        .filter(|id| {
                            lib.disc_of(*id).is_some_and(|n| {
                                crate::model::sortkey::sort_key(&format!("{n:03}"), false) == *key
                            })
                        })
                        .collect()
                })
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
            Row::Disc { .. } => 2,
            Row::Track { disc: Some(_), .. } => 3,
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
        step_over(&spine_of(rows), self.selected, motion)
    }

    /// Every fold that must be open for `row` to be on screen.
    ///
    /// An artist needs none — level 0 shows them. An album needs its artist. A
    /// track needs both, which is why this is a list and not a parent link.
    fn ancestors_of(row: &Row) -> Vec<Vec<SortKey>> {
        match row {
            Row::Artist { .. } => Vec::new(),
            Row::Album { artist, .. } => vec![vec![artist.clone()]],
            Row::Disc { artist, album, .. } => {
                vec![vec![artist.clone()], vec![artist.clone(), album.clone()]]
            }
            // **And its disc where it has one** (#169), or revealing a track
            // on disc 2 would open the record and stop at the header.
            Row::Track {
                artist,
                album,
                disc: Some(disc),
                ..
            } => vec![
                vec![artist.clone()],
                vec![artist.clone(), album.clone()],
                vec![artist.clone(), album.clone(), disc.clone()],
            ],
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
        let want = Self::mark_of_inner(row);

        self.folds.set_level(0);
        self.folds.open_all(opens);

        if let Some(at) = self
            .rows(lib)
            .iter()
            .position(|r| Self::mark_of_inner(r) == want)
        {
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

    /// **What the cursor is on, in terms that survive the list being rebuilt.**
    ///
    /// `folds` has been keyed on the ident path since it was written, "so expansion
    /// survives a re-sort or a rescan" — and the cursor was the one thing not given the
    /// same treatment. `rebuild_library` set it to zero, which was right while the only
    /// callers were a `:view` change and a filter change: those produce a genuinely
    /// different list, and no index into the old one means anything.
    ///
    /// It stopped being right when a librarian change started causing a rebuild. Rating
    /// a track, or a play being counted halfway through one, sent the cursor to the top
    /// of the tree — and the second happens with nobody touching the keyboard.
    pub fn mark(&self, lib: &Shelf) -> Option<Mark> {
        self.rows(lib).get(self.selected).map(Self::mark_of_inner)
    }

    /// Put the cursor back on what [`Self::mark`] remembered.
    ///
    /// **Falls back to where it was, not to the top.** If the row is gone — the track
    /// was retired, the filter no longer admits it — the nearest thing to the truth is
    /// the position the cursor held, clamped to the list that exists now. Row 0 is a
    /// guess that happens to be wrong every time the list is longer than one.
    pub fn point_at(&mut self, lib: &Shelf, mark: Option<Mark>) {
        let rows = self.rows(lib);
        self.selected =
            match mark.and_then(|want| rows.iter().position(|r| Self::mark_of_inner(r) == want)) {
                Some(at) => at,
                None => self.selected.min(rows.len().saturating_sub(1)),
            };
    }

    /// What identifies a row across a fold change: a track by its id, a
    /// container by its path. Not the `Row` itself — that carries whether it is
    /// expanded, which is precisely what a fold change alters.
    fn mark_of_inner(row: &Row) -> (Vec<SortKey>, Option<CopyId>) {
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
                    .iter()
                    // **A group of one has no fold to open** (#135). Reporting
                    // one would make `zR` believe there was something still
                    // shut, on a row that is not drawn.
                    .filter(|(_, tracks)| tracks.len() > 1)
                    .map(|(album, _)| vec![artist.clone(), album.clone()])
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
                // A closed disc goes up to its record.
                Row::Disc { artist, album, .. } => vec![artist.clone(), album.clone()],
                // **A track standing in for its group closes the artist**, not
                // the album it stands in for. That album has no row (#135), so
                // closing it folded something invisible: nothing moved, the
                // cursor could not follow, and `h` looked broken on the
                // commonest row in a library of singles.
                //
                // `reveal_row` already takes this view from the other side —
                // "a track standing in for its group has only the artist above
                // it" — and this is the same statement going up.
                Row::Track {
                    artist,
                    album,
                    disc,
                    alone,
                    ..
                } => match (alone, disc) {
                    (true, _) => vec![artist.clone()],
                    // **And a track on disc 2 closes disc 2**, not the record
                    // — one step up is one step up, and closing the album
                    // from here would shut four other discs the cursor was
                    // nowhere near.
                    (false, Some(disc)) => vec![artist.clone(), album.clone(), disc.clone()],
                    (false, None) => vec![artist.clone(), album.clone()],
                },
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

/// What identifies a row across a fold change — see `TreeState::mark_of_inner`.
///
/// Free rather than associated so a caller comparing a row from the unfolded
/// list with one from the folded list does not need a `TreeState` in hand.
pub fn mark_of(row: &Row) -> (Vec<SortKey>, Option<CopyId>) {
    TreeState::mark_of_inner(row)
}

/// The shape of these rows, and nothing else about them — #261.
///
/// The transitional half of `tree-over-the-wire.md`: today the view flattens a `Shelf` and
/// derives this, and the destination is the librarian sending it. Written as a function of
/// `&[Row]` so both roads arrive at the same navigator.
pub fn spine_of(rows: &[Row]) -> Spine {
    let mut spine = Spine::default();
    for row in rows {
        spine.push(
            TreeState::depth_of(row) as u8,
            row.is_leaf(),
            row.is_expanded(),
        );
    }
    spine
}

/// Move by the shape of the tree — every motion, over four bits a row.
///
/// **A free function over a `Spine`, not a method on the state**, because the destination
/// has no rows to be a method on: a view holds a page, a cursor and this. `TreeState::step`
/// is a caller of it and will go when the rows do.
pub fn step_over(spine: &Spine, at: usize, motion: Motion) -> Option<usize> {
    let depth = spine.depth(at)?;

    let scan = |want: u8, forward: bool| -> Option<usize> {
        if forward {
            (at + 1..spine.len()).find(|i| spine.depth(*i) == Some(want))
        } else {
            (0..at).rev().find(|i| spine.depth(*i) == Some(want))
        }
    };

    match motion {
        Motion::NextSibling => scan(depth, true),
        Motion::PrevSibling => scan(depth, false),
        Motion::NextSection => scan(0, true),
        Motion::PrevSection => scan(0, false),
        // The nearest row above that is shallower — its container.
        Motion::Parent => (0..at).rev().find(|i| spine.depth(*i) < Some(depth)),
        // The group is this row if it is a container, otherwise the one holding it — so
        // `]z` on an album goes to its last track, and on a track to the last track beside
        // it.
        //
        // Asked as "is this row a container", not as "is its depth below 2". The depths
        // happen to agree today because the tree is exactly artist/album/track, and would
        // stop agreeing the moment it is not — which is a shape this file otherwise makes
        // no assumption about, and is why `is_leaf` gets a bit of its own.
        Motion::GroupEnd => {
            let start = if spine.is_leaf(at)? {
                (0..at).rev().find(|i| spine.depth(*i) < Some(depth))?
            } else {
                at
            };
            group_end(spine, start).filter(|e| *e != at)
        }
        // `][` and `[]`. The end of the section the cursor is in, and once it is there, the
        // end of the next one — which is what makes a repeated `][` walk section ends the
        // way a repeated `]]` walks section starts.
        Motion::NextSectionEnd => {
            let here = section_start(spine, at)?;
            match group_end(spine, here) {
                Some(end) if end > at => Some(end),
                _ => {
                    let next = scan(0, true)?;
                    group_end(spine, next).or(Some(next))
                }
            }
        }
        // The row before this section begins is the last row of the one before it, so this
        // needs no second scan. Landing on it moves the cursor into that section, and the
        // next press walks back again.
        Motion::PrevSectionEnd => match section_start(spine, at)? {
            0 => None,
            start => Some(start - 1),
        },
        Motion::NextFold => (at + 1..spine.len()).find(|i| spine.is_leaf(*i) == Some(false)),
        // vim's `zk` is the **end** of the previous fold, not its start — `:h zk`. Landing
        // on the start instead made `zj` and `zk` a pair that did not undo each other, and
        // left no key for the end at all.
        //
        // "A closed fold is counted as one fold", so a closed container is its own end; an
        // open one ends where the depth drops back.
        Motion::PrevFold => (0..at).rev().find(|i| is_fold_end(spine, *i)),
    }
}

/// The last row belonging to the group that starts at `start`, or `None` when it holds
/// nothing.
fn group_end(spine: &Spine, start: usize) -> Option<usize> {
    let floor = spine.depth(start)?;
    (start + 1..spine.len())
        .take_while(|i| spine.depth(*i) > Some(floor))
        .last()
}

/// The depth-0 row at or above `at` — the section it belongs to.
fn section_start(spine: &Spine, at: usize) -> Option<usize> {
    (0..=at).rev().find(|i| spine.depth(*i) == Some(0))
}

/// Whether a fold ends on this row, in vim's sense: a closed fold counts as one fold and
/// ends at itself, and an open one ends on the last row before the depth drops back.
fn is_fold_end(spine: &Spine, i: usize) -> bool {
    if spine.is_leaf(i) == Some(false) && spine.is_expanded(i) == Some(false) {
        return true;
    }
    match spine.depth(i + 1) {
        Some(next) => Some(next) < spine.depth(i),
        None => spine.depth(i) > Some(0),
    }
}

/// **The album name a collapsed row has to carry** — #135.
///
/// A group of one is not drawn, so the album name that used to be on the row
/// above is nowhere. This puts it back on the row that replaced it — unless
/// the title already begins with it, which is the shape a release named after
/// its own lead track has: `Fake I.D` holding `Fake I.D (Original Mix)`.
/// Printing both would be the noise this issue is about.
///
/// **One answer, asked by both the drawing and the search**, so what matches
/// is what somebody can see. A haystack holding text nobody drew is the bug
/// #131 fixed and this would otherwise reintroduce.
pub fn stands_for(lib: &Shelf, row: &Row, title: &str) -> Option<String> {
    let Row::Track {
        artist,
        album,
        alone: true,
        ..
    } = row
    else {
        return None;
    };
    let name = lib.album_name(artist, album);
    // **The count comes even when the name does not** (#137). One track of
    // twelve is exactly the row somebody wants flagged, and it is the row
    // #135 took the album header away from.
    let short = lib
        .album_total(artist, album)
        .map(|total| format!("1 of {total}"));
    let already = title.to_lowercase().starts_with(&name.to_lowercase());
    match (name.is_empty() || already, short) {
        (true, None) => None,
        (true, Some(count)) => Some(count),
        (false, None) => Some(name.to_string()),
        (false, Some(count)) => Some(format!("{name}  {count}")),
    }
}

#[cfg(test)]
mod spine_agrees {
    use super::*;
    use crate::fixture::Tagged as Track;
    use crate::operations::scan::Holdings as Scan;
    use std::path::PathBuf;

    /// Every shape the flatten can emit, in one library.
    ///
    /// **The awkward ones are the point.** A one-track album is drawn as a track *in place of*
    /// its album row, so a leaf sits at depth 2 among depth-1 siblings; a record spanning discs
    /// grows headers at depth 2 with its tracks at 3; and a copy nothing placed stays at depth
    /// 2 among them. Those three are exactly where a navigator that reasons from depth alone
    /// goes wrong, and why `is_leaf` has a bit of its own.
    fn every_shape() -> Shelf {
        let mut s = Scan::new();
        let mut add = |path: &str, artist: &str, album: &str, title: &str, disc: Option<u32>| {
            s.upsert(Track {
                path: PathBuf::from(path),
                artist: Some(artist.into()),
                album: Some(album.into()),
                title: Some(title.into()),
                discnumber: disc,
                disc_total: disc.map(|_| 2),
                ..Default::default()
            });
        };
        // An ordinary two-track record.
        add("/m/a1.mp3", "Burial", "Untrue", "Archangel", None);
        add("/m/a2.mp3", "Burial", "Untrue", "Shell of Light", None);
        // A record held as one track, which is drawn without an album row.
        add("/m/a3.mp3", "Burial", "Rival Dealer", "Hiders", None);
        // A record over two discs, plus a copy nothing placed.
        add(
            "/m/b1.mp3",
            "The Beatles",
            "The Beatles",
            "Back in the USSR",
            Some(1),
        );
        add(
            "/m/b2.mp3",
            "The Beatles",
            "The Beatles",
            "Dear Prudence",
            Some(1),
        );
        add(
            "/m/b3.mp3",
            "The Beatles",
            "The Beatles",
            "Birthday",
            Some(2),
        );
        add(
            "/m/b4.mp3",
            "The Beatles",
            "The Beatles",
            "Yer Blues",
            Some(2),
        );
        add("/m/b5.mp3", "The Beatles", "The Beatles", "Unplaced", None);
        // A second artist, so the section motions have somewhere to go.
        add("/m/c1.mp3", "Mr. Oizo", "The Church", "Ham", None);
        add("/m/c2.mp3", "Mr. Oizo", "The Church", "Bear Biscuit", None);
        Shelf::rebuild(&crate::fixture::from_holdings(&s), true)
    }

    const EVERY: [Motion; 10] = [
        Motion::NextSibling,
        Motion::PrevSibling,
        Motion::NextSection,
        Motion::PrevSection,
        Motion::NextSectionEnd,
        Motion::PrevSectionEnd,
        Motion::Parent,
        Motion::GroupEnd,
        Motion::NextFold,
        Motion::PrevFold,
    ];

    /// **Four bits a row is the navigator's whole input, checked rather than claimed** — #261.
    ///
    /// `tree-over-the-wire.md` rests on this: if a spine cannot drive every motion to the same
    /// answer the rows do, a view cannot stop holding the tree. So the row-based `step` is kept
    /// as an oracle and every (row, motion) pair is compared against it, over a library holding
    /// each shape the flatten can emit, at four fold states.
    #[test]
    fn a_spine_moves_the_cursor_exactly_where_the_rows_did() {
        let lib = every_shape();
        let mut checked = 0;
        let mut moved = 0;
        let mut disagreed = Vec::new();

        for opening in 0..4u16 {
            let mut state = TreeState::default();
            state.folds.set_level(opening);
            let rows = state.rows(&lib);
            let spine = spine_of(&rows);
            assert_eq!(spine.len(), rows.len(), "the spine lost rows");

            for at in 0..rows.len() {
                state.selected = at;
                for motion in EVERY {
                    let want = state.step_over_rows(&rows, motion);
                    let got = step_over(&spine, at, motion);
                    checked += 1;
                    moved += usize::from(got.is_some());
                    if want != got {
                        disagreed.push(format!(
                            "level {opening}, row {at} ({:?}), {}: rows say {want:?}, spine says {got:?}",
                            rows[at].label(),
                            motion.name()
                        ));
                    }
                }
            }
        }

        // **The instrument, before the claim.** Two ways this passes over nothing: no rows to
        // walk, and every motion answering `None` — which `assert!(disagreed.is_empty())` would
        // report as agreement.
        assert!(
            checked > 400,
            "only {checked} (row, motion) pairs compared — the fixture is not the tree this \
             claims to have walked"
        );
        assert!(
            moved * 2 > checked,
            "only {moved} of {checked} motions went anywhere; two navigators that both refuse \
             to move agree about nothing"
        );
        assert!(disagreed.is_empty(), "{}", disagreed.join("\n"));
    }

    /// **The fixture really holds the awkward shapes**, or the agreement above is about a tree
    /// that is only ever artist/album/track.
    #[test]
    fn the_fixture_draws_every_depth() {
        let lib = every_shape();
        let mut state = TreeState::default();
        state.open_everything(&lib);
        let rows = state.rows(&lib);
        let spine = spine_of(&rows);

        for depth in 0..=3u8 {
            assert!(
                (0..spine.len()).any(|i| spine.depth(i) == Some(depth)),
                "no row at depth {depth}; drawn: {:?}",
                rows.iter().map(Row::label).collect::<Vec<_>>()
            );
        }
        // The one that depth alone cannot tell apart: a leaf and a container sharing depth 2.
        let at_two = |leaf: bool| {
            (0..spine.len()).any(|i| spine.depth(i) == Some(2) && spine.is_leaf(i) == Some(leaf))
        };
        assert!(
            at_two(true),
            "no leaf at depth 2 — `is_leaf` is untested here"
        );
        assert!(
            at_two(false),
            "no container at depth 2 — the disc row is missing"
        );
    }

    /// A spine survives the wire unchanged.
    #[test]
    fn hex_is_a_round_trip() {
        let lib = every_shape();
        let mut state = TreeState::default();
        state.open_everything(&lib);
        let spine = spine_of(&state.rows(&lib));
        assert!(
            spine.len() > 10,
            "too small to be a round trip worth making"
        );
        assert_eq!(Spine::from_hex(&spine.to_hex()), Some(spine.clone()));
        assert_eq!(spine.to_hex().len(), spine.len(), "one hex digit a row");
        assert_eq!(Spine::from_hex("not hex"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl TreeState {
        pub(super) fn step_over_rows(&self, rows: &[Row], motion: Motion) -> Option<usize> {
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
    }

    use crate::fixture::Tagged as Track;
    use crate::operations::scan::Holdings as Scan;
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
            // **Two tracks apiece, because these tests are about folds** and a
            // group of one is not drawn any more (#135). A one-track album
            // here would have no fold to test.
            ("/m/b4.mp3", "Burial", "Burial", "Distant Lights"),
            ("/m/o1.mp3", "Mr. Oizo", "The Church", "Ham"),
            ("/m/o2.mp3", "Mr. Oizo", "The Church", "Bear Biscuit"),
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

    /// **A record you have part of says so, and a whole one does not** —
    /// #137.
    ///
    /// `12 of 12` would be noise on 5,577 of the 6,485 files here. The number
    /// is worth a row only where it is short.
    #[test]
    fn an_album_says_how_much_of_it_is_here_only_when_it_is_short() {
        let some_of = |held: u32, total: u16| {
            let mut s = Scan::new();
            for n in 1..=held {
                s.upsert(Track {
                    path: PathBuf::from(format!("/m/Band/Record/{n}.mp3")),
                    artist: Some("Band".into()),
                    album: Some("Record".into()),
                    title: Some(format!("Track {n}")),
                    tracknumber: Some(n),
                    track_total: Some(total),
                    disc_total: Some(1),
                    ..Default::default()
                });
            }
            let lib = Shelf::rebuild(&crate::fixture::from_holdings(&s), true);
            let mut state = TreeState::default();
            state.open_everything(&lib);
            state
                .rows(&lib)
                .iter()
                .map(Row::label)
                .collect::<Vec<_>>()
                .join("\n")
        };

        assert!(
            some_of(2, 5).contains("Record  2 of 5"),
            "a record held in part did not say so: {}",
            some_of(2, 5)
        );
        assert!(
            !some_of(3, 3).contains(" of "),
            "a whole record counted itself: {}",
            some_of(3, 3)
        );

        // Nothing counted it, so there is nothing to be short of.
        let mut quiet = Scan::new();
        for n in 1..=2u32 {
            quiet.upsert(Track {
                path: PathBuf::from(format!("/m/Band/Other/{n}.mp3")),
                artist: Some("Band".into()),
                album: Some("Other".into()),
                title: Some(format!("Track {n}")),
                tracknumber: Some(n),
                ..Default::default()
            });
        }
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&quiet), true);
        let mut state = TreeState::default();
        state.open_everything(&lib);
        assert!(
            !state
                .rows(&lib)
                .iter()
                .any(|r| Row::label(r).contains(" of ")),
            "an uncounted record claimed a total"
        );
    }

    /// **A group whose copies come off two pressings says nothing** — #137.
    ///
    /// A tree node is a release *group*, and the count is a fact about one
    /// pressing. Printing one pressing's total beside a count of copies from
    /// both would be halves that do not belong together — and can read as
    /// more tracks held than the record has.
    #[test]
    fn an_album_spanning_two_pressings_does_not_count_itself() {
        let mut s = Scan::new();
        // Same album title, two editions — the reissue has more tracks. The
        // fixture separates them by MusicBrainz release id, which is what
        // mints two pressings under one group.
        for (n, release, total) in [(1u32, 1u128, 10u16), (2, 2, 12)] {
            s.upsert(Track {
                path: PathBuf::from(format!("/m/Band/Record/{n}.mp3")),
                artist: Some("Band".into()),
                album: Some("Record".into()),
                title: Some(format!("Track {n}")),
                tracknumber: Some(n),
                track_total: Some(total),
                disc_total: Some(1),
                mb_release: Some(uuid::Uuid::from_u128(release)),
                ..Default::default()
            });
        }
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&s), true);
        let mut state = TreeState::default();
        state.open_everything(&lib);
        assert!(
            !state
                .rows(&lib)
                .iter()
                .any(|r| Row::label(r).contains(" of ")),
            "a group spanning two pressings printed one of their totals: {:?}",
            state.rows(&lib).iter().map(Row::label).collect::<Vec<_>>()
        );
    }

    /// **The one-track case still says it** — #137 meeting #135.
    ///
    /// A row that is 1 of 12 is exactly what somebody wants flagged, and it is
    /// the row whose album header was taken away.
    #[test]
    fn a_collapsed_row_still_says_how_short_it_is() {
        let mut s = Scan::new();
        s.upsert(Track {
            path: PathBuf::from("/m/The Bug/London Zoo/1.mp3"),
            artist: Some("The Bug".into()),
            album: Some("London Zoo".into()),
            title: Some("Poison Dart".into()),
            tracknumber: Some(3),
            track_total: Some(12),
            disc_total: Some(1),
            ..Default::default()
        });
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&s), true);
        let mut state = TreeState::default();
        state.open_everything(&lib);
        let rows = state.rows(&lib);
        let row = rows
            .iter()
            .find(|r| matches!(r, Row::Track { .. }))
            .expect("the track that replaced its album");
        assert_eq!(
            stands_for(&lib, row, "Poison Dart").as_deref(),
            Some("London Zoo  1 of 12"),
            "the collapsed row lost the count with the header"
        );
    }

    /// **A group of one is not a level** — #135, and the whole issue.
    ///
    /// An album row that opens onto exactly one track costs a keystroke and
    /// tells you nothing. On a library of singles that is nearly every row:
    /// 434 artists, 367 of them one track deep, three levels of navigation to
    /// reach one file.
    #[test]
    fn an_album_of_one_track_is_not_drawn() {
        let store = store_with(&[
            ("/m/1.mp3", "Riton", "Fake I.D", "Fake I.D (Original Mix)"),
            ("/m/2.mp3", "Burial", "Untrue", "Archangel"),
            ("/m/3.mp3", "Burial", "Untrue", "Shell of Light"),
        ]);
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&store), true);
        let mut state = TreeState::default();
        state.open_everything(&lib);

        let rows = state.rows(&lib);
        let labels: Vec<_> = rows.iter().map(Row::label).collect();
        assert_eq!(
            labels.iter().filter(|l| l.contains("Fake I.D")).count(),
            0,
            "the one-track album still has a row of its own: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Untrue")),
            "an album with two tracks lost its row: {labels:?}"
        );

        // The track is there, at the album's indent, and it is a leaf with no
        // fold — because there is nothing left to open. Its caret column says
        // so: `·` where an album would carry `▸`, in the same column and at
        // the same width, so the row does not shift and the eye can tell a
        // thing that opens from a thing that does not.
        let alone = rows
            .iter()
            .find(|r| matches!(r, Row::Track { alone: true, .. }))
            .expect("the track that replaced its album");
        assert_eq!(alone.label(), "  · ");
        assert_eq!(
            alone.label().chars().count(),
            "  ▸ ".chars().count(),
            "the leaf marker changed the indent an album row sets"
        );
        assert!(alone.is_leaf());
        assert!(!alone.is_expanded());
    }

    /// **`h` on a track standing in for its group closes the artist.**
    ///
    /// The bug: `close` targeted `[artist, album]` for every track, and a
    /// track that replaced its album is folding something with no row (#135).
    /// Nothing moved, the cursor could not follow the fold it had just made,
    /// and `h` looked broken — on the commonest row in a library of singles,
    /// where 466 of 490 albums are groups of one.
    ///
    /// snacks' rule is that `h` on a closed thing goes up and out. Here "up"
    /// has to skip the level that was never drawn.
    #[test]
    fn closing_a_track_that_replaced_its_album_goes_up_to_the_artist() {
        let store = store_with(&[
            ("/m/1.mp3", "Riton", "Fake I.D", "Fake I.D (Original Mix)"),
            ("/m/2.mp3", "Burial", "Untrue", "Archangel"),
            ("/m/3.mp3", "Burial", "Untrue", "Shell of Light"),
        ]);
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&store), true);
        let mut state = TreeState::default();
        state.open_everything(&lib);

        let at = state
            .rows(&lib)
            .iter()
            .position(|r| matches!(r, Row::Track { alone: true, .. }))
            .expect("the track that replaced its album");
        state.selected = at;

        assert!(
            state.close(&lib),
            "`h` refused a row it should have acted on"
        );
        let row = &state.rows(&lib)[state.selected];
        assert!(
            matches!(row, Row::Artist { display, expanded: false, .. } if display == "Riton"),
            "the cursor did not land on the closed artist: {row:?}"
        );
        assert!(
            !state
                .rows(&lib)
                .iter()
                .any(|r| matches!(r, Row::Track { alone: true, .. })),
            "the artist closed and its stand-in track is still drawn"
        );
    }

    /// **And a track under a real album still closes that album**, which is
    /// the case the rule was written for — only the elided level is skipped.
    #[test]
    fn closing_an_ordinary_track_goes_up_to_its_album() {
        let store = store_with(&[
            ("/m/2.mp3", "Burial", "Untrue", "Archangel"),
            ("/m/3.mp3", "Burial", "Untrue", "Shell of Light"),
        ]);
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&store), true);
        let mut state = TreeState::default();
        state.open_everything(&lib);

        let at = state
            .rows(&lib)
            .iter()
            .position(|r| matches!(r, Row::Track { alone: false, .. }))
            .expect("a track under an album of its own");
        state.selected = at;

        assert!(state.close(&lib));
        let row = &state.rows(&lib)[state.selected];
        assert!(
            matches!(row, Row::Album { display, expanded: false, .. } if display == "Untrue"),
            "the cursor did not land on the closed album: {row:?}"
        );
    }

    /// **What the collapsed row stands in for still shows** — #135.
    ///
    /// Holding one track of an album is not the same as holding a single, and
    /// the row has to say which. `London Zoo` is information; `Fake I.D` in
    /// front of `Fake I.D (Original Mix)` is the noise being removed.
    #[test]
    fn a_collapsed_row_names_the_album_only_when_that_says_something() {
        let store = store_with(&[
            (
                "/m/1.mp3",
                "The Bug",
                "London Zoo",
                "Poison Dart (Original Mix)",
            ),
            ("/m/2.mp3", "Riton", "Fake I.D", "Fake I.D (Original Mix)"),
        ]);
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&store), true);
        let mut state = TreeState::default();
        state.open_everything(&lib);
        let rows = state.rows(&lib);

        // Asked of one artist's row, since `stands_for` is answered per row
        // and both artists here hold exactly one track.
        let of = |who: &str, title: &str| {
            rows.iter()
                .find(|r| match r {
                    Row::Track { artist, .. } => lib.artist_name(artist) == who,
                    _ => false,
                })
                .and_then(|r| stands_for(&lib, r, title))
        };
        assert_eq!(
            of("The Bug", "Poison Dart (Original Mix)").as_deref(),
            Some("London Zoo"),
            "one track of an album, and the album has nowhere else to be said"
        );
        assert_eq!(
            of("Riton", "Fake I.D (Original Mix)"),
            None,
            "the title already begins with it"
        );

        // An album that *is* drawn says its own name; nothing repeats it.
        let both = store_with(&[
            ("/m/3.mp3", "Burial", "Untrue", "Archangel"),
            ("/m/4.mp3", "Burial", "Untrue", "Shell of Light"),
        ]);
        let two = Shelf::rebuild(&crate::fixture::from_holdings(&both), true);
        let mut open = TreeState::default();
        open.open_everything(&two);
        assert!(
            open.rows(&two)
                .iter()
                .all(|r| stands_for(&two, r, "Archangel").is_none()),
            "a track under a drawn album row named it twice"
        );
    }

    /// **A collapsed album is reachable, and the cursor lands on it** — #135
    /// meeting #131. The album row a search would have found is gone, so the
    /// track that replaced it has to answer for it.
    #[test]
    fn a_search_still_reaches_an_album_that_is_not_drawn() {
        let store = store_with(&[
            (
                "/m/1.mp3",
                "The Bug",
                "London Zoo",
                "Poison Dart (Original Mix)",
            ),
            ("/m/2.mp3", "Burial", "Untrue", "Archangel"),
            ("/m/3.mp3", "Burial", "Untrue", "Shell of Light"),
        ]);
        let lib = Shelf::rebuild(&crate::fixture::from_holdings(&store), true);
        let state = TreeState::default();

        // Closed, as a search starts: the artists and nothing else.
        assert_eq!(state.rows(&lib).len(), 2);

        let all = state.all_rows(&lib);
        let found = all
            .iter()
            .find(|r| {
                matches!(r, Row::Track { .. })
                    && stands_for(&lib, r, "Poison Dart (Original Mix)").as_deref()
                        == Some("London Zoo")
            })
            .expect("nothing in the unfolded rows carries the album name");

        let mut state = state;
        state.reveal_row(&lib, found);
        let rows = state.rows(&lib);
        assert_eq!(
            mark_of(&rows[state.selected]),
            mark_of(found),
            "the cursor did not land on the row the search found"
        );
    }

    #[test]
    fn selecting_an_album_narrows_the_track_pane() {
        let (_store, lib) = fixture();
        let mut state = TreeState::default();
        state.toggle(&lib); // expand Burial

        // Artist row selected: everything by that artist.
        state.selected = 0;
        assert_eq!(state.tracks_for(&lib).len(), 4);

        // "Burial" the album: its two.
        state.selected = 1;
        assert_eq!(state.tracks_for(&lib).len(), 2);

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
        assert_eq!(state.rows(&lib).len(), 11, "+ 6 tracks");

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
        assert_eq!(state.rows(&lib).len(), 11);
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
            8,
            "Burial, its 2 albums and their 4 tracks, plus Mr. Oizo"
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
            state.selected, 9,
            "a second `][` should reach `six`, the end of the next artist — \
             row 9, because Bruno's one-track album is not a row (#135)"
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

    /// Rows 0..11, all open:
    ///
    ///     0 Anna  1 First  2 one  3 two  4 three  5 Second  6 four  7 five
    ///     8 Bruno  9 six  10 Cleo  11 seven
    ///
    /// **Bruno and Cleo hold one album of one track each, so neither album is
    /// drawn** (#135) — their tracks sit where the album rows were. That is
    /// deliberate here: the motions have to walk a collapsed row like any
    /// other, and this fixture is where they meet one.
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
