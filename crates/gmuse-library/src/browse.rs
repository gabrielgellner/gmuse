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
