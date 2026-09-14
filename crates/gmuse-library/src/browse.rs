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

use crate::model::rows::{CopyId, Listing};
use crate::model::seam::Librarian;
use crate::shelf::{Levels, Shelf};

/// One line of the tree.
///
/// **Ordered, and the order is the tree's.** A client draws them in sequence and uses `depth`
/// to indent; an artist is followed by its albums and each album by its tracks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Node {
    /// A grouping node — an artist, under the default levels.
    Outer {
        name: String,
        /// How many inner nodes it holds, so a client can draw a count without opening it.
        albums: usize,
        tracks: usize,
    },
    /// A grouping node inside an outer one — an album.
    Inner {
        name: String,
        tracks: usize,
        /// How many the record has, where the library knows and the shelf is short — #137.
        total: Option<u16>,
    },
    /// A playable row.
    Track {
        row: CopyId,
        title: String,
        /// The track's own credit, which is not always who it files under.
        artist: Option<String>,
        length_ms: u64,
        disc: Option<u16>,
    },
}

impl Node {
    /// How far to indent it. Flat levels put everything at zero.
    pub fn depth(&self) -> usize {
        match self {
            Node::Outer { .. } => 0,
            Node::Inner { .. } => 1,
            Node::Track { .. } => 2,
        }
    }
}

/// The tree over `members`, grouped by `levels`.
///
/// **Everything open.** Which nodes a person has folded is the view's state and the librarian
/// has no business holding it — so this returns the whole shape and a client draws what it
/// wants. `tree-view-design.md`'s search reads the unfolded list for the same reason.
pub fn tree(
    library: &Librarian,
    members: std::collections::HashSet<CopyId>,
    levels: Levels,
    smart_artist_sort: bool,
) -> Vec<Node> {
    let shelf = Shelf::grouped(library, members, levels, smart_artist_sort);
    flatten(library, &shelf, levels)
}

/// The nodes of a shelf already built.
///
/// **Split out to be measured** — #261. A fold state that travels with a request is only
/// affordable if this half is the cheap one.
pub fn flatten(library: &Librarian, shelf: &Shelf, levels: Levels) -> Vec<Node> {
    let mut out = Vec::new();

    if levels.is_flat() {
        // **No grouping, so every row is a track** — which is what view 2 was.
        for id in shelf.sorted() {
            if let Some(node) = track_of(library, shelf, *id) {
                out.push(node);
            }
        }
        return out;
    }

    for (outer, inners) in shelf.artists() {
        out.push(Node::Outer {
            name: shelf.artist_name(outer).to_string(),
            albums: inners.len(),
            tracks: inners.values().map(Vec::len).sum(),
        });
        for (inner, tracks) in inners {
            out.push(Node::Inner {
                name: shelf.album_name(outer, inner).to_string(),
                tracks: tracks.len(),
                total: shelf.album_total(outer, inner),
            });
            for id in tracks {
                if let Some(node) = track_of(library, shelf, *id) {
                    out.push(node);
                }
            }
        }
    }
    out
}

/// One track node, or nothing where the library cannot draw the row.
fn track_of(library: &Librarian, shelf: &Shelf, id: CopyId) -> Option<Node> {
    let listed = library.listed(id)?;
    Some(Node::Track {
        row: id,
        title: listed.title.to_owned(),
        artist: listed.artist.map(str::to_owned),
        length_ms: listed.duration.as_millis() as u64,
        disc: shelf.disc_of(id),
    })
}

/// **The shape of a tree, four bits a row** — #261, `tree-over-the-wire.md` §3.
///
/// Every structural motion — `}` `{` `]]` `[[` `][` `[]` `-` `]z` `zj` `zk` — is a scan for
/// the first row matching a predicate, and read against the navigator those predicates consume
/// exactly three fields: a row's depth, whether it is a leaf, and whether it is open. Not a
/// title, not an id, not a sort key.
///
/// So this is what a view holds instead of the tree: **8.9 KB for the library that needs
/// 1.7 MB of `Shelf`**, and `j`, `gg`, `G` and `3000j` do not read even that, being arithmetic
/// on the cursor and the total.
///
/// ## Why two bits of depth is not a guess
///
/// `Shelf::tree` is `BTreeMap<SortKey, BTreeMap<SortKey, Vec<CopyId>>>` — exactly two grouping
/// levels, in the type. With the disc row between an album and its tracks (#169) that is four
/// depths, 0 through 3, and there is no arrangement of `Levels` that produces a fifth. A
/// deeper tree would have to change `Shelf` first, and would fail the assertion here while
/// doing it rather than silently drawing at the wrong indent.
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
