//! **The browse tree, as the librarian computes it** — #261, §7.11.
//!
//! `Shelf::grouped` is the library's own grouping, and the TUI has been calling it with a
//! `Librarian` of its own — which is why a view holds a fold at all. The grouping is a library
//! question; only the *folding* (which nodes a person has opened) is a view's.
//!
//! So this answers the question and leaves the folding alone: a flat, ordered list of nodes,
//! which a client pages through and opens as it likes.
//!
//! ## Why this is not in `query/`
//!
//! It builds a `Shelf`, and `crate::shelf` sits *above* `crate::query` — a layer may call the
//! one beneath it and nothing may reach up. `the_query_layer_does_not_know_the_player_exists`
//! said so the moment this was written there, which is the direction rule doing its job.
//!
//! ## Why a list and not a `Shelf`
//!
//! A `Shelf` is `BTreeMap<(SortKey, SortKey), …>` in four places, and a tuple key is not a JSON
//! object key. Flattening is not a concession to the wire — the tree *is* drawn as a list, and
//! `ui::tree::flatten` already turns one into exactly this shape.

use crate::fold::Expansion;
use crate::model::rows::CopyId;
use crate::model::seam::Librarian;
use crate::shelf::{Levels, Shelf, SortKey};

/// **What identifies a line across a rebuild** — a container by its path, a track by its id.
///
/// Named because two methods and a search all pass it around (#131), and because it is what a
/// paged reply anchors on: the rows moved, so where did my cursor go?
pub type Mark = (Vec<SortKey>, Option<CopyId>);

/// One line of the tree.
///
/// **Ordered, and the order is the tree's.** A client draws them in sequence and indents by
/// [`Node::depth`]; an artist is followed by its albums and each album by its tracks.
///
/// **Four variants, not three** — #261. This was `Outer / Inner / Track`, which is not what a
/// tree of this library draws: a record spanning discs grows a header between the album and
/// its tracks (#169), and a record held as a single track is drawn *in place of* its album row
/// (#135). Leaving those to the client would mean every client re-deriving rows the library
/// already knows about — two answers to one question, which is what #257 exists to prevent.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Node {
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
        /// **What a client draws**, so a page is enough and the view need not go back to the
        /// library for every visible line — #261. The keys above are identity; these are the
        /// row. Empty or zero where the library cannot draw it, which is a row it would have
        /// skipped before.
        title: String,
        /// The track's own credit, which is not always who it files under.
        credit: Option<String>,
        length_ms: u64,
        /// Which disc of its record, for the row's own numbering — the `disc` key above is the
        /// fold path, and a track can carry a number without a header being drawn.
        number: Option<u16>,
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

impl Node {
    /// The marker and indent. A track's *text* comes from the format template,
    /// so it is rendered rather than labelled — a row beside an artist row
    /// needs to look different, and the template is how that is said.
    pub fn label(&self) -> String {
        match self {
            Node::Artist {
                display, expanded, ..
            } => format!("{} {display}", if *expanded { "▾" } else { "▸" }),
            Node::Album {
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
            Node::Disc {
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
            Node::Track { alone, disc, .. } => match (alone, disc) {
                (true, _) => "  · ",
                (false, None) => "    · ",
                (false, Some(_)) => "      · ",
            }
            .to_string(),
        }
    }

    /// Whether this row can be opened or closed at all.
    /// How far it indents — and the only depths a `Shelf` can produce.
    ///
    /// A track sits at 3 **only under a drawn disc header**: a single-disc record has no header
    /// and its tracks sit at 2 beside one, and a record held as one track (#135) sits at 2
    /// where its album row would have been. Depth alone therefore cannot say whether a line is
    /// a leaf, which is why [`Spine`] spends a bit on it.
    pub fn depth(&self) -> usize {
        match self {
            Node::Artist { .. } => 0,
            Node::Album { .. } => 1,
            Node::Disc { .. } => 2,
            Node::Track { disc: Some(_), .. } => 3,
            Node::Track { .. } => 2,
        }
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self, Node::Track { .. })
    }

    /// Whether this row is a container that is currently open. A leaf is
    /// neither open nor closed, and answers `false`.
    pub fn is_expanded(&self) -> bool {
        match self {
            Node::Artist { expanded, .. }
            | Node::Album { expanded, .. }
            | Node::Disc { expanded, .. } => *expanded,
            Node::Track { .. } => false,
        }
    }
}

/// The tree over `members`, grouped by `levels`.
///
/// **Everything open.** Which nodes a person has folded is the view's state and the librarian
/// has no business holding it — so this returns the whole shape and a client draws what it
/// wants. `tree-view-design.md`'s search reads the unfolded list for the same reason.
/// The tree over `members`, grouped by `levels`, with everything open.
///
/// What a client that keeps no fold state asks for. A view that does keeps one and calls
/// [`flatten`] — `tree-over-the-wire.md` §2: the grouping is 370 ms and the flatten is 1.8 ms,
/// so the shelf is what gets kept and the folds travel with the request.
pub fn tree(
    library: &Librarian,
    members: std::collections::HashSet<CopyId>,
    levels: Levels,
    smart_artist_sort: bool,
) -> Vec<Node> {
    let shelf = Shelf::grouped(library, members, levels, smart_artist_sort);
    if levels.is_flat() {
        // **No grouping, so every line is a track** — which is what view 2 is. It has no folds
        // by construction, so it does not go through [`flatten`]: a flat list of tracks is not
        // a tree with everything open, it is a tree with no levels to open.
        return shelf
            .sorted()
            .iter()
            .map(|id| {
                // **No path, because there is no tree to have one in.** `alone` is true for
                // the same reason: nothing groups it, so nothing could be opened to reveal it.
                let nowhere = crate::model::sortkey::sort_key("", false);
                track(&shelf, &nowhere, &nowhere, None, *id, true)
            })
            .collect();
    }
    flatten(&shelf, &Expansion::default(), true)
}

/// The visible lines of a shelf, honouring `folds` — or every line when `unfolded`.
///
/// **`unfolded` is what `/` searches.** A closed fold still holds its rows, exactly as a closed
/// fold in vim still holds its lines: searching the visible list with the artists shut finds
/// nothing at all, because the album row it wants does not exist yet.
///
/// Display names come from the shelf and the tracks, since §4 keeps display strings on the
/// track and only sort keys in the tree.
pub fn flatten(lib: &Shelf, folds: &Expansion<SortKey>, unfolded: bool) -> Vec<Node> {
    let mut rows = Vec::new();
    for (artist_key, albums) in lib.artists() {
        let display = lib.artist_name(artist_key).to_string();
        let expanded = unfolded || folds.is_open(std::slice::from_ref(artist_key));
        rows.push(Node::Artist {
            key: artist_key.clone(),
            display,
            albums: albums.len(),
            expanded,
        });
        if !expanded {
            continue;
        }
        for (album_key, tracks) in albums {
            // **A group of one is not a level** (#135). The album row would open onto exactly
            // this track, so the track takes its place — and there is no fold, because there is
            // nothing left to open.
            //
            // Applied in both listings, unfolded included: a level that is not drawn does not
            // exist to be revealed, and a search that found one could not put the cursor
            // anywhere.
            if let [only] = tracks[..] {
                rows.push(track(lib, artist_key, album_key, None, only, true));
                continue;
            }
            let display = lib.album_name(artist_key, album_key).to_string();
            let open = unfolded || folds.is_open(&[artist_key.clone(), album_key.clone()]);
            rows.push(Node::Album {
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
            // **A disc is a level only when there is more than one** — #169, and #135's rule
            // one level up: a header that opens onto everything beneath it costs a keystroke
            // and says nothing. Measured, 1,003 of this library's 1,029 albums hold their
            // copies on one disc and grow no row at all.
            if lib.album_discs(artist_key, album_key) < 2 {
                for id in tracks {
                    rows.push(track(lib, artist_key, album_key, None, *id, false));
                }
                continue;
            }
            // The tracks arrive sorted by `(disc, track)` already, so grouping is a run over
            // them rather than a re-sort — and the header cannot disagree with the order for
            // the same reason.
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
                    // A copy nothing placed has no disc to sit under, so it stays where an
                    // ordinary track would.
                    for id in run {
                        rows.push(track(lib, artist_key, album_key, None, *id, false));
                    }
                    continue;
                };
                // Zero-padded, because a `SortKey` orders as text and `disc 10` would otherwise
                // sit between 1 and 2.
                let key = crate::model::sortkey::sort_key(&format!("{number:03}"), false);
                let path = [artist_key.clone(), album_key.clone(), key.clone()];
                let disc_open = unfolded || folds.is_open(&path);
                rows.push(Node::Disc {
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
                    rows.push(track(
                        lib,
                        artist_key,
                        album_key,
                        Some(key.clone()),
                        *id,
                        false,
                    ));
                }
            }
        }
    }
    rows
}

/// One track line, carrying both its identity and what a client draws.
///
/// **Both, and that is the point** — #261. The keys are how a toggle and an anchor name it; the
/// title and the length are why a page is enough to draw from. A node that carried only
/// identity would send the view back to the library for every visible row.
fn track(
    lib: &Shelf,
    artist: &SortKey,
    album: &SortKey,
    disc: Option<SortKey>,
    id: CopyId,
    alone: bool,
) -> Node {
    let line = lib.line(id);
    Node::Track {
        artist: artist.clone(),
        album: album.clone(),
        disc,
        id,
        alone,
        title: line.map(|l| l.title.clone()).unwrap_or_default(),
        credit: line.and_then(|l| l.credit.clone()),
        length_ms: line.map_or(0, |l| l.length_ms),
        number: lib.disc_of(id),
    }
}

/// The shape of these lines, and nothing else about them — #261.
pub fn spine_of(rows: &[Node]) -> Spine {
    let mut spine = Spine::default();
    for row in rows {
        spine.push(row.depth() as u8, row.is_leaf(), row.is_expanded());
    }
    spine
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spine(Vec<u8>);

const DEPTH: u8 = 0b0000_0011;
const LEAF: u8 = 0b0000_0100;
const OPEN: u8 = 0b0000_1000;

impl Spine {
    /// Add a row.
    ///
    /// **Panics above depth 3**, which is unreachable through `Shelf` and is a bug in a
    /// caller that built the tree some other way. Clamping instead would draw the row at the
    /// wrong indent and move the cursor to the wrong place, quietly.
    pub fn push(&mut self, depth: u8, leaf: bool, expanded: bool) {
        assert!(depth <= 3, "depth {depth} cannot come from a Shelf");
        self.0
            .push(depth | if leaf { LEAF } else { 0 } | if expanded { OPEN } else { 0 });
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How far this row is indented, or `None` past the end.
    pub fn depth(&self, at: usize) -> Option<u8> {
        self.0.get(at).map(|b| b & DEPTH)
    }

    /// Whether the row holds nothing — a track.
    ///
    /// **Its own bit, because depth does not imply it.** Depth 2 is both a disc header and a
    /// track on a record with one disc, and `]z` on the first means its tracks where on the
    /// second it means its siblings.
    pub fn is_leaf(&self, at: usize) -> Option<bool> {
        self.0.get(at).map(|b| b & LEAF != 0)
    }

    /// Whether this container is open. A leaf is neither open nor closed and answers `false`.
    pub fn is_expanded(&self, at: usize) -> Option<bool> {
        self.0.get(at).map(|b| b & OPEN != 0)
    }

    /// One hex digit a row.
    ///
    /// **Inspectable on purpose.** A spine is the thing a wrong cursor position is blamed on,
    /// and `"012233"` can be read in a log line where base64 cannot.
    pub fn to_hex(&self) -> String {
        self.0
            .iter()
            .map(|b| char::from_digit(*b as u32, 16).expect("four bits"))
            .collect()
    }

    /// Back from the wire, or `None` on anything that is not hex.
    pub fn from_hex(text: &str) -> Option<Self> {
        text.chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<Vec<_>>>()
            .map(Spine)
    }
}
