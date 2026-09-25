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

/// The view a `Remote` is showing, as the wire spells it.
///
/// **Set once, when the view changes.** A `Remote` is rebuilt rather than retuned, because its
/// flat ordering is the view's too — see [`Remote::showing`].
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Showing {
    /// The composed view's own source, or `None` for the whole library.
    pub view: Option<String>,
    /// Two field names, or `Some(vec![])` for no grouping at all.
    pub levels: Option<Vec<String>>,
}

/// What the librarian said about the view it was asked for.
#[derive(Default, Clone)]
pub struct Said {
    pub problems: Vec<String>,
    pub widened: Vec<usize>,
}

/// What one reply said, and what it was asked.
#[derive(Default)]
struct Last {
    /// The fold state and the window, which is the whole of a plain `query.tree`.
    asked: Option<(crate::fold::Parts<SortKey>, usize, usize)>,
    nodes: Vec<Node>,
    spine: Spine,
    /// **Which fold state the spine belongs to, kept apart from the window** — #261.
    ///
    /// The spine comes back with *every* reply, whatever window was asked for, because it is
    /// four bits a line for the whole tree and the navigator's only input. Keyed with the
    /// window it arrived beside, it was evicted by the next question and asked for again:
    /// `spine` wants `(folds, 0, 0)` and `page` wants `(folds, 0, 41)`, a frame asks both, and
    /// one slot holds one — so **every frame made two round trips and neither ever hit**.
    /// Measured at 521 fetches over a run that needed none after the first.
    spine_for: Option<crate::fold::Parts<SortKey>>,
    at: usize,
    /// How many lines the tree has, so a window that stops short is told apart from one that
    /// was cut short — the last page of a list is shorter than the limit asked for, and would
    /// otherwise be refetched on every frame the cursor spent at the bottom.
    total: usize,
    artists: usize,
    albums: usize,
}

impl Last {
    /// Whether the reply already held answers this question.
    ///
    /// **A held window answers any window inside it**, which is what makes a frame free: a pane
    /// asks for the forty-one lines it draws and then asks again for the one under the cursor,
    /// and the second is a slice of the first. Only the exact question was matched before, so
    /// the narrower one evicted the wider one it had just been cut from.
    ///
    /// A `limit` of zero asks for no page at all — the spine and nothing else — and is answered
    /// by any reply under the same folds, because every reply carries the spine.
    fn answers(&self, want: &(crate::fold::Parts<SortKey>, usize, usize)) -> bool {
        let (folds, at, limit) = want;
        if *limit == 0 {
            return self.spine_for.as_ref() == Some(folds);
        }
        let Some((held, _, _)) = self.asked.as_ref() else {
            return false;
        };
        // **The window held, not the window asked for.** The engine answers from wherever it
        // put the cursor — an anchor beats a cursor, and both are clamped to the tree's length —
        // so the reply describes itself and the question does not.
        let ends = self.at + self.nodes.len();
        held == folds && *at >= self.at && (at + limit <= ends || ends >= self.total)
    }

    /// The lines of a window inside the one held.
    fn slice(&self, at: usize, limit: usize) -> Vec<Node> {
        let start = at.saturating_sub(self.at).min(self.nodes.len());
        self.nodes[start..(start + limit).min(self.nodes.len())].to_vec()
    }
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
    /// **The composed view and the grouping, sent with every tree question** — #261.
    ///
    /// Held here rather than passed through `Browsing`, because a view is not a property of
    /// any one question: every tree question is asked *of a view*, and threading it through
    /// eight methods would let two of them disagree.
    showing: Showing,
    /// What the librarian said about the view, from the last reply that carried it.
    ///
    /// **Kept because the complaint and the question arrive together but are wanted apart**:
    /// the tree is redrawn every frame and the complaint is reported once, when the view
    /// changes. #119 is why it is reported at all.
    said: Mutex<Said>,
    last: Mutex<Last>,
    /// The flat ordering of this view, fetched **when something asks** and lent thereafter.
    ///
    /// [`Holding::sorted`] lends, so this has to own it — which is the whole reason that
    /// signature is worth keeping: view 2 scrolls through all 7,273 and must not ask per frame.
    /// 116 KB against the 40.6 MB it replaces.
    ///
    /// **Lazy, because a reader is built per view and the tree never asks** — #261. Paging the
    /// flat list of the whole library is 1.2 s, and paying it on every `:view` would have made
    /// returning to the unnarrowed library slower than the walk this change removed. `OnceLock`
    /// is what allows both: filled through `&self`, and lent as a slice afterwards.
    sorted: std::sync::OnceLock<Vec<CopyId>>,
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
        Remote::showing(client, Showing::default())
    }

    /// **A reader of one view** — #261.
    ///
    /// A new `Remote` per view rather than a setter, and the flat ordering is why: `sorted`
    /// lends, so it cannot be swapped behind a `&self`, and it is the view's ordering — a
    /// narrowed view scrolls through fewer rows in view 2. Rebuilding is one paged walk and
    /// happens when the view changes, which is exactly when the ordering really did.
    ///
    /// The client is shared, so this is not a reconnection.
    pub fn showing(client: Arc<Client>, showing: Showing) -> Self {
        let me = Remote {
            inner: Arc::new(Inner {
                client,
                showing,
                said: Mutex::new(Said::default()),
                last: Mutex::new(Last::default()),
                sorted: std::sync::OnceLock::new(),
                rows_kept: Mutex::new(std::collections::HashMap::new()),
            }),
        };
        // **Ask once so the complaint is here before anybody looks.** A view is reported when
        // it changes, which is now; the spine is the cheapest question that carries the
        // answer, and the reply it caches is the one the first frame wants anyway.
        let _ = me.fetch(&Expansion::default(), 0, 0, serde_json::Value::Null);
        me
    }

    /// The flat ordering of this view, paged in the first time anybody asks.
    fn ordering(&self) -> &[CopyId] {
        self.inner.sorted.get_or_init(|| {
            let mut base = serde_json::json!({ "levels": [] });
            if let (Some(map), Some(view)) = (base.as_object_mut(), self.inner.showing.view.clone())
            {
                map.insert("view".into(), view.into());
            }
            walk(&self.inner.client, base)
                .into_iter()
                .filter_map(|n| match n {
                    Node::Track { id, .. } => Some(id),
                    _ => None,
                })
                .collect()
        })
    }

    /// **Whether the flat ordering has been paged in yet** — #261.
    ///
    /// For a test asserting that something did *not* ask for it. Paging the whole library's
    /// flat list is ~0.8 s, and the reason view 2's cursor is a mark is so a rebuild never
    /// triggers it; "the cursor came back" is true whether or not that happened, so the
    /// absence needs its own witness.
    pub fn ordering_is_here(&self) -> bool {
        self.inner.sorted.get().is_some()
    }

    /// Which view this reader is showing, so a caller can tell whether to build another.
    pub fn shows(&self) -> &Showing {
        &self.inner.showing
    }

    /// What the librarian said about the view, as of the last tree question.
    pub fn said(&self) -> Said {
        self.inner
            .said
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Put the view and the grouping into a tree question.
    ///
    /// Every `query.tree`, `query.reveal` and `query.under` goes through here, because a view
    /// that reached two of the three would give a pane whose rows and whose `x` disagree.
    fn asking(&self, mut params: serde_json::Value) -> serde_json::Value {
        if let Some(map) = params.as_object_mut() {
            if let Some(view) = self.inner.showing.view.clone() {
                map.insert("view".into(), view.into());
            }
            if let Some(levels) = self.inner.showing.levels.clone() {
                map.insert("levels".into(), levels.into());
            }
        }
        params
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
        if !last.answers(&want) {
            let spine = (last.spine_for.as_ref() == Some(&want.0)).then(|| last.spine.clone());
            *last = self.fetch(folds, at, limit, serde_json::Value::Null);
            last.asked = Some(want);
            // A reply always carries the spine, so this only matters when the reply did not
            // arrive: a failed fetch is `Last::default()` and would otherwise lose a spine that
            // is still true, leaving the navigator with an empty tree until the next success.
            if last.spine.is_empty()
                && let Some(spine) = spine
            {
                last.spine = spine;
            }
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
        let Ok(reply) = self
            .inner
            .client
            .call("query.tree", Some(self.asking(params)))
        else {
            return Last::default();
        };
        // **The complaint arrives with the answer and is kept until it is asked for.** A tree
        // is redrawn every frame; a broken saved view is reported once, when the view changes.
        if let Ok(mut said) = self.inner.said.lock() {
            said.problems = reply
                .get("problems")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            said.widened = reply
                .get("widened")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
        }
        let n = |key: &str| reply.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        Last {
            asked: None,
            spine_for: Some(folds.parts()),
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
            total: n("total"),
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
        let Ok(reply) = self
            .inner
            .client
            .call("query.under", Some(self.asking(params)))
        else {
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
            self.asking(serde_json::json!({ "folds": parts_of(&folds) })),
        )
    }

    /// **The shape, asked with no page at all.** A motion reads four bits a line and no titles,
    /// so `limit: 0` is the honest request — and it is the same question every frame, so the
    /// cache answers it.
    fn spine(&self, folds: &Expansion<SortKey>) -> Spine {
        self.cached(folds, 0, 0, |last| last.spine.clone())
    }

    fn page(&self, folds: &Expansion<SortKey>, at: usize, limit: usize) -> Vec<Node> {
        self.cached(folds, at, limit, |last| last.slice(at, limit))
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
        let reply = self
            .inner
            .client
            .call("query.reveal", Some(self.asking(params)))
            .ok()?;
        Some(Found {
            open: paths_of(reply.get("open")?)?,
            at: reply.get("at")?.as_u64()? as usize,
        })
    }
}

impl Holding for Remote {
    fn sorted(&self) -> &[CopyId] {
        self.ordering()
    }

    fn len(&self) -> usize {
        self.ordering().len()
    }

    fn is_empty(&self) -> bool {
        self.ordering().is_empty()
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

impl Remote {
    /// **What the library has heard**, fetched whole.
    ///
    /// One reply of 4.5 MB against the 11.8 MB fold a view used to keep to answer the same nine
    /// questions. Through `call_typed`, which is the whole reason it is affordable: sending it
    /// was tried once over `call` and made the view *larger* than folding the log had
    /// (`tree-over-the-wire.md` §11).
    ///
    /// Not cached here — the caller holds it in `Ui::listened` and asks again when a `library`
    /// event says there is more log, which is the moment a detached view refolds.
    pub fn heard(&self) -> Option<crate::projection::heard::Heard> {
        #[derive(serde::Deserialize)]
        struct Reply {
            heard: crate::projection::heard::Heard,
        }
        self.inner
            .client
            .call_typed::<Reply>("query.listened", Some(serde_json::json!({})))
            .ok()
            .map(|r| r.heard)
    }
}

/// What `query.row` answers: a block of lines, each with the three things a row is asked for.
#[derive(Default, serde::Deserialize)]
struct Rows {
    rows: Vec<Line>,
}

#[derive(serde::Deserialize)]
struct Line {
    id: CopyId,
    row: Option<crate::model::rows::Slip>,
    locator: Option<String>,
    recording: Option<Id<crate::model::entity::Recording>>,
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
fn walk(client: &Client, base: serde_json::Value) -> Vec<Node> {
    /// Just the two fields a walk reads. The rest of a page's reply is left on the wire.
    #[derive(serde::Deserialize)]
    struct Page {
        nodes: Vec<Node>,
        next: Option<u64>,
    }

    let mut out = Vec::new();
    let mut cursor = Some(0u64);
    while let Some(at) = cursor {
        let mut params = base.clone();
        let Some(map) = params.as_object_mut() else {
            break;
        };
        map.insert("cursor".into(), at.into());
        map.insert("limit".into(), 5_000.into());
        // **Typed, because this is the largest reply the process ever asks for** — #261. Every
        // line of the library, and through a `serde_json::Value` that is a tree several times
        // the size of the text before any of it is a `Node`.
        let Ok(page) = client.call_typed::<Page>("query.tree", Some(params)) else {
            break;
        };
        if page.nodes.is_empty() {
            // No lines and a `next` would spin for ever; the page being empty is the end.
            break;
        }
        out.extend(page.nodes);
        cursor = page.next;
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
    /// **One trip for the screenful** — #261.
    ///
    /// The default is a no-op because a local library has nothing to fetch. This one has,
    /// and doing it a row at a time cost 198 ms a keystroke in view 4.
    fn ahead(&self, ids: &[CopyId]) {
        self.warm(ids);
    }

    /// **One trip for the pairing** — #261.
    ///
    /// The default inverts the library by walking every row, which over a socket is a round
    /// trip each: 706–956 ms to answer a queue of a hundred. The engine holds the rows, so it
    /// does the walk and sends back only the pairs asked for.
    fn copies_of(
        &self,
        recordings: &[Id<crate::model::entity::Recording>],
    ) -> std::collections::HashMap<Id<crate::model::entity::Recording>, CopyId> {
        if recordings.is_empty() {
            return Default::default();
        }
        #[derive(serde::Deserialize, Default)]
        struct Held {
            held: Vec<(Id<crate::model::entity::Recording>, CopyId)>,
        }
        let names: Vec<String> = recordings.iter().map(|r| r.to_string()).collect();
        let got: Held = self
            .inner
            .client
            .call_typed(
                gmuse_proto::request::method::QUERY_HOLDINGS,
                Some(serde_json::json!({ "recordings": names })),
            )
            .unwrap_or_default();
        got.held.into_iter().collect()
    }

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
        self.ordering().to_vec()
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

    fn covers(
        &self,
        focus: crate::model::focus::Focus,
    ) -> Vec<Id<crate::model::entity::Recording>> {
        self.read(
            "query.covers",
            serde_json::json!({ "focus": serde_json::to_value(focus).unwrap_or_default() }),
        )
        .and_then(|r| serde_json::from_value(r.get("recordings")?.clone()).ok())
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

    /// **A name, not a page** — #292.
    ///
    /// This was `self.about_of(focus).1`: ask for the whole page and keep the
    /// label. `breadcrumb` calls it once per step of the path, every frame, so
    /// an attached view paid one full page build — fact log included — per step
    /// per frame. Measured in the running player at **110 ms a frame** with the
    /// page itself already cached, which is what `--log`'s `panel` figure was.
    ///
    /// The local `Listing::label_of` exists for exactly this reason (#128); the
    /// wire had no equivalent, so the remote reintroduced the bug the local one
    /// was written to fix.
    fn label_of(&self, focus: crate::model::focus::Focus) -> String {
        self.read(
            "query.label",
            serde_json::json!({ "focus": serde_json::to_value(focus).unwrap_or_default() }),
        )
        .and_then(|r| r.get("label")?.as_str().map(str::to_owned))
        .unwrap_or_default()
    }

    /// **The history is the engine's to read, and the panel's to ask for** —
    /// #293.
    ///
    /// A claim's basis is the library's to state (`info-panel.md`), so this side
    /// never hands one over. What it *can* say is whether the section is open —
    /// and until #293 it could not, so the engine read the log on every page
    /// build including the folded default. `--log` measured that at 116 ms a
    /// frame.
    fn about(
        &self,
        focus: crate::model::focus::Focus,
        history: crate::model::about::History<'_>,
    ) -> crate::model::about::About {
        self.about_of(focus, history.is_shown()).0
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
        // **Read ahead only if the ordering is already here.** Filling it is a walk of the
        // whole view, which is far more than one row is worth — and `Listing::ahead` is how a
        // caller that wants a block says so.
        let from = self
            .inner
            .sorted
            .get()
            .and_then(|sorted| sorted.iter().position(|r| *r == id));
        let mut want = vec![id];
        if let (Some(at), Some(sorted)) = (from, self.inner.sorted.get()) {
            want.extend(sorted.iter().skip(at + 1).take(AHEAD - 1).copied());
        }

        // **Straight from the wire into what it is** — #261. A block of fifty rows is the
        // reply this asks for most often, and going through a `serde_json::Value` first builds
        // a tree several times the size of the text only to pull the same fields back out.
        let got: Rows = self
            .inner
            .client
            .call_typed("query.row", Some(serde_json::json!({ "ids": want })))
            .unwrap_or_default();

        self.keep(got);
        self.inner
            .rows_kept
            .lock()
            .ok()
            .and_then(|kept| kept.get(&id).cloned())
            .unwrap_or((None, None, None))
    }

    /// Put a reply in the cache.
    ///
    /// **Clearing is what a full cache does, and it is why `ahead` exists.** Evicting the
    /// oldest would need an order this map does not keep, and a cache that throws
    /// everything away is only cheap while what it threw away is cheap to fetch again —
    /// which was exactly the 198 ms measured in view 4, where forty library-order batches
    /// per frame overran a 500-row cap four times over. `ahead` fetches the screenful
    /// itself, so the cap is no longer reached by rows nobody asked for.
    fn keep(&self, got: Rows) {
        let Ok(mut kept) = self.inner.rows_kept.lock() else {
            return;
        };
        if kept.len() + got.rows.len() > KEEP {
            kept.clear();
        }
        for line in got.rows {
            kept.insert(
                line.id,
                (
                    line.row,
                    line.locator.map(|locator| crate::model::rows::Playable {
                        locator,
                        gain: None,
                    }),
                    line.recording,
                ),
            );
        }
    }

    /// **The screenful in one trip** — `Listing::ahead` for a library over a socket.
    ///
    /// Asks only for what is not held, and asks in the *caller's* order rather than the
    /// library's, because the caller's order is the one about to be drawn. That is the
    /// whole difference from [`Remote::row_of`]'s read-ahead, which is right for a walk
    /// down the library and wrong for a queue.
    ///
    /// Chunked, so a caller that names more rows than the cache holds still gets a reply
    /// per chunk rather than one enormous one that clears the cache as it lands.
    fn warm(&self, ids: &[CopyId]) {
        let missing: Vec<CopyId> = {
            let Ok(kept) = self.inner.rows_kept.lock() else {
                return;
            };
            let mut seen = std::collections::HashSet::new();
            ids.iter()
                .filter(|id| !kept.contains_key(*id) && seen.insert(**id))
                .copied()
                .collect()
        };
        for chunk in missing.chunks(AHEAD.max(1) * 4) {
            let got: Rows = self
                .inner
                .client
                .call_typed("query.row", Some(serde_json::json!({ "ids": chunk })))
                .unwrap_or_default();
            self.keep(got);
        }
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

    fn about_of(
        &self,
        focus: crate::model::focus::Focus,
        history: bool,
    ) -> (crate::model::about::About, String) {
        let empty = || crate::model::about::About {
            kind: String::new(),
            title: String::new(),
            subtitle: None,
            sections: Vec::new(),
        };
        self.read(
            "query.about",
            serde_json::json!({
                "focus": serde_json::to_value(focus).unwrap_or_default(),
                "history": history,
            }),
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

/// **What one reply may be asked for twice** — #261.
///
/// The cache holds one `query.tree` reply. Whether a second question is a round trip or free
/// is decided entirely by [`Last::answers`], and neither answer is visible from outside: a
/// wrongly-refused question draws the same lines a fetch later, and a wrongly-accepted one
/// draws the wrong lines with no error anywhere. So it is asserted here, on the reply's own
/// terms, rather than through a pane.
#[cfg(test)]
mod one_reply_answers_more_than_one_question {
    use super::*;

    fn folds() -> crate::fold::Parts<SortKey> {
        Expansion::<SortKey>::default().parts()
    }

    /// A reply that landed at `at`, carrying `len` lines out of `total`.
    fn held(at: usize, len: usize, total: usize) -> Last {
        Last {
            asked: Some((folds(), at, len)),
            nodes: (0..len)
                .map(|i| Node::Artist {
                    key: SortKey::already(&format!("{}", at + i)),
                    display: format!("{}", at + i),
                    albums: 1,
                    expanded: false,
                })
                .collect(),
            spine: Spine::default(),
            spine_for: Some(folds()),
            at,
            total,
            artists: 0,
            albums: 0,
        }
    }

    #[test]
    fn a_window_inside_the_one_held_is_not_asked_for_again() {
        let last = held(10, 41, 1_000);
        // The instrument: the wide question it was actually asked must hold, or every
        // narrower one below is being checked against a cache that answers nothing.
        assert!(
            last.answers(&(folds(), 10, 41)),
            "the reply does not answer the question it was the reply to"
        );
        assert!(
            last.answers(&(folds(), 20, 5)),
            "a slice of the held window"
        );
        assert!(last.answers(&(folds(), 50, 1)), "the last line held");

        assert!(!last.answers(&(folds(), 5, 10)), "starts above the window");
        assert!(!last.answers(&(folds(), 40, 20)), "runs past the window");
        assert!(!last.answers(&(folds(), 200, 1)), "nowhere near it");
    }

    /// **A page at the end of the list is shorter than the limit, and is still whole.**
    ///
    /// Without this the cursor sitting on the last line refetched on every frame — the
    /// question asks for forty-one lines, thirty come back because that is all there are, and
    /// a plain length comparison calls that a short answer for ever.
    #[test]
    fn a_window_that_stops_because_the_tree_does_is_answered() {
        let last = held(970, 30, 1_000);
        assert!(
            last.answers(&(folds(), 970, 41)),
            "the tree ends at 1000 and the window reaches it"
        );
        assert!(last.answers(&(folds(), 990, 41)), "and past it");

        // But a short window that stops early for any other reason is not whole.
        let cut = held(10, 30, 1_000);
        assert!(
            !cut.answers(&(folds(), 10, 41)),
            "thirty lines out of a thousand is not the end of anything"
        );
    }

    /// **The spine is asked for with `limit: 0`, and every reply carries it.**
    ///
    /// It is the navigator's entire input and the same question every frame. Keyed with the
    /// window it arrived beside, `spine` and `page` evicted each other and a frame made two
    /// round trips that could never hit.
    #[test]
    fn the_spine_is_answered_by_any_reply_under_the_same_folds() {
        let last = held(500, 41, 1_000);
        assert!(
            last.answers(&(folds(), 0, 0)),
            "a spine asked for at the top, answered by a reply from the middle"
        );

        let mut stale = held(500, 41, 1_000);
        stale.spine_for = None;
        assert!(
            !stale.answers(&(folds(), 0, 0)),
            "a reply that carried no spine cannot answer for one"
        );
    }

    /// The lines handed back are the ones asked for, counted from where the reply landed.
    #[test]
    fn a_slice_is_taken_from_where_the_reply_landed_not_where_it_was_asked() {
        let last = held(10, 41, 1_000);
        let got = last.slice(20, 5);
        let names: Vec<_> = got
            .iter()
            .map(|n| match n {
                Node::Artist { display, .. } => display.clone(),
                _ => unreachable!("the fixture holds artists"),
            })
            .collect();
        assert_eq!(
            names,
            ["20", "21", "22", "23", "24"],
            "off by the window's start"
        );

        // And the whole window is still the whole window.
        assert_eq!(last.slice(10, 41).len(), 41);
        // A slice running past what is held is truncated rather than panicking.
        assert_eq!(last.slice(45, 20).len(), 6);
    }
}
