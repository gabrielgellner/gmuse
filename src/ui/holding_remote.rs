//! **A library the view does not have** — #261, `tree-over-the-wire.md` §7.
//!
//! The second implementation of [`Holding`]. A detached view *is* the librarian (§4.4) and folds
//! the log itself; an attached one asks the engine, which has already done it. That is the whole
//! of the 40.6 MB — a fold, a `Librarian` and a `Shelf`, held a second time in a process whose
//! librarian is somewhere else.
//!
//! ## One ask a frame, not four
//!
//! A redraw asks four questions about the same tree — its shape, its page, its counts, and where
//! the cursor is — and one `query.tree` reply carries all four. So the last reply is kept and
//! answered from whenever the question has not changed.
//!
//! **Keyed by the question**, which is the fold state and the window. A different fold state is a
//! different tree, and answering the new one from the old reply is the single way a cache like
//! this goes wrong. `remote_transport` caches for a different reason — §7.8 forbids asking at
//! all — and this one caches because asking four times for one answer would be silly.
//!
//! ## What a failed ask means
//!
//! **An empty tree, not a panic and not a stale page.** The engine going away mid-draw is an
//! ordinary thing (`gmuse ctl engine.shutdown`), and a view that unwound on it would take the
//! terminal with it. An empty pane is visibly wrong in the right way: the next event reconnects
//! and it fills again.

use std::sync::{Arc, Mutex};

/// **How many rows a miss reads ahead** — #261.
///
/// Fifty, and the table says why — `just read-cost` against the real library:
///
/// | ahead | a screenful | every row |
/// | --- | --- | --- |
/// | 1 | 9.0 ms | 1.5 s |
/// | 50 | **6.4 ms** | 574 ms |
/// | 100 | 9.2 ms | 565 ms |
/// | 250 | 25.7 ms | 610 ms |
/// | 500 | 46.6 ms | 590 ms |
///
/// **The walk is flat from 50 upwards**, which says the round trips stopped being the cost
/// somewhere below there and the rows themselves — written out at one end and read at the other
/// — are what is left. So a bigger block buys nothing on the walk and makes the first draw wait
/// for rows nobody is looking at.
///
/// It also says where the next saving is not: batching harder. A filter that walks 7,273 rows
/// to keep forty is the wrong shape however it is fetched, and §7.11's `query.run` is the
/// shape that is not.
const AHEAD: usize = 50;

/// **How many rows are kept**, and it is a cap rather than a policy — #261.
///
/// Without one this grew to hold every row a walk touched: the startup filter reads all 7,273
/// and the view went from 28.3 MB to 71.8 MB, which is trading a 26.5 MB `Librarian` for a
/// 43 MB pile of rows. Worse, and measured before it was believed.
///
/// What the cache is *for* is redrawing the same forty lines, and the panes beside them. Five
/// hundred covers that with room to scroll. A walk blows straight through it, which is the
/// right outcome: a caller reading every row once should not leave every row behind.
///
/// **Cleared rather than evicted one at a time.** An LRU would need an order to maintain on
/// every hit, for a map that is either serving a screenful — where nothing is evicted — or
/// being walked past, where nothing is worth keeping.
const KEEP: usize = 500;

use gmuse_client::Client;

use crate::browse::{Browsing, Found, Holding, Mark, Node, Spine, Target};
use crate::fold::Expansion;
use crate::model::id::Id;
use crate::model::rows::CopyId;
use crate::shelf::SortKey;

/// What one reply said, and what it was asked.
#[derive(Default)]
struct Last {
    /// The fold state and the window, which is the whole of a plain `query.tree`.
    asked: Option<(crate::fold::Parts<SortKey>, usize, usize)>,
    nodes: Vec<Node>,
    spine: Spine,
    at: usize,
    artists: usize,
    albums: usize,
}

/// **Cheap to clone, because one process needs one of these** — #261.
///
/// A view reads the library through two seams: `Holding` for the tree and the flat list,
/// `Listing` for what a row says. Both are this, and two of them would be two caches of the
/// same answers and twice the asking. So the state is behind one `Arc` and a clone is a pointer.
#[derive(Clone)]
pub struct Remote {
    inner: Arc<Inner>,
}

struct Inner {
    client: Arc<Client>,
    last: Mutex<Last>,
    /// The flat ordering, fetched once and lent thereafter.
    ///
    /// [`Holding::sorted`] lends, so this has to own it — which is the whole reason that
    /// signature is worth keeping: view 2 scrolls through all 7,273 and must not ask per frame.
    /// 116 KB against the 40.6 MB it replaces.
    sorted: Vec<CopyId>,
    /// Rows already asked for, so a redraw asks about the same forty once.
    #[allow(clippy::type_complexity)]
    rows_kept: Mutex<
        std::collections::HashMap<
            CopyId,
            (
                Option<crate::model::rows::Slip>,
                Option<crate::model::rows::Playable>,
                Option<Id<crate::model::entity::Recording>>,
            ),
        >,
    >,
}

impl Remote {
    /// Ask for the ordering once; everything else is asked as it is drawn.
    pub fn open(client: Arc<Client>) -> Self {
        let sorted = walk(&client, serde_json::json!({ "levels": [] }))
            .iter()
            .filter_map(|n| serde_json::from_value(n.get("id")?.clone()).ok())
            .collect();
        Remote {
            inner: Arc::new(Inner {
                client,
                last: Mutex::new(Last::default()),
                sorted,
                rows_kept: Mutex::new(std::collections::HashMap::new()),
            }),
        }
    }

    /// Reuse the last reply when the question has not changed.
    fn cached<R>(
        &self,
        folds: &Expansion<SortKey>,
        at: usize,
        limit: usize,
        read: impl FnOnce(&Last) -> R,
    ) -> R {
        let want = (folds.parts(), at, limit);
        let Ok(mut last) = self.inner.last.lock() else {
            return read(&Last::default());
        };
        if last.asked.as_ref() != Some(&want) {
            *last = self.fetch(folds, at, limit, serde_json::Value::Null);
            last.asked = Some(want);
        }
        read(&last)
    }

    /// A question the cache cannot hold — one carrying an anchor or a settle, which asks about a
    /// window it does not name. Fetched and thrown away.
    fn one_off(&self, folds: &Expansion<SortKey>, extra: serde_json::Value) -> Last {
        self.fetch(folds, 0, 1, extra)
    }

    fn fetch(
        &self,
        folds: &Expansion<SortKey>,
        at: usize,
        limit: usize,
        extra: serde_json::Value,
    ) -> Last {
        let mut params = serde_json::json!({
            "folds": parts_of(folds),
            "cursor": at,
            "limit": limit,
        });
        if let (Some(map), Some(more)) = (params.as_object_mut(), extra.as_object()) {
            for (k, v) in more {
                map.insert(k.clone(), v.clone());
            }
        }
        let Ok(reply) = self.inner.client.call("query.tree", Some(params)) else {
            return Last::default();
        };
        let n = |key: &str| reply.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        Last {
            asked: None,
            nodes: reply
                .get("nodes")
                .and_then(|v| v.as_array())
                // A line that will not parse is dropped rather than drawn wrong.
                .map(|a| a.iter().filter_map(node_of).collect())
                .unwrap_or_default(),
            spine: reply
                .get("spine")
                .and_then(|v| v.as_str())
                .and_then(Spine::from_hex)
                .unwrap_or_default(),
            at: n("at"),
            artists: n("artists"),
            albums: n("albums"),
        }
    }

    /// One `query.under`: the tracks below a line, and what is left to open under it.
    fn under(&self, path: &[SortKey], id: Option<CopyId>) -> (Vec<CopyId>, Vec<Vec<SortKey>>) {
        let mut params = serde_json::json!({ "path": keys(path) });
        if let (Some(map), Some(v)) = (
            params.as_object_mut(),
            id.and_then(|i| serde_json::to_value(i).ok()),
        ) {
            map.insert("id".into(), v);
        }
        let Ok(reply) = self.inner.client.call("query.under", Some(params)) else {
            return (Vec::new(), Vec::new());
        };
        // **A bare array of ids, not lines.** `query.under` answers what to play, and a
        // track's id is the whole of that — the node parser next door reads an `id` field out
        // of each *line*, which over this reply finds nothing at all.
        let tracks = reply
            .get("tracks")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let openable = reply.get("openable").and_then(paths_of).unwrap_or_default();
        (tracks, openable)
    }
}

impl Browsing for Remote {
    /// **The whole tree**, which only the tests ask for — see [`Browsing::lines`].
    ///
    /// Walked page by page, because "all of it" has no spelling on the wire: the protocol pages
    /// everything on purpose, and a method meaning "send me everything" is the one call that
    /// could not be answered from a window.
    fn lines(&self, folds: &Expansion<SortKey>, unfolded: bool) -> Vec<Node> {
        let folds = if unfolded {
            Expansion::default()
        } else {
            folds.clone()
        };
        walk(
            &self.inner.client,
            serde_json::json!({ "folds": parts_of(&folds) }),
        )
        .iter()
        .filter_map(node_of)
        .collect()
    }

    /// **The shape, asked with no page at all.** A motion reads four bits a line and no titles,
    /// so `limit: 0` is the honest request — and it is the same question every frame, so the
    /// cache answers it.
    fn spine(&self, folds: &Expansion<SortKey>) -> Spine {
        self.cached(folds, 0, 0, |last| last.spine.clone())
    }

    fn page(&self, folds: &Expansion<SortKey>, at: usize, limit: usize) -> Vec<Node> {
        self.cached(folds, at, limit, |last| last.nodes.clone())
    }

    /// Where a line is now.
    ///
    /// **Checked rather than trusted**: `query.tree` falls back to the cursor when the anchor is
    /// gone, so an answer is only an answer if the line it came back with is the one asked for.
    fn at(&self, folds: &Expansion<SortKey>, mark: &Mark) -> Option<usize> {
        let (path, id) = mark;
        let asked = self.one_off(
            folds,
            serde_json::json!({ "anchor": { "path": keys(path), "id": id } }),
        );
        let found = asked.nodes.first()?;
        (found.mark() == *mark).then_some(asked.at)
    }

    fn settle(&self, folds: &Expansion<SortKey>, seat: &[SortKey]) -> usize {
        self.one_off(folds, serde_json::json!({ "settle": keys(seat) }))
            .at
    }

    fn tracks_under(&self, node: &Node) -> Vec<CopyId> {
        let (path, id) = node.mark();
        self.under(&path, id).0
    }

    fn openable_under(&self, path: &[SortKey]) -> Vec<Vec<SortKey>> {
        self.under(path, None).1
    }

    fn find(&self, folds: &Expansion<SortKey>, want: &Target, from: usize) -> Option<Found> {
        let mut params = serde_json::json!({ "folds": parts_of(folds), "from": from });
        let map = params.as_object_mut()?;
        match want {
            Target::Id(id) => {
                map.insert("id".into(), serde_json::to_value(id).ok()?);
            }
            Target::Pattern { needle, backwards } => {
                map.insert("pattern".into(), needle.clone().into());
                map.insert("backwards".into(), (*backwards).into());
            }
        }
        let reply = self.inner.client.call("query.reveal", Some(params)).ok()?;
        Some(Found {
            open: paths_of(reply.get("open")?)?,
            at: reply.get("at")?.as_u64()? as usize,
        })
    }
}

impl Holding for Remote {
    fn sorted(&self) -> &[CopyId] {
        &self.inner.sorted
    }

    fn len(&self) -> usize {
        self.inner.sorted.len()
    }

    fn is_empty(&self) -> bool {
        self.inner.sorted.is_empty()
    }

    fn artist_count(&self) -> usize {
        self.inner.last.lock().map(|l| l.artists).unwrap_or(0)
    }

    fn album_count(&self) -> usize {
        self.inner.last.lock().map(|l| l.albums).unwrap_or(0)
    }

    /// **Already on the line**, because the library worked it out when it drew the page — the
    /// rule about when a record's name has nowhere else to go lives in one place (#135), and a
    /// client re-deriving it is a second copy of it.
    fn standing_for(&self, row: &Node, _title: &str) -> Option<String> {
        match row {
            Node::Track { stands_for, .. } => stands_for.clone(),
            _ => None,
        }
    }

    fn locate(&self, id: CopyId) -> Option<(SortKey, SortKey)> {
        let asked = self.one_off(
            &Expansion::default(),
            serde_json::json!({ "anchor": { "path": [], "id": id } }),
        );
        match asked.nodes.first()? {
            Node::Track { artist, album, .. } => Some((artist.clone(), album.clone())),
            _ => None,
        }
    }

    fn artist_tracks(&self, artist: &SortKey) -> Vec<CopyId> {
        self.under(std::slice::from_ref(artist), None).0
    }

    fn album_tracks(&self, artist: &SortKey, album: &SortKey) -> Vec<CopyId> {
        self.under(&[artist.clone(), album.clone()], None).0
    }
}

/// **Every line, followed page by page.**
///
/// `query.tree` caps a page at 5,000 and says so only by handing back a `next` — so asking for
/// a million gets 5,000 and looks like the whole library until the library is bigger than that.
/// This one has 7,273 rows, so the version that asked once was already wrong here and silently.
///
/// **The loop itself has no test.** Exercising it needs a library larger than one page, and the
/// fixtures are four lines; what is checked instead is the mechanism it relies on — that a page
/// says where the next one starts and following it reaches every line — in
/// `a_client_that_follows_next_sees_the_whole_library`, with a page size of one.
fn walk(client: &Client, base: serde_json::Value) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut cursor = Some(0u64);
    while let Some(at) = cursor {
        let mut params = base.clone();
        let Some(map) = params.as_object_mut() else {
            break;
        };
        map.insert("cursor".into(), at.into());
        map.insert("limit".into(), 5_000.into());
        let Ok(reply) = client.call("query.tree", Some(params)) else {
            break;
        };
        match reply.get("nodes").and_then(|v| v.as_array()) {
            Some(page) if !page.is_empty() => out.extend(page.iter().cloned()),
            // No lines and a `next` would spin for ever; the page being empty is the end.
            _ => break,
        }
        cursor = reply.get("next").and_then(|v| v.as_u64());
    }
    out
}

/// A fold state as the wire spells it.
fn parts_of(folds: &Expansion<SortKey>) -> serde_json::Value {
    let parts = folds.parts();
    serde_json::json!({
        "level": parts.level,
        "eager": parts.eager,
        "opened": parts.opened.iter().map(|p| keys(p)).collect::<Vec<_>>(),
        "closed": parts.closed.iter().map(|p| keys(p)).collect::<Vec<_>>(),
    })
}

/// Sort keys as the wire spells them.
fn keys(path: &[SortKey]) -> Vec<String> {
    path.iter().map(|k| k.as_str().to_owned()).collect()
}

fn node_of(value: &serde_json::Value) -> Option<Node> {
    serde_json::from_value(value.clone()).ok()
}

fn paths_of(value: &serde_json::Value) -> Option<Vec<Vec<SortKey>>> {
    Some(
        value
            .as_array()?
            .iter()
            .filter_map(|p| {
                Some(
                    p.as_array()?
                        .iter()
                        .filter_map(|k| Some(SortKey::already(k.as_str()?)))
                        .collect(),
                )
            })
            .collect(),
    )
}

/// **The read seam, asked rather than folded** — #261, `tree-over-the-wire.md` §7.
///
/// Every question `model::rows::Listing` asks is answerable without a library now — the seam
/// lends nothing — so this is the implementation that makes an attached view stop folding.
///
/// **Rows are kept, because a redraw asks about the same forty.** Keyed by id and cleared when
/// the library moves, which is the same rule the tree's cache follows: a cache goes wrong by
/// answering the wrong question, and "which fold was this" is half of every question here.
impl crate::model::rows::Listing for Remote {
    fn listed(&self, id: CopyId) -> Option<crate::model::rows::Slip> {
        self.row_of(id).0
    }

    fn playable(&self, id: CopyId) -> Option<crate::model::rows::Playable> {
        self.row_of(id).1
    }

    fn recording_of(&self, id: CopyId) -> Option<Id<crate::model::entity::Recording>> {
        self.row_of(id).2
    }

    fn rows(&self) -> Vec<CopyId> {
        self.inner.sorted.clone()
    }

    fn row(
        &self,
        recording: Id<crate::model::entity::Recording>,
    ) -> Option<crate::model::seam::Row> {
        self.recording(recording, &[]).0
    }

    fn chosen(
        &self,
        recording: Id<crate::model::entity::Recording>,
        here: &[Id<crate::model::entity::Device>],
        _prefers: &[crate::model::encoding::Format],
    ) -> Option<(
        String,
        Id<crate::model::entity::Encoding>,
        crate::model::encoding::Levels,
    )> {
        self.recording(recording, here).1
    }

    fn entries_of(
        &self,
        collection: Id<crate::model::entity::Collection>,
    ) -> Vec<Id<crate::model::entity::Recording>> {
        self.read(
            "query.entries",
            serde_json::json!({ "collection": collection.to_string() }),
        )
        .and_then(|r| serde_json::from_value(r.get("entries")?.clone()).ok())
        .unwrap_or_default()
    }

    fn find(&self, of: crate::model::about::Kind, name: &str) -> crate::model::about::Found {
        self.read(
            "query.name",
            serde_json::json!({ "kind": of.word(), "name": name }),
        )
        .and_then(|r| serde_json::from_value(r.get("found")?.clone()).ok())
        // **Nothing by that name**, which is what a library with nothing to say answers, and
        // what a library that could not be asked has to.
        .unwrap_or(crate::model::about::Found::Nothing)
    }

    fn label_of(&self, focus: crate::model::focus::Focus) -> String {
        self.about_of(focus).1
    }

    fn about(
        &self,
        focus: crate::model::focus::Focus,
        _happened: &[crate::model::seam::Happened],
    ) -> crate::model::about::About {
        // **The history is the engine's**, not this side's. A claim's basis is the library's to
        // state (`info-panel.md`), and a panel handed one could be handed a different one — so
        // the argument is ignored here and the engine reads its own.
        self.about_of(focus).0
    }

    fn places(&self, facet: Id<crate::model::entity::Facet>) -> bool {
        self.read(
            "query.places",
            serde_json::json!({ "facet": facet.to_string() }),
        )
        .and_then(|r| r.get("places")?.as_bool())
        .unwrap_or(false)
    }
}

impl Remote {
    /// One call, or nothing. A failed ask is an empty answer for the reason the tree's is.
    fn read(&self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        self.inner.client.call(method, Some(params)).ok()
    }

    /// The three things a row is asked for, kept — and read ahead.
    ///
    /// **A block, not a row.** One row over a unix socket is 226 µs and a filter reads one per
    /// row of the whole library: 1.5 s for 7,273, on every `:view`. Measured with
    /// `just read-cost`, and it is the cost of *asking*, not of answering — the engine has the
    /// row in hand.
    ///
    /// So a miss fetches this row and the [`AHEAD`] after it **in the library's own order**,
    /// which is the order both callers walk in: a filter goes through `rows()` from the top,
    /// and a pane draws a run of adjacent lines. Reading ahead in any other order would be a
    /// guess; this one is what the caller already told us it was doing.
    fn row_of(
        &self,
        id: CopyId,
    ) -> (
        Option<crate::model::rows::Slip>,
        Option<crate::model::rows::Playable>,
        Option<Id<crate::model::entity::Recording>>,
    ) {
        if let Ok(kept) = self.inner.rows_kept.lock()
            && let Some(held) = kept.get(&id)
        {
            return held.clone();
        }

        // The row asked for leads, so a caller that walks in some other order still gets what
        // it asked for in one trip rather than none.
        let from = self.inner.sorted.iter().position(|r| *r == id);
        let mut want = vec![id];
        if let Some(at) = from {
            want.extend(
                self.inner
                    .sorted
                    .iter()
                    .skip(at + 1)
                    .take(AHEAD - 1)
                    .copied(),
            );
        }

        let got = self
            .read("query.row", serde_json::json!({ "ids": want }))
            .and_then(|r| r.get("rows")?.as_array().cloned())
            .unwrap_or_default();
        let mut mine = (None, None, None);
        if let Ok(mut kept) = self.inner.rows_kept.lock() {
            if kept.len() + got.len() > KEEP {
                kept.clear();
            }
            for line in &got {
                let at = |key: &str| line.get(key).cloned().unwrap_or(serde_json::Value::Null);
                let Some(this): Option<CopyId> = serde_json::from_value(at("id")).ok() else {
                    continue;
                };
                let row = (
                    serde_json::from_value(at("row")).ok().flatten(),
                    at("locator")
                        .as_str()
                        .map(|l| crate::model::rows::Playable {
                            locator: l.to_owned(),
                            gain: None,
                        }),
                    serde_json::from_value(at("recording")).ok().flatten(),
                );
                if this == id {
                    mine = row.clone();
                }
                kept.insert(this, row);
            }
        }
        mine
    }

    /// What a recording draws as, and which file to play for it.
    #[allow(clippy::type_complexity)]
    fn recording(
        &self,
        recording: Id<crate::model::entity::Recording>,
        here: &[Id<crate::model::entity::Device>],
    ) -> (
        Option<crate::model::seam::Row>,
        Option<(
            String,
            Id<crate::model::entity::Encoding>,
            crate::model::encoding::Levels,
        )>,
    ) {
        self.read(
            "query.recording",
            serde_json::json!({
                "recording": recording.to_string(),
                "here": here.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
            }),
        )
        .map(|r| {
            let at = |key: &str| r.get(key).cloned().unwrap_or(serde_json::Value::Null);
            (
                serde_json::from_value(at("row")).ok().flatten(),
                serde_json::from_value(at("chosen")).ok().flatten(),
            )
        })
        .unwrap_or((None, None))
    }

    fn about_of(&self, focus: crate::model::focus::Focus) -> (crate::model::about::About, String) {
        let empty = || crate::model::about::About {
            kind: String::new(),
            title: String::new(),
            subtitle: None,
            sections: Vec::new(),
        };
        self.read(
            "query.about",
            serde_json::json!({ "focus": serde_json::to_value(focus).unwrap_or_default() }),
        )
        .map(|r| {
            let at = |key: &str| r.get(key).cloned().unwrap_or(serde_json::Value::Null);
            (
                serde_json::from_value(at("about")).unwrap_or_else(|_| empty()),
                at("label").as_str().unwrap_or_default().to_owned(),
            )
        })
        .unwrap_or_else(|| (empty(), String::new()))
    }
}
