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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
        /// **The record this line stands in for**, where its group is not drawn (#135).
        ///
        /// Here rather than on the reply wrapper, because it is display like the title beside
        /// it: a record held as a single track has no album line, so the name has nowhere to be
        /// written except here — and a client re-deriving that rule is a second copy of it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stands_for: Option<String>,
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

    /// **Where this line sits in the fold tree** — the path a toggle opens or closes.
    ///
    /// Not the same as its identity: a track's seat is its *container*, because a track has no
    /// fold of its own. Two of them are exceptions worth reading twice — a track that replaced
    /// its album row (#135) seats on the artist, since that album's fold is not a place any
    /// more, and a track under a drawn disc header seats on the disc, so `h` from disc 2 shuts
    /// disc 2 rather than the whole record.
    pub fn seat(&self) -> Vec<SortKey> {
        match self {
            Node::Artist { key, .. } => vec![key.clone()],
            Node::Album { artist, key, .. } => vec![artist.clone(), key.clone()],
            Node::Disc {
                artist, album, key, ..
            } => vec![artist.clone(), album.clone(), key.clone()],
            Node::Track {
                artist,
                alone: true,
                ..
            } => vec![artist.clone()],
            Node::Track {
                artist,
                album,
                disc: Some(disc),
                ..
            } => vec![artist.clone(), album.clone(), disc.clone()],
            Node::Track { artist, album, .. } => vec![artist.clone(), album.clone()],
        }
    }

    /// **What names this line across a rebuild** — its seat, plus its id if it is a track.
    ///
    /// What a paged reply anchors on: the folds changed, so the rows moved; find this again
    /// and the cursor is where the person left it.
    pub fn mark(&self) -> Mark {
        match self {
            Node::Track { id, .. } => (self.seat(), Some(*id)),
            _ => (self.seat(), None),
        }
    }

    /// **Every fold that must be open for this line to be on screen.**
    ///
    /// An artist needs none — level 0 shows them. An album needs its artist. A track needs
    /// both, which is why this is a list and not a parent link — **and its disc where it has
    /// one** (#169), or revealing a track on disc 2 opens the record and stops at the header.
    pub fn ancestors(&self) -> Vec<Vec<SortKey>> {
        match self {
            Node::Artist { .. } => Vec::new(),
            Node::Album { artist, .. } => vec![vec![artist.clone()]],
            Node::Disc { artist, album, .. } => {
                vec![vec![artist.clone()], vec![artist.clone(), album.clone()]]
            }
            Node::Track {
                artist,
                album,
                disc: Some(disc),
                ..
            } => vec![
                vec![artist.clone()],
                vec![artist.clone(), album.clone()],
                vec![artist.clone(), album.clone(), disc.clone()],
            ],
            Node::Track { artist, album, .. } => {
                vec![vec![artist.clone()], vec![artist.clone(), album.clone()]]
            }
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
    if lib.levels().is_flat() {
        // **No grouping, so every line is a track** — which is what view 2 is. A flat list is
        // not a tree with everything open; it is a tree with no levels to open, so the folds
        // have nothing to say about it.
        //
        // **The shelf is asked rather than the caller told** — #261. This was a parameter and
        // was dropped twice in one afternoon, because the code holding a shelf is not always
        // the code that chose its levels, and a grouped tree answered to a flat question looks
        // exactly like a correct one until you count the rows. `Shelf` has known its own levels
        // since #97 and nothing had asked it.
        return lib
            .sorted()
            .iter()
            .map(|id| {
                // **No path, because there is no tree to have one in.** `alone` is true for
                // the same reason: nothing groups it, so nothing could be opened to reveal it.
                let nowhere = crate::model::sortkey::sort_key("", false);
                track(lib, &nowhere, &nowhere, None, *id, true)
            })
            .collect();
    }
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
    let title = line.map(|l| l.title.clone()).unwrap_or_default();
    Node::Track {
        stands_for: alone.then(|| named(lib, artist, album, &title)).flatten(),
        artist: artist.clone(),
        album: album.clone(),
        disc,
        id,
        alone,
        title,
        credit: line.and_then(|l| l.credit.clone()),
        length_ms: line.map_or(0, |l| l.length_ms),
        number: lib.disc_of(id),
    }
}

/// One line as it goes over the wire: the node, plus the seat a client anchors and toggles by.
///
/// **The seat is sent rather than derived**, because [`Node::seat`] has three exceptions a
/// client would otherwise have to know — a track that replaced its album row seats on the
/// artist (#135), a track under a drawn disc header seats on the disc (#169), and every other
/// track seats on its album. A client re-deriving those is a second copy of a rule, which is
/// how two answers to one question start.
#[derive(Debug, serde::Serialize)]
pub struct Drawn<'a> {
    #[serde(flatten)]
    pub node: &'a Node,
    pub seat: Vec<SortKey>,
}

impl<'a> Drawn<'a> {
    /// One line as a client sees it, with the two things only the library can say.
    pub fn of(node: &'a Node) -> Self {
        Drawn {
            node,
            seat: node.seat(),
        }
    }
}

/// **What this line is searched by** — #131, #135.
///
/// A track searches its **title and nothing else**. A flat list has to search the artist and
/// the album too, because a row there is the only place they appear; the tree writes them on
/// the lines *above*, so searching them here would make `/malibu` match the album line and
/// every track under it, and `n` would then walk ten lines that matched for a reason nothing on
/// them shows.
///
/// **And the album it stands in for, when it draws one** (#135). The rule above is "do not
/// search what is written on the line above"; a group of one has no line above, so the name is
/// written here instead — and searched here, because a haystack that is not drawn text is a
/// match that paints nothing.
///
/// Every other line searches its label, which is what it draws.
pub fn haystack(lib: &Shelf, node: &Node) -> crate::text::Haystack {
    match node {
        Node::Track { title, .. } => match stands_for(lib, node, title) {
            Some(album) => crate::text::Haystack::of(&format!("{album} — {title}")),
            None => crate::text::Haystack::of(title),
        },
        other => crate::text::Haystack::of(&other.label()),
    }
}

/// **Open the way to a line, and say where it lands** — #261, `tree-over-the-wire.md` §4.
///
/// Returns the folds that have to be opened and the index the target then sits at. **The index
/// is measured after the reveal, not before**: opening a fold inserts every line under it, so
/// an index taken against the unfolded list is wrong by the time a client could use it.
///
/// `from` is where a repeated `n` resumes; `backwards` is `N`. A pattern that matches nothing,
/// or an id the shelf does not hold, is `None` — there is nothing to open and nowhere to go.
///
/// **`from` and `at` are both where the cursor is on screen**, and that is not a detail: the
/// search runs over the unfolded tree while a cursor counts visible lines, so the two are
/// different coordinate systems, and a reply fed straight back as the next request lands in the
/// wrong one — `n` then finds the same line for ever. The conversion happens here, by name
/// rather than by index, because a name survives a fold change and a number does not. That is
/// #131's lesson about "the coordinates the search runs in", one layer down.
pub fn reveal(
    lib: &Shelf,
    folds: &Expansion<SortKey>,
    want: &Target,
    from: usize,
) -> Option<Found> {
    // **Searched unfolded, because a closed fold still holds its lines** — exactly as a closed
    // fold in vim still holds its lines. Searching what is visible with the artists shut finds
    // nothing at all, because the line it wants does not exist yet.
    let all = flatten(lib, folds, true);

    // Where the cursor is, in the coordinates the search runs in. A cursor at the head is the
    // head of both lists, so the common case pays no second flatten.
    let from = if from == 0 {
        0
    } else {
        flatten(lib, folds, false)
            .get(from)
            .map(|here| here.mark())
            .and_then(|mark| all.iter().position(|n| n.mark() == mark))
            .unwrap_or(0)
    };

    let hit = match want {
        Target::Id(id) => all
            .iter()
            .position(|n| matches!(n, Node::Track { id: this, .. } if this == id)),
        Target::Pattern { needle, backwards } => {
            let needle = crate::text::Needle::new(needle);
            if needle.is_empty() {
                return None;
            }
            let hits = |i: &usize| haystack(lib, &all[*i]).contains(&needle);
            if *backwards {
                // **`N` wraps too**, the other way. An asymmetry here would be invisible with
                // two matches and maddening with twenty.
                (0..from.min(all.len()))
                    .rev()
                    .find(hits)
                    .or_else(|| (from.min(all.len())..all.len()).rev().find(hits))
            } else {
                (from + 1..all.len())
                    .find(hits)
                    // **Wraps**, because `/` does: a search that stopped at the end would make
                    // the last match in a library unreachable from anywhere below it.
                    .or_else(|| (0..=from.min(all.len().saturating_sub(1))).find(hits))
            }
        }
    }?;

    let open = all[hit].ancestors();
    let mut opened = folds.clone();
    for path in &open {
        opened.open(path);
    }
    let mark = all[hit].mark();
    let at = flatten(lib, &opened, false)
        .iter()
        .position(|n| n.mark() == mark)?;
    Some(Found { open, at })
}

/// What [`reveal`] is looking for.
pub enum Target {
    /// A track by its id — what `reveal` on a now-playing row asks.
    Id(CopyId),
    /// Text, as `/` and `?` ask it.
    Pattern { needle: String, backwards: bool },
}

/// Where a reveal landed, and what had to open for it to.
pub struct Found {
    /// Fold paths to open, in no particular order — a client applies all of them.
    pub open: Vec<Vec<SortKey>>,
    /// The index **after** those are applied.
    pub at: usize,
}

/// **How a view asks for a tree** — #261, `tree-over-the-wire.md`.
///
/// Three questions, which is all `ui::tree` ever asked a `Shelf`: what lines are visible under
/// these folds, what tracks sit under this line, and what is left to open under this path. A
/// view that names this instead of a `Shelf` is one implementation away from not holding a
/// library at all, which is the whole point — `Context::player`'s `dyn Transport` is the same
/// move, made for the same reason (#219).
///
/// **`lines` returns all of them, and that is where this is not finished.** A page is what a
/// view should hold; this is the seam, not yet the paging.
pub trait Browsing {
    /// The lines visible under `folds`, or every line when `unfolded`.
    ///
    /// **Nothing in the program calls this any more** — #261. The renderer draws a window and
    /// the search asks [`Browsing::find`], which were its last two callers.
    ///
    /// It stays because it is the **oracle**: the tests check the rules a page and a search
    /// have to obey — a closed fold still holds its lines, a reveal lands where the revealed
    /// tree puts it — against a list they can see, and a claim about a page is only checkable
    /// against the whole it came from.
    fn lines(&self, folds: &Expansion<SortKey>, unfolded: bool) -> Vec<Node>;

    /// **The shape of the whole tree under these folds** — four bits a line.
    ///
    /// What every structural motion reads, and all it reads. 8.9 KB for a library whose shelf
    /// is 2.7 MB, so a client can hold it and move the cursor without asking anyone.
    fn spine(&self, folds: &Expansion<SortKey>) -> Spine;

    /// `limit` lines from `at`. Fewer at the end, none past it.
    fn page(&self, folds: &Expansion<SortKey>, at: usize, limit: usize) -> Vec<Node>;

    /// Where this line is now — the question a fold change asks.
    fn at(&self, folds: &Expansion<SortKey>, mark: &Mark) -> Option<usize>;

    /// **Where the cursor goes when its line may no longer exist.**
    ///
    /// A level change can close the row out from under you. vim puts the cursor on the start of
    /// the fold that swallowed it, which here means the deepest visible line whose seat is a
    /// prefix of where you were — asked by seat rather than by index, because the indices are
    /// exactly what the change invalidated.
    fn settle(&self, folds: &Expansion<SortKey>, seat: &[SortKey]) -> usize;

    /// Every track under one line: one track, one disc, one record, or an artist's whole shelf.
    fn tracks_under(&self, node: &Node) -> Vec<CopyId>;

    /// Every openable path strictly under `path`.
    fn openable_under(&self, path: &[SortKey]) -> Vec<Vec<SortKey>>;

    /// Open the way to a line and say where it lands — see [`reveal`].
    fn find(&self, folds: &Expansion<SortKey>, want: &Target, from: usize) -> Option<Found>;
}

/// **Everything a view holds a library for** — #261.
///
/// [`Browsing`] is the tree; this adds what view 2 and the status line ask, which is the flat
/// ordering and four counts. Together they are the whole of it: a view that names this names no
/// projection, and the one implementation is a `Shelf` until a socket is the other.
///
/// **`sorted` lends rather than gives**, so a remote implementation has to keep the ordering it
/// was told rather than fetch it per frame — which is right: 7,273 ids is 116 KB and the flat
/// list scrolls through all of them.
pub trait Holding: Browsing {
    fn sorted(&self) -> &[CopyId];
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
    fn artist_count(&self) -> usize;
    fn album_count(&self) -> usize;

    /// The album a track stands in for, where its group is not drawn — see [`stands_for`].
    fn standing_for(&self, row: &Node, title: &str) -> Option<String>;

    /// Which artist and record a copy is filed under.
    ///
    /// **Owned**, because an implementation that got this answer over a socket has nothing to
    /// lend it from — the one place in this trait where `sorted`'s trick does not work, since a
    /// lookup by id cannot be cached the way one ordering can.
    fn locate(&self, id: CopyId) -> Option<(SortKey, SortKey)>;
    /// Every track filed under one artist.
    fn artist_tracks(&self, artist: &SortKey) -> Vec<CopyId>;
    /// Every track on one record.
    fn album_tracks(&self, artist: &SortKey, album: &SortKey) -> Vec<CopyId>;
}

/// An empty shelf, so a caller can move one out of a field and put it back.
///
/// `Box` is fundamental, so this is a local impl rather than an orphan.
impl Default for Box<dyn Holding> {
    fn default() -> Self {
        Box::new(Shelf::default())
    }
}

/// **Forwarded through the box**, so a caller holding one can pass `&it` where `&dyn Browsing`
/// is wanted.
///
/// Without this every call site would have to write `&*ui.library` — fifty reborrows saying
/// nothing, in a codebase where the whole point of the seam is that asking the library reads
/// the same however the answer arrives.
impl Browsing for Box<dyn Holding> {
    fn lines(&self, folds: &Expansion<SortKey>, unfolded: bool) -> Vec<Node> {
        (**self).lines(folds, unfolded)
    }
    fn spine(&self, folds: &Expansion<SortKey>) -> Spine {
        (**self).spine(folds)
    }
    fn page(&self, folds: &Expansion<SortKey>, at: usize, limit: usize) -> Vec<Node> {
        (**self).page(folds, at, limit)
    }
    fn at(&self, folds: &Expansion<SortKey>, mark: &Mark) -> Option<usize> {
        (**self).at(folds, mark)
    }
    fn settle(&self, folds: &Expansion<SortKey>, seat: &[SortKey]) -> usize {
        (**self).settle(folds, seat)
    }
    fn tracks_under(&self, node: &Node) -> Vec<CopyId> {
        (**self).tracks_under(node)
    }
    fn openable_under(&self, path: &[SortKey]) -> Vec<Vec<SortKey>> {
        (**self).openable_under(path)
    }
    fn find(&self, folds: &Expansion<SortKey>, want: &Target, from: usize) -> Option<Found> {
        (**self).find(folds, want, from)
    }
}

impl Holding for Box<dyn Holding> {
    fn sorted(&self) -> &[CopyId] {
        (**self).sorted()
    }
    fn len(&self) -> usize {
        (**self).len()
    }
    fn is_empty(&self) -> bool {
        (**self).is_empty()
    }
    fn artist_count(&self) -> usize {
        (**self).artist_count()
    }
    fn album_count(&self) -> usize {
        (**self).album_count()
    }
    fn standing_for(&self, row: &Node, title: &str) -> Option<String> {
        (**self).standing_for(row, title)
    }
    fn locate(&self, id: CopyId) -> Option<(SortKey, SortKey)> {
        (**self).locate(id)
    }
    fn artist_tracks(&self, artist: &SortKey) -> Vec<CopyId> {
        (**self).artist_tracks(artist)
    }
    fn album_tracks(&self, artist: &SortKey, album: &SortKey) -> Vec<CopyId> {
        (**self).album_tracks(artist, album)
    }
}

impl Holding for Shelf {
    fn sorted(&self) -> &[CopyId] {
        Shelf::sorted(self)
    }
    fn len(&self) -> usize {
        Shelf::len(self)
    }
    fn is_empty(&self) -> bool {
        Shelf::is_empty(self)
    }
    fn artist_count(&self) -> usize {
        Shelf::artist_count(self)
    }
    fn album_count(&self) -> usize {
        Shelf::album_count(self)
    }
    fn standing_for(&self, row: &Node, title: &str) -> Option<String> {
        stands_for(self, row, title)
    }
    fn locate(&self, id: CopyId) -> Option<(SortKey, SortKey)> {
        Shelf::locate(self, id).map(|(a, b)| (a.clone(), b.clone()))
    }
    fn artist_tracks(&self, artist: &SortKey) -> Vec<CopyId> {
        Shelf::artist_tracks(self, artist)
    }
    fn album_tracks(&self, artist: &SortKey, album: &SortKey) -> Vec<CopyId> {
        Shelf::album_tracks(self, artist, album).to_vec()
    }
}

impl Browsing for Shelf {
    fn lines(&self, folds: &Expansion<SortKey>, unfolded: bool) -> Vec<Node> {
        flatten(self, folds, unfolded)
    }

    fn spine(&self, folds: &Expansion<SortKey>) -> Spine {
        spine_of(&flatten(self, folds, false))
    }

    fn page(&self, folds: &Expansion<SortKey>, at: usize, limit: usize) -> Vec<Node> {
        flatten(self, folds, false)
            .into_iter()
            .skip(at)
            .take(limit)
            .collect()
    }

    fn at(&self, folds: &Expansion<SortKey>, mark: &Mark) -> Option<usize> {
        flatten(self, folds, false)
            .iter()
            .position(|n| &n.mark() == mark)
    }

    fn settle(&self, folds: &Expansion<SortKey>, seat: &[SortKey]) -> usize {
        let lines = flatten(self, folds, false);
        let mut best = 0;
        let mut deepest = 0;
        for (i, line) in lines.iter().enumerate() {
            let at = line.seat();
            if at.len() <= seat.len() && seat.starts_with(&at) && at.len() >= deepest {
                deepest = at.len();
                best = i;
            }
        }
        best.min(lines.len().saturating_sub(1))
    }

    /// What `a` and `e` add, and what `x` plays the first of.
    fn tracks_under(&self, node: &Node) -> Vec<CopyId> {
        match node {
            Node::Artist { key, .. } => self
                .artists()
                .find(|(k, _)| *k == key)
                .map(|(_, albums)| albums.values().flatten().copied().collect())
                .unwrap_or_default(),
            Node::Album { artist, key, .. } => self
                .artists()
                .find(|(k, _)| *k == artist)
                .and_then(|(_, albums)| albums.get(key))
                .cloned()
                .unwrap_or_default(),
            // A disc plays the disc, which is what `x` on one should mean.
            Node::Disc {
                artist, album, key, ..
            } => self
                .artists()
                .find(|(k, _)| *k == artist)
                .and_then(|(_, albums)| albums.get(album))
                .map(|tracks| {
                    tracks
                        .iter()
                        .copied()
                        .filter(|id| {
                            self.disc_of(*id).is_some_and(|n| {
                                crate::model::sortkey::sort_key(&format!("{n:03}"), false) == *key
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Node::Track { id, .. } => vec![*id],
        }
    }

    fn find(&self, folds: &Expansion<SortKey>, want: &Target, from: usize) -> Option<Found> {
        reveal(self, folds, want, from)
    }

    /// Only albums can be under anything, so this is one level deep by construction.
    fn openable_under(&self, path: &[SortKey]) -> Vec<Vec<SortKey>> {
        let [artist] = path else { return Vec::new() };
        self.artists()
            .find(|(k, _)| *k == artist)
            .map(|(_, albums)| {
                albums
                    .iter()
                    // **A group of one has no fold to open** (#135). Reporting one would make
                    // `zR` believe there was something still shut, on a row that is not drawn.
                    .filter(|(_, tracks)| tracks.len() > 1)
                    .map(|(album, _)| vec![artist.clone(), album.clone()])
                    .collect()
            })
            .unwrap_or_default()
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

/// **The album a track stands in for, when its group is not drawn** — #135, #137.
///
/// A record held as a single track has no album line, so the name has nowhere to be written
/// except on the track. `None` when there is a line above to carry it, when the name would
/// merely repeat the title, and when nothing counted the record.
pub fn stands_for(lib: &Shelf, row: &Node, title: &str) -> Option<String> {
    let Node::Track {
        artist,
        album,
        alone: true,
        ..
    } = row
    else {
        return None;
    };
    named(lib, artist, album, title)
}

/// The rule itself, so the flatten and [`stands_for`] cannot come to disagree about it.
fn named(lib: &Shelf, artist: &SortKey, album: &SortKey, title: &str) -> Option<String> {
    let name = lib.album_name(artist, album);
    // **The count comes even when the name does not** (#137). One track of twelve is exactly
    // the row somebody wants flagged, and it is the row #135 took the album header away from.
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
mod tests {
    use super::*;
    use crate::fixture::Tagged as Track;
    use crate::operations::scan::Holdings as Scan;
    use std::path::PathBuf;

    /// Three artists and a record over two discs — the shapes a reveal has to get right.
    ///
    /// **More than one artist, because one cannot show the bug.** A reveal measured against the
    /// unfolded list and one measured against the revealed list agree whenever everything above
    /// the target is open, and with a single artist there is nothing above it to be shut.
    ///
    /// **Three, because two cannot show the other one.** With two matches, a forward search
    /// that wraps and a backward search land on the same line, so `N` could be ignored entirely
    /// and nothing would notice.
    fn library() -> Shelf {
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
        add("/m/a1.mp3", "Aaa Band", "First", "Opener", None);
        add("/m/a2.mp3", "Aaa Band", "First", "Second Track", None);
        add("/m/m1.mp3", "Mmm Band", "Middle", "In Between", None);
        add("/m/m2.mp3", "Mmm Band", "Middle", "Also Here", None);
        add(
            "/m/z1.mp3",
            "Zzz Band",
            "Later",
            "Back in the USSR",
            Some(1),
        );
        add("/m/z2.mp3", "Zzz Band", "Later", "Dear Prudence", Some(1));
        add("/m/z3.mp3", "Zzz Band", "Later", "Birthday", Some(2));
        add("/m/z4.mp3", "Zzz Band", "Later", "Yer Blues", Some(2));
        Shelf::rebuild(&crate::fixture::from_holdings(&s), true)
    }

    fn shut() -> Expansion<SortKey> {
        let mut folds = Expansion::default();
        folds.opening_from(2);
        folds
    }

    /// **The index comes back measured after the reveal, not before** — #261.
    ///
    /// Opening a fold inserts every line under it, so the two differ by however much was
    /// revealed above the target. Here the target is under the *second* artist and the first
    /// stays shut, so an index taken against the unfolded tree overshoots by that artist's
    /// whole record — which is exactly the stale cursor the design says not to send.
    #[test]
    fn a_reveal_lands_where_the_revealed_tree_puts_it() {
        let lib = library();
        let folds = shut();
        let target = Target::Pattern {
            needle: "Yer Blues".into(),
            backwards: false,
        };
        let found = reveal(&lib, &folds, &target, 0).expect("a track that is in the library");

        let unfolded = flatten(&lib, &folds, true);
        let before = unfolded
            .iter()
            .position(|n| matches!(n, Node::Track { title, .. } if title == "Yer Blues"))
            .expect("it is in the unfolded tree");
        assert_ne!(
            found.at, before,
            "the revealed index equals the unfolded one, so this fixture cannot tell them apart"
        );

        let mut opened = folds.clone();
        for path in &found.open {
            opened.open(path);
        }
        let after = flatten(&lib, &opened, false);
        assert!(after.len() < unfolded.len(), "the reveal opened everything");
        assert!(
            matches!(&after[found.at], Node::Track { title, .. } if title == "Yer Blues"),
            "landed on {:?}",
            after.get(found.at).map(Node::label)
        );
    }

    /// **A track on disc 2 needs its disc opened, not just its record** — #169.
    ///
    /// Revealing it without that opens the album and stops at the disc header, which looks like
    /// a reveal that worked until you notice the cursor is on a line you did not search for.
    #[test]
    fn revealing_a_track_on_a_second_disc_opens_that_disc() {
        let lib = library();
        let folds = {
            // The disc level opens by itself (#169), so it is shut by hand here — otherwise
            // this passes whether or not `ancestors` names the disc.
            let mut f = Expansion::default();
            f.close(&[
                crate::model::sortkey::sort_key("zzz band", true),
                crate::model::sortkey::sort_key("later", false),
                crate::model::sortkey::sort_key("002", false),
            ]);
            f
        };
        let target = Target::Pattern {
            needle: "Yer Blues".into(),
            backwards: false,
        };
        let found = reveal(&lib, &folds, &target, 0).expect("it is in the library");
        assert!(
            found.open.iter().any(|p| p.len() == 3),
            "nothing opened the disc; opened {:?}",
            found.open
        );
    }

    /// **`n` moves on, then comes back round** — and the cursor it takes is the one on screen.
    ///
    /// The search runs unfolded and a cursor counts visible lines, so feeding a reply straight
    /// back as the next request is a coordinate mistake — and a silent one: `n` simply finds
    /// the same line for ever. This walks the cycle, which is the only way to see it.
    #[test]
    fn a_repeated_search_moves_on_and_then_comes_round() {
        let lib = library();
        let folds = Expansion::default();
        let band = |from| {
            reveal(
                &lib,
                &folds,
                &Target::Pattern {
                    needle: "Band".into(),
                    backwards: false,
                },
                from,
            )
            .map(|f| f.at)
        };

        // The instrument: one match would make "moves on" and "comes round" the same answer.
        let matches = flatten(&lib, &folds, true)
            .iter()
            .filter(|n| haystack(&lib, n).contains(&crate::text::Needle::new("Band")))
            .count();
        assert!(
            matches >= 2,
            "only {matches} lines match, so a cycle of one proves nothing"
        );

        // Walk the whole cycle: each `n` must land somewhere new until the last, which comes
        // back to the start. A search that stood still and one that wrapped early both fail.
        let first = band(0).expect("a line carries the word");
        let mut seen = vec![first];
        let mut at = first;
        for _ in 1..matches {
            at = band(at).expect("another line carries it");
            assert!(
                !seen.contains(&at),
                "`n` came back to {at} after {seen:?} with matches still ahead"
            );
            seen.push(at);
        }
        assert_eq!(seen.len(), matches, "`n` did not visit every match");
        assert_eq!(
            band(at),
            Some(first),
            "`n` past the last match did not come round to {first}"
        );
    }

    /// `N` looks the other way.
    #[test]
    fn a_backwards_search_goes_the_other_way() {
        let lib = library();
        let folds = Expansion::default();
        let look = |backwards, from| {
            reveal(
                &lib,
                &folds,
                &Target::Pattern {
                    needle: "Band".into(),
                    backwards,
                },
                from,
            )
            .map(|f| f.at)
        };
        let ahead = look(false, 0).expect("something ahead");
        assert!(ahead > 0, "a forward search stayed at the head");
        let onwards = look(false, ahead).expect("and something after that");
        let back = look(true, ahead).expect("and something behind");
        assert_ne!(
            onwards, back,
            "`n` and `N` from {ahead} both landed on {back}, so this cannot see the direction"
        );
        assert!(back < ahead, "`N` from {ahead} went forwards to {back}");
        assert!(
            onwards > ahead,
            "`n` from {ahead} went backwards to {onwards}"
        );

        // **And `N` from the head comes round to the last match**, the mirror of `n` wrapping
        // at the end. Without this, "backwards never wraps" passes every assertion above,
        // because each of them starts somewhere with a match already behind it.
        assert_eq!(
            look(true, 0),
            Some(onwards),
            "`N` from the head did not come round to the last match at {onwards}"
        );
    }
}
