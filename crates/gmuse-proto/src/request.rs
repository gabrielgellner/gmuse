//! **One typed value per line** — I4, §7.5.
//!
//! Every ingress parses into typed domain values exactly once, at its edge, and
//! nothing downstream handles untyped data. [`Request::of`] is that edge for the
//! socket: after it, `method` is not a string anyone matches on again and
//! `params` is not a `Value` anyone reaches into.
//!
//! ## Why the mapping is a function rather than a serde attribute
//!
//! §7.5 draws `Envelope { id, request }` with the request "decoded from
//! `{method, params}`", which reads like `#[serde(tag = "method", content =
//! "params")]` flattened into the envelope. Serde's `flatten` buffers into its
//! own content type and does not compose with adjacent tagging, and the
//! workarounds are worse than the thing they avoid: what matters to I4 is that
//! there is **one place** where a string becomes a type, not which mechanism
//! spells it. This is that place, and it can say why a method was refused —
//! which an attribute cannot.
//!
//! ## What is not here yet, and why that is not an oversight
//!
//! §7.4's table is illustrative and names `deck.*`, `lane.*`, `param.set` and
//! `mapping.*`. Their payloads are `DeckId`, `When`, `Lane` and `BindingTarget`
//! — types that arrive with phases 6 to 8. Declaring the variants now would mean
//! inventing those types to hold nothing, and **a method name is public API**
//! (§7.4): shipping `deck.load` that accepts a shape nobody has designed is
//! harder to correct than not shipping it.

use serde::{Deserialize, Serialize};

use crate::error::RpcError;
use crate::hello::HelloParams;

/// **Which nodes a person has open** — a view's state, sent with the request.
///
/// The librarian holds none of this (`tree-over-the-wire.md` §2): folding is a view's business
/// and grouping is the library's. It is small because the two lists are *exceptions* to
/// `level` rather than members — twenty artists opened by hand is about 600 bytes, and it does
/// not grow with the library.
///
/// Paths are the sort keys a previous reply gave back, as strings: this crate cannot name a
/// `SortKey` and has no business normalising one. The server turns them into typed keys, which
/// is §12's rule about parsing at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folds {
    /// Everything shallower than this is open. `0` closes everything.
    #[serde(default)]
    pub level: u16,
    /// The depth from which folds open by themselves — the disc level (#169).
    #[serde(default)]
    pub eager: Option<u16>,
    /// Opened by hand at or beyond `level`.
    #[serde(default)]
    pub opened: Vec<Vec<String>>,
    /// Closed by hand shallower than `level`, or at a depth that opens by itself.
    #[serde(default)]
    pub closed: Vec<Vec<String>>,
}

/// **What names a line across a rebuild** — its container's path, and its id if it is a track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub path: Vec<String>,
    #[serde(default)]
    pub id: Option<gmuse_ids::copy::CopyId>,
}

/// What a client asked for, once it is a type.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// The handshake. Must be first on a connection (§7.3).
    Hello(HelloParams),
    /// The cmus-style command language, parsed engine-side (§7.12).
    ///
    /// **The line stays a string here on purpose.** Its parser is the `:`
    /// line's — one textual command language, which is the standing complaint
    /// against `--repl` — and that parser belongs to the engine, not to a wire
    /// crate that must not depend on it (§4.3). The boundary is real; it is one
    /// layer in.
    ExecLine {
        line: String,
    },
    /// What the engine is doing.
    EngineStatus,
    /// Ask it to stop.
    EngineShutdown,
    /// The engine's clock, for offset estimation (§7.9).
    TimeNow,
    /// **Is this filter valid, and if not why** — §7.11.
    ///
    /// `query-language.md` has one language for the `:` line, the config, saved rules and the
    /// socket, and that only holds if one thing parses it: "clients MUST NOT reimplement the
    /// parser". This is how a client checks one without having a parser of its own.
    ///
    /// **The AST half of §7.11 is not here.** The spec says it returns "either the core AST or
    /// `QueryParse { message, span }`", and returning an AST means committing to a wire shape
    /// for it — which §7.11 itself says the surface/core round-tripping issues block. A client
    /// sends text to `query.run`; the AST would only save re-parsing it.
    QueryParse {
        text: String,
    },
    /// **The browse tree, grouped** — §7.11, #261.
    ///
    /// The grouping is a library question and `Shelf::grouped` is the library's answer; the
    /// TUI has been calling it with a `Librarian` of its own, which is why a view holds a fold.
    /// Only the *folding* — which nodes a person has opened — is a view's, and that stays
    /// there: this returns the whole shape and a client draws what it likes.
    ///
    /// `levels` names the two grouping fields, `["artist", "album"]` being the tree as it has
    /// always been. An empty list is flat, which is what view 2 was.
    QueryTree {
        /// **The composed view, as its own source** — #261.
        ///
        /// Not a filter expression: a view is a left fold over terms, and a term may name a
        /// collection or another saved view as well as a filter. A bare filter is a view of
        /// one term, so this is strictly wider than the `filter` it replaces.
        ///
        /// It crosses as text because that is what a view *is* — `:edit` round-trips this
        /// exact document — and because resolving it needs the rows, which are here. A client
        /// that resolved it itself would read every row to keep forty.
        view: Option<String>,
        /// Two field names, or empty for no grouping. Omitted means artist and album.
        levels: Option<Vec<String>>,
        limit: Option<usize>,
        cursor: Option<usize>,
        /// Which nodes the client has open. **Omitted means all of them**, which is what a
        /// client that keeps no fold state wants and what `/` searches.
        folds: Option<Folds>,
        /// The line the cursor was on, if the client would rather resume there than at
        /// `cursor`. A toggle moves every row below it, so an index is stale the moment the
        /// folds change and a name is not.
        anchor: Option<Anchor>,
        /// **Where the cursor goes when its line may not exist any more.**
        ///
        /// A level change can close the row out from under you; vim puts the cursor on the
        /// start of the fold that swallowed it, which is the deepest visible line whose seat is
        /// a prefix of this one. Beaten by `anchor`, which is the exact line rather than the
        /// nearest surviving ancestor.
        settle: Option<Vec<String>>,
    },
    /// **Open the way to a line, and say where it lands** — #261.
    ///
    /// What `/`, `n` and a reveal-by-id ask. The reply is the folds to open and the index the
    /// target sits at **once they are**: opening a fold inserts every line under it, so an
    /// index measured against the unfolded list is stale before a client can use it.
    ///
    /// Searched with everything open, because a closed fold still holds its lines.
    QueryReveal {
        /// The composed view, as its own source — see [`Request::QueryTree`].
        view: Option<String>,
        levels: Option<Vec<String>>,
        folds: Option<Folds>,
        /// Text to match. Exactly one of this and `id`.
        pattern: Option<String>,
        /// A track to find. Exactly one of this and `pattern`.
        id: Option<gmuse_ids::copy::CopyId>,
        /// Where a repeated `n` resumes from.
        from: Option<usize>,
        /// `N` rather than `n`.
        backwards: Option<bool>,
    },
    /// **What sits under one line of the tree** — #261.
    ///
    /// Two answers in one call because they are asked together and cost the same walk: the
    /// tracks under a path — what `a`, `e` and `x` act on — and the paths still openable below
    /// it, which is what `zR` needs to know when it has finished.
    ///
    /// An empty `path` is the whole library, which is what `x` on nothing selected means.
    QueryUnder {
        /// The composed view, as its own source — see [`Request::QueryTree`].
        view: Option<String>,
        levels: Option<Vec<String>>,
        /// The seat of the line, as a previous reply gave it.
        path: Vec<String>,
        /// **The track's id, when the line is a track** — the other half of its mark.
        ///
        /// A seat does not name a track: a track sits at its record's path, and so does the
        /// record's own line. Without this, `x` on a track plays the whole record — which is
        /// what it did until a mutation asked for one line rather than the whole library and
        /// nothing noticed the difference.
        id: Option<gmuse_ids::copy::CopyId>,
    },
    /// **What the library says about one row** — #261.
    ///
    /// Three answers in one call because a caller drawing a row wants all three and they cost
    /// one lookup: the row written out, where its file is, and which recording it is about.
    /// `model::rows::Listing` is the seam these cross for; `Slip` is what makes it possible.
    QueryRow {
        /// **A list, because a caller walking rows is walking all of them.**
        ///
        /// One row at a time is 226 µs over a unix socket and 1.5 s for a library of 7,273 —
        /// which is what a filter costs, and a filter runs on every `:view`. Measured with
        /// `just read-cost`.
        ids: Vec<gmuse_ids::copy::CopyId>,
    },
    /// **Which copy this device holds of each recording named** — #261.
    ///
    /// The inverse of `QueryRow`'s third answer, and asked by everything that arrives holding
    /// recordings rather than copies: the engine's queue, a restored session, a collection. A
    /// recording is what crosses the wire — it outlives a file and a path — and a pane draws
    /// copies, so somebody has to make the pairing on every launch.
    ///
    /// **A list, because a caller holding one recording is holding all of them.** The client
    /// used to invert the *whole library* to answer a hundred lookups — 7,273 rows, which is
    /// one `recording_of` each and was 706–956 ms over a socket. Asking for what is wanted is
    /// one round trip and a reply the size of the question.
    ///
    /// A recording held as several encodings answers with one of them, which is the same
    /// answer the whole-library walk gave and the same one a pane can draw. Which copy to
    /// prefer is `shelf copies`' question and is not decidable here.
    QueryHoldings {
        recordings: Vec<String>,
    },
    /// **What the library says about one recording** — #261. The row it would draw, and which
    /// file to play for it on the devices given.
    QueryRecording {
        recording: String,
        /// Devices the caller can reach. Empty means "wherever".
        here: Vec<String>,
    },
    /// **What the library says about one thing, and who said it** — `info-panel.md`, #261.
    QueryAbout {
        focus: serde_json::Value,
    },
    /// **Resolve a name to a thing** — #129.
    QueryName {
        kind: String,
        name: String,
    },
    /// Whether a facet places a row in the tree — one bit, asked per level.
    QueryPlaces {
        facet: String,
    },
    /// What a collection holds.
    QueryEntries {
        collection: String,
    },
    /// **The recordings a focus stands for** — what filing from a page writes one fact per.
    QueryCovers {
        focus: serde_json::Value,
    },
    /// **What the library has heard** — #261.
    ///
    /// The whole of it in one reply: plays, ratings, when things were acquired and last played,
    /// the assertion graph with its concept labels and derivation edges, and the saved views and
    /// collections. 2.7 MB against the 11.8 MB fold it is taken from, and it ships whole rather
    /// than being asked per recording — there is no expensive half, which is a measurement
    /// rather than a hope (`tree-over-the-wire.md` §10).
    ///
    /// **A client reads this with `call_typed`.** It is 4.5 MB of JSON, and building a
    /// `serde_json::Value` of that on the way is what made the first attempt cost more than it
    /// saved (§11).
    QueryListened,
    /// **Run a filter and get a page of rows** — §7.11, #261.
    ///
    /// Answered on the connection's own thread, from `Desk::current()` — one atomic load of a
    /// shared fold. So a query never queues behind the appender and never behind another
    /// query, and **a client holds only the rows it is drawing.**
    ///
    /// An absent `filter` is everything, which is what a picker opens with.
    QueryRun {
        filter: Option<String>,
        /// How many rows. Omitted means a screenful.
        limit: Option<usize>,
        /// Where to resume, from a previous reply's `next`.
        cursor: Option<usize>,
    },
    /// **Read a page of the programme** — §7.4, #257.
    ///
    /// A page rather than the whole thing, because the whole thing *is* the play order: after
    /// a play gesture at scope `all` it is the library. `from` is the token to start at, and
    /// omitting it means "where playback is" — which makes the default answer "up next", and
    /// the only one that is meaningful whether or not anything was queued by hand.
    QueueGet {
        /// A token from a previous reply's `next`, or from `playback.get`'s `entry`.
        from: Option<u64>,
        /// How many entries. Omitted means a screenful.
        limit: Option<usize>,
    },
    /// Append recordings to the queue.
    ///
    /// **`base_version` is optional here and required on the mutating
    /// siblings** (§7.7). Two appends to a list lose nothing; removing index 3
    /// from a list that has changed removes the wrong track.
    QueueEnqueue {
        recordings: Vec<String>,
        base_version: Option<u64>,
    },
    /// **The queue is now this list** — #233, §7.7.
    ///
    /// For a view that holds a mirror and edits it. A TUI's queue gestures are
    /// vim's — put before, put after, delete three, undo — and every one of them
    /// is "the queue is now this". Naming them individually would need `insert`,
    /// `move` and a transaction, each having to agree with the others about
    /// indices.
    ///
    /// `base_version` is **required** here and optional on `enqueue`: appending to
    /// a list somebody else also appended to loses nothing, while replacing a list
    /// you have not read is a lost update by construction.
    QueueSet {
        recordings: Vec<String>,
        base_version: u64,
    },
    /// Remove one entry by position.
    QueueRemove {
        index: usize,
        base_version: Option<u64>,
    },
    QueueClear {
        base_version: Option<u64>,
    },
    /// What plays after this one runs out — §6 calls it control state because
    /// it decides what plays next.
    SetScope {
        mode: String,
    },
    /// Play this recording on the main player — #230.
    ///
    /// **The one method here that §7.4's table does not list.** That table has
    /// `queue.enqueue` and `deck.load`, and the engine has neither a queue nor a
    /// deck: this is the single player stream it already has code for, reached
    /// over the socket. It is named `player.load` rather than folded into
    /// `deck.load` because #224 settled that the player is a deck *plus a
    /// queue* rather than a deck, so claiming the deck spelling would claim a
    /// deck exists.
    PlayerLoad {
        recording: String,
    },
    /// **Play, pause and toggle, as three distinct verbs** — #225,
    /// `transport-keymap-spec.md` D-6 to D-8.
    ///
    /// The text language cannot carry these. `play` there means *play the selection*, which
    /// needs a cursor; `pause` means the cmus toggle; and `toggle` names a setting
    /// (`toggle shuffle`). All three are settled spellings with muscle memory behind them.
    /// #224's O-6 settled two enums rather than three: `cmd::Command` is the text language,
    /// and this is the wire.
    ///
    /// **No `DeckSel` yet.** #224 settled the selector's shape, and nothing has decks — a
    /// parameter with one reachable value would be a wire commitment bought with nothing.
    PlayerTransport {
        verb: Transport,
    },
    /// **Seek, as a number rather than as a sentence** — #265.
    ///
    /// This went over the wire as the text `seek 12`, built with `format!` from a
    /// `Duration` by way of `as_secs()` — so an attached view's seek lost its
    /// fraction, and a session restore landed up to a second early. Both ends
    /// already held the value typed: `SeekTo` on one side, `Command::Seek` on the
    /// other. Only the middle spelled it out and read it back.
    PlayerSeek {
        to: Seek,
    },
    /// **What is playing, right now, asked rather than waited for** — #243, #248.
    ///
    /// The `playback` topic reports *discontinuities* and §7.8 forbids a steady
    /// position stream, so a client that connects between two of them learns
    /// nothing until the next one — which mid-album is the length of a track. A
    /// status bar polls and cannot subscribe at all; a view that attaches
    /// mid-playback showed "stopped" over audible music.
    ///
    /// **The queue already answers its own version of this question.** `queue.get`
    /// returns the modes on the read as well as on the event, *"so a view that
    /// attaches mid-stream knows them without waiting for one to change"*. This is
    /// that sentence applied to the topic it was always more true of.
    ///
    /// `format` renders the current track through the same template language the
    /// config and the `:` line use. It exists because this library's `now_playing`
    /// puts the **title last**, so a bar that truncated a pre-rendered line would
    /// throw away the field it was looking at (§13, 2026-09-01).
    PlaybackGet {
        format: Option<String>,
    },
    /// Start receiving events on these topics (§7.8).
    Subscribe {
        topics: Vec<crate::Topic>,
    },
    /// Stop receiving them.
    Unsubscribe {
        topics: Vec<crate::Topic>,
    },
    /// One of §8.4's named write operations.
    ///
    /// **A single variant carrying the operation**, rather than eight, because
    /// what the engine does with them is the same in every case: route to the
    /// librarian, which is the only appender. The operations differ in their
    /// arguments, not in how they reach the log.
    Library {
        operation: LibraryOp,
        params: serde_json::Value,
    },
}

/// **Where to seek to, in seconds** — #265.
///
/// Seconds rather than frames because a view does not know the device rate, and
/// `f64` rather than whole seconds because the fraction is the whole point: a
/// session restore seeks to `position_secs`, and rounding that down is audible
/// as a lost half-second every time a view reattaches.
///
/// Two variants rather than a number and a flag, so "seek to 5" and "seek on by
/// 5" cannot be confused by a caller that forgot the flag.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Seek {
    /// From the start of the track.
    Absolute(f64),
    /// Forwards (positive) or backwards (negative) from where it is now.
    Relative(f64),
}

/// **Which transport verb** — #225, `transport-keymap-spec.md` D-6 to D-8.
///
/// Serialised in lower case so the wire reads as the words people use: `{"verb": "toggle"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// Start, or keep going. **Idempotent**: sending it to a playing stream changes nothing,
    /// which is what makes it safe under key repeat.
    Play,
    /// Halt, leaving the playhead where it is. **Idempotent**, and the one a broadcast wants:
    /// "pause everything" must not start whatever was paused.
    Pause,
    /// Pause if running, resume if not — the cmus verb, resolved by the engine.
    Toggle,
    /// Return to the reference point, or set it there — **the engine decides which**
    /// from the run state (#229, D-14). Running means go back and wait; paused means
    /// the playhead *is* the new reference. A sender that resolved it would race the
    /// state it read, which is D-8 with another verb's name on it.
    ///
    /// The reference starts at the top of the track, which is what makes one verb
    /// cover both readings: on a player that has marked nothing, cue is "back to the
    /// start and wait" — cmus's stop, without stopping.
    Cue,
    /// **Play from the reference without moving it** — #228, §5's third case.
    ///
    /// The start of a preview; the end is a plain `Cue`, because a running deck
    /// already returns to the reference and waits. `KeyPhase` stays in the view
    /// (§8): the engine never learns whether a terminal can report key release,
    /// only that a preview began.
    ///
    /// **Spelled out, because `rename_all = "lowercase"` would make it
    /// `cuepreview`** — and a method name is public API (§7.4), so a wire word
    /// nobody would choose on purpose is not one to inherit from a derive.
    #[serde(rename = "cue_preview")]
    CuePreview,
}

/// §8.4's write surface, and there is no other one.
///
/// **No read-only operation is here and that is deliberate.** `query`,
/// `render`, `duplicates`, `copies`, `slots`, `verify`, `artists`, `names` and
/// `tree` read directly with no engine (I15) — and several are explicitly
/// *reported, never decided*: their output is a decision brief for a person,
/// which is not a thing to put behind an RPC that returns a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryOp {
    /// A file of proposed facts, each checked before it is appended.
    Apply,
    /// Catalogue this audio.
    Import,
    /// Reconcile holdings with what is on disk. Long-running.
    Scan,
    /// Retire or bring back copies.
    Restore,
    Remove,
    /// Drop holdings assertions nothing reads. Long-running.
    Compact,
    /// File whatever is in no collection.
    Unfiled,
    /// The facts a view emits directly: collections, ratings, merges.
    Edit,
}

impl LibraryOp {
    pub const ALL: &'static [LibraryOp] = &[
        LibraryOp::Apply,
        LibraryOp::Import,
        LibraryOp::Scan,
        LibraryOp::Restore,
        LibraryOp::Remove,
        LibraryOp::Compact,
        LibraryOp::Unfiled,
        LibraryOp::Edit,
    ];

    /// The method name, which is `library.` and this.
    pub fn as_str(self) -> &'static str {
        match self {
            LibraryOp::Apply => "apply",
            LibraryOp::Import => "import",
            LibraryOp::Scan => "scan",
            LibraryOp::Restore => "restore",
            LibraryOp::Remove => "remove",
            LibraryOp::Compact => "compact",
            LibraryOp::Unfiled => "unfiled",
            LibraryOp::Edit => "edit",
        }
    }

    pub fn method(self) -> &'static str {
        match self {
            LibraryOp::Apply => "library.apply",
            LibraryOp::Import => "library.import",
            LibraryOp::Scan => "library.scan",
            LibraryOp::Restore => "library.restore",
            LibraryOp::Remove => "library.remove",
            LibraryOp::Compact => "library.compact",
            LibraryOp::Unfiled => "library.unfiled",
            LibraryOp::Edit => "library.edit",
        }
    }

    /// **Long-running ones acknowledge and report through events** (§8.4).
    pub fn is_long_running(self) -> bool {
        matches!(self, LibraryOp::Scan | LibraryOp::Compact)
    }

    fn of_method(method: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|op| op.method() == method)
    }
}

/// Which topics a subscription is about.
///
/// **An absent or empty list means every topic.** A client that asks for
/// everything is the common case — a status bar wants whatever moves — and
/// making it enumerate seven names it would have to update when an eighth
/// arrives is how clients fall behind the protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TopicsParams {
    #[serde(default)]
    pub topics: Vec<crate::Topic>,
}

impl TopicsParams {
    pub fn resolved(self) -> Vec<crate::Topic> {
        if self.topics.is_empty() {
            crate::Topic::ALL.to_vec()
        } else {
            self.topics
        }
    }
}

/// The typed parameter blocks, so each one has a name a refusal can quote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecLineParams {
    pub line: String,
}

/// The method names, in one place.
///
/// **Public API** (§7.4): renaming one is a protocol version bump, so they are
/// constants rather than string literals scattered through a match.
pub mod method {
    pub const HELLO: &str = "hello";
    pub const EXEC_LINE: &str = "cmd.exec_line";
    pub const ENGINE_STATUS: &str = "engine.status";
    pub const ENGINE_SHUTDOWN: &str = "engine.shutdown";
    pub const TIME_NOW: &str = "time.now";
    pub const PLAYER_LOAD: &str = "player.load";
    pub const PLAYER_TRANSPORT: &str = "player.transport";
    pub const PLAYER_SEEK: &str = "player.seek";
    pub const PLAYBACK_GET: &str = "playback.get";
    pub const QUERY_PARSE: &str = "query.parse";
    pub const QUERY_RUN: &str = "query.run";
    pub const QUERY_TREE: &str = "query.tree";
    pub const QUERY_REVEAL: &str = "query.reveal";
    pub const QUERY_UNDER: &str = "query.under";
    pub const QUERY_ROW: &str = "query.row";
    pub const QUERY_HOLDINGS: &str = "query.holdings";
    pub const QUERY_RECORDING: &str = "query.recording";
    pub const QUERY_ABOUT: &str = "query.about";
    pub const QUERY_NAME: &str = "query.name";
    pub const QUERY_PLACES: &str = "query.places";
    pub const QUERY_ENTRIES: &str = "query.entries";
    pub const QUERY_COVERS: &str = "query.covers";
    pub const QUERY_LISTENED: &str = "query.listened";
    pub const QUEUE_GET: &str = "queue.get";
    pub const QUEUE_ENQUEUE: &str = "queue.enqueue";
    pub const QUEUE_SET: &str = "queue.set";
    pub const QUEUE_REMOVE: &str = "queue.remove";
    pub const QUEUE_CLEAR: &str = "queue.clear";
    pub const SET_SCOPE: &str = "settings.set_scope";
    pub const SUBSCRIBE: &str = "sub.subscribe";
    pub const UNSUBSCRIBE: &str = "sub.unsubscribe";

    /// §8.4's write surface. Listed here as well as on [`super::LibraryOp`]
    /// because `ALL` is what `engine.status` advertises and what the CLI matches
    /// a first word against — a method missing from it is a method no caller can
    /// discover or reach, which is how `library.scan` spent its first afternoon
    /// being parsed as a `:` line.
    pub const LIBRARY_APPLY: &str = "library.apply";
    pub const LIBRARY_IMPORT: &str = "library.import";
    pub const LIBRARY_SCAN: &str = "library.scan";
    pub const LIBRARY_RESTORE: &str = "library.restore";
    pub const LIBRARY_REMOVE: &str = "library.remove";
    pub const LIBRARY_COMPACT: &str = "library.compact";
    pub const LIBRARY_UNFILED: &str = "library.unfiled";
    pub const LIBRARY_EDIT: &str = "library.edit";

    /// Every method this engine answers. Used by `engine.status` and by the
    /// test that keeps the golden transcripts honest.
    pub const ALL: &[&str] = &[
        HELLO,
        EXEC_LINE,
        ENGINE_STATUS,
        ENGINE_SHUTDOWN,
        TIME_NOW,
        PLAYER_LOAD,
        PLAYBACK_GET,
        QUERY_PARSE,
        QUERY_RUN,
        QUERY_TREE,
        QUERY_REVEAL,
        QUERY_UNDER,
        PLAYER_TRANSPORT,
        PLAYER_SEEK,
        QUERY_ROW,
        QUERY_HOLDINGS,
        QUERY_RECORDING,
        QUERY_ABOUT,
        QUERY_NAME,
        QUERY_PLACES,
        QUERY_ENTRIES,
        QUERY_COVERS,
        QUERY_LISTENED,
        QUEUE_GET,
        QUEUE_ENQUEUE,
        QUEUE_SET,
        QUEUE_REMOVE,
        QUEUE_CLEAR,
        SET_SCOPE,
        SUBSCRIBE,
        UNSUBSCRIBE,
        LIBRARY_APPLY,
        LIBRARY_IMPORT,
        LIBRARY_SCAN,
        LIBRARY_RESTORE,
        LIBRARY_REMOVE,
        LIBRARY_COMPACT,
        LIBRARY_UNFILED,
        LIBRARY_EDIT,
    ];
}

impl Request {
    /// Turn a method and its params into a typed request.
    ///
    /// **The one parse boundary for the socket.** An unknown method is a
    /// `-32601` and a malformed payload is a `-32602` naming the field, so a
    /// caller never has to guess which of the two it hit.
    pub fn of(method: &str, params: Option<serde_json::Value>) -> Result<Self, RpcError> {
        fn typed<T: serde::de::DeserializeOwned>(
            method: &str,
            params: Option<serde_json::Value>,
        ) -> Result<T, RpcError> {
            // **Absent params are an empty object, not null.**
            //
            // A method whose fields are all optional should accept being called
            // with nothing — `sub.subscribe` documents exactly that, and the
            // first version refused it with "invalid type: null, expected
            // struct TopicsParams", which contradicted the doc comment two
            // screens above. Found by writing the golden transcript for it.
            //
            // Null is retried as `{}` rather than mapped up front so that a
            // method genuinely wanting a null payload still sees one.
            let value = params.unwrap_or(serde_json::Value::Null);
            let retry = value.is_null();
            serde_json::from_value(value)
                .or_else(|e| {
                    if retry {
                        serde_json::from_value(serde_json::Value::Object(Default::default()))
                            .map_err(|_| e)
                    } else {
                        Err(e)
                    }
                })
                .map_err(|e| RpcError::invalid_params("params", format!("{method}: {e}")))
        }

        match method {
            method::HELLO => Ok(Self::Hello(typed(method, params)?)),
            method::EXEC_LINE => {
                let p: ExecLineParams = typed(method, params)?;
                Ok(Self::ExecLine { line: p.line })
            }
            method::ENGINE_STATUS => Ok(Self::EngineStatus),
            method::ENGINE_SHUTDOWN => Ok(Self::EngineShutdown),
            method::TIME_NOW => Ok(Self::TimeNow),
            other if LibraryOp::of_method(other).is_some() => Ok(Self::Library {
                operation: LibraryOp::of_method(other).expect("just matched"),
                params: params.unwrap_or(serde_json::Value::Null),
            }),
            method::PLAYER_TRANSPORT => {
                #[derive(Deserialize)]
                struct P {
                    verb: Transport,
                }
                let p: P = typed(method, params)?;
                Ok(Self::PlayerTransport { verb: p.verb })
            }
            method::PLAYER_SEEK => {
                #[derive(Deserialize)]
                struct P {
                    to: Seek,
                }
                let p: P = typed(method, params)?;
                Ok(Self::PlayerSeek { to: p.to })
            }
            method::PLAYER_LOAD => {
                #[derive(Deserialize)]
                struct Load {
                    recording: String,
                }
                let p: Load = typed(method, params)?;
                Ok(Self::PlayerLoad {
                    recording: p.recording,
                })
            }
            // **`format` is optional and absent is not empty.** A caller that wants
            // fields gets fields; one that wants a line asks for it. An empty string
            // would have to mean one of those and would silently be the wrong one.
            method::PLAYBACK_GET => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    format: Option<String>,
                }
                let p: P = typed(method, params)?;
                Ok(Self::PlaybackGet { format: p.format })
            }
            method::QUERY_PARSE => {
                #[derive(Deserialize)]
                struct P {
                    text: String,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryParse { text: p.text })
            }
            method::QUERY_RUN => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    filter: Option<String>,
                    #[serde(default)]
                    limit: Option<usize>,
                    #[serde(default)]
                    cursor: Option<usize>,
                }
                // **All three optional**, so a bare `query.run` is "the first screenful of
                // everything" — which is what a picker opens with.
                let p: P = match params {
                    Some(_) => typed(method, params)?,
                    None => P {
                        filter: None,
                        limit: None,
                        cursor: None,
                    },
                };
                Ok(Self::QueryRun {
                    filter: p.filter,
                    limit: p.limit,
                    cursor: p.cursor,
                })
            }
            method::QUERY_TREE => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    view: Option<String>,
                    #[serde(default)]
                    levels: Option<Vec<String>>,
                    #[serde(default)]
                    limit: Option<usize>,
                    #[serde(default)]
                    cursor: Option<usize>,
                    #[serde(default)]
                    folds: Option<Folds>,
                    #[serde(default)]
                    anchor: Option<Anchor>,
                    #[serde(default)]
                    settle: Option<Vec<String>>,
                }
                let p: P = match params {
                    Some(_) => typed(method, params)?,
                    None => P {
                        view: None,
                        levels: None,
                        limit: None,
                        cursor: None,
                        folds: None,
                        anchor: None,
                        settle: None,
                    },
                };
                Ok(Self::QueryTree {
                    view: p.view,
                    levels: p.levels,
                    limit: p.limit,
                    cursor: p.cursor,
                    folds: p.folds,
                    anchor: p.anchor,
                    settle: p.settle,
                })
            }
            method::QUERY_REVEAL => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    view: Option<String>,
                    #[serde(default)]
                    levels: Option<Vec<String>>,
                    #[serde(default)]
                    folds: Option<Folds>,
                    #[serde(default)]
                    pattern: Option<String>,
                    #[serde(default)]
                    id: Option<gmuse_ids::copy::CopyId>,
                    #[serde(default)]
                    from: Option<usize>,
                    #[serde(default)]
                    backwards: Option<bool>,
                }
                let p: P = typed(method, params)?;
                // **One target or the other, said here rather than downstream.** Both would be
                // two questions in one call and neither is nothing to look for, and a handler
                // discovering that has already built a tree to find out.
                if p.pattern.is_some() == p.id.is_some() {
                    return Err(RpcError::invalid_params(
                        "pattern",
                        "exactly one of `pattern` and `id`",
                    ));
                }
                Ok(Self::QueryReveal {
                    view: p.view,
                    levels: p.levels,
                    folds: p.folds,
                    pattern: p.pattern,
                    id: p.id,
                    from: p.from,
                    backwards: p.backwards,
                })
            }
            method::QUERY_UNDER => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    view: Option<String>,
                    #[serde(default)]
                    levels: Option<Vec<String>>,
                    #[serde(default)]
                    path: Vec<String>,
                    #[serde(default)]
                    id: Option<gmuse_ids::copy::CopyId>,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryUnder {
                    view: p.view,
                    levels: p.levels,
                    path: p.path,
                    id: p.id,
                })
            }
            method::QUERY_ROW => {
                #[derive(Deserialize)]
                struct P {
                    ids: Vec<gmuse_ids::copy::CopyId>,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryRow { ids: p.ids })
            }
            method::QUERY_HOLDINGS => {
                #[derive(Deserialize)]
                struct P {
                    recordings: Vec<String>,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryHoldings {
                    recordings: p.recordings,
                })
            }
            method::QUERY_RECORDING => {
                #[derive(Deserialize)]
                struct P {
                    recording: String,
                    #[serde(default)]
                    here: Vec<String>,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryRecording {
                    recording: p.recording,
                    here: p.here,
                })
            }
            method::QUERY_ABOUT => {
                #[derive(Deserialize)]
                struct P {
                    focus: serde_json::Value,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryAbout { focus: p.focus })
            }
            method::QUERY_NAME => {
                #[derive(Deserialize)]
                struct P {
                    kind: String,
                    name: String,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryName {
                    kind: p.kind,
                    name: p.name,
                })
            }
            method::QUERY_PLACES => {
                #[derive(Deserialize)]
                struct P {
                    facet: String,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryPlaces { facet: p.facet })
            }
            method::QUERY_ENTRIES => {
                #[derive(Deserialize)]
                struct P {
                    collection: String,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryEntries {
                    collection: p.collection,
                })
            }
            method::QUERY_COVERS => {
                #[derive(Deserialize)]
                struct P {
                    focus: serde_json::Value,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueryCovers { focus: p.focus })
            }
            method::QUERY_LISTENED => Ok(Self::QueryListened),
            method::QUEUE_GET => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    from: Option<u64>,
                    #[serde(default)]
                    limit: Option<usize>,
                }
                // **Both optional, so a bare `queue.get` still works.** A client that asked
                // for the queue before this existed was asking for what plays next, and that
                // is what it still gets.
                let p: P = match params {
                    Some(_) => typed(method, params)?,
                    None => P {
                        from: None,
                        limit: None,
                    },
                };
                Ok(Self::QueueGet {
                    from: p.from,
                    limit: p.limit,
                })
            }
            method::QUEUE_ENQUEUE => {
                #[derive(Deserialize)]
                struct P {
                    recordings: Vec<String>,
                    #[serde(default)]
                    base_version: Option<u64>,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueueEnqueue {
                    recordings: p.recordings,
                    base_version: p.base_version,
                })
            }
            method::QUEUE_SET => {
                #[derive(Deserialize)]
                struct P {
                    recordings: Vec<String>,
                    base_version: u64,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueueSet {
                    recordings: p.recordings,
                    base_version: p.base_version,
                })
            }
            method::QUEUE_REMOVE => {
                #[derive(Deserialize)]
                struct P {
                    index: usize,
                    #[serde(default)]
                    base_version: Option<u64>,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueueRemove {
                    index: p.index,
                    base_version: p.base_version,
                })
            }
            method::QUEUE_CLEAR => {
                #[derive(Deserialize, Default)]
                struct P {
                    #[serde(default)]
                    base_version: Option<u64>,
                }
                let p: P = typed(method, params)?;
                Ok(Self::QueueClear {
                    base_version: p.base_version,
                })
            }
            method::SET_SCOPE => {
                #[derive(Deserialize)]
                struct P {
                    mode: String,
                }
                let p: P = typed(method, params)?;
                Ok(Self::SetScope { mode: p.mode })
            }
            method::SUBSCRIBE => {
                let p: TopicsParams = typed(method, params)?;
                Ok(Self::Subscribe {
                    topics: p.resolved(),
                })
            }
            method::UNSUBSCRIBE => {
                let p: TopicsParams = typed(method, params)?;
                Ok(Self::Unsubscribe {
                    topics: p.resolved(),
                })
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }

    /// The name this request arrived under.
    pub fn method(&self) -> &'static str {
        match self {
            Self::Hello(_) => method::HELLO,
            Self::ExecLine { .. } => method::EXEC_LINE,
            Self::EngineStatus => method::ENGINE_STATUS,
            Self::EngineShutdown => method::ENGINE_SHUTDOWN,
            Self::TimeNow => method::TIME_NOW,
            Self::PlayerLoad { .. } => method::PLAYER_LOAD,
            Self::PlayerTransport { .. } => method::PLAYER_TRANSPORT,
            Self::PlayerSeek { .. } => method::PLAYER_SEEK,
            Self::PlaybackGet { .. } => method::PLAYBACK_GET,
            Self::QueryParse { .. } => method::QUERY_PARSE,
            Self::QueryRun { .. } => method::QUERY_RUN,
            Self::QueryTree { .. } => method::QUERY_TREE,
            Self::QueryReveal { .. } => method::QUERY_REVEAL,
            Self::QueryUnder { .. } => method::QUERY_UNDER,
            Self::QueryRow { .. } => method::QUERY_ROW,
            Self::QueryHoldings { .. } => method::QUERY_HOLDINGS,
            Self::QueryRecording { .. } => method::QUERY_RECORDING,
            Self::QueryAbout { .. } => method::QUERY_ABOUT,
            Self::QueryName { .. } => method::QUERY_NAME,
            Self::QueryPlaces { .. } => method::QUERY_PLACES,
            Self::QueryEntries { .. } => method::QUERY_ENTRIES,
            Self::QueryCovers { .. } => method::QUERY_COVERS,
            Self::QueryListened => method::QUERY_LISTENED,
            Self::QueueGet { .. } => method::QUEUE_GET,
            Self::QueueEnqueue { .. } => method::QUEUE_ENQUEUE,
            Self::QueueSet { .. } => method::QUEUE_SET,
            Self::QueueRemove { .. } => method::QUEUE_REMOVE,
            Self::QueueClear { .. } => method::QUEUE_CLEAR,
            Self::SetScope { .. } => method::SET_SCOPE,
            Self::Subscribe { .. } => method::SUBSCRIBE,
            Self::Unsubscribe { .. } => method::UNSUBSCRIBE,
            Self::Library { operation, .. } => operation.method(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::code;

    fn of(method: &str, params: serde_json::Value) -> Result<Request, RpcError> {
        Request::of(method, Some(params))
    }

    #[test]
    fn every_named_method_parses_and_reports_its_own_name() {
        let cases = [
            (method::HELLO, serde_json::json!({"protocol":"gmuse/0"})),
            (method::EXEC_LINE, serde_json::json!({"line":"pause"})),
            (method::ENGINE_STATUS, serde_json::json!(null)),
            (method::ENGINE_SHUTDOWN, serde_json::json!(null)),
            (method::TIME_NOW, serde_json::json!(null)),
        ];
        for (name, params) in cases {
            let request = of(name, params).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(request.method(), name);
        }
    }

    /// `method::ALL` is what `engine.status` advertises, so a method that
    /// parses and is not listed would be undiscoverable, and one listed that
    /// does not parse would be a lie.
    #[test]
    fn the_advertised_list_is_exactly_what_parses() {
        for name in method::ALL {
            assert!(
                !matches!(
                    Request::of(name, Some(serde_json::json!({"protocol":"gmuse/0","line":"pause"}))),
                    Err(ref e) if e.code == code::METHOD_NOT_FOUND
                ),
                "{name} is advertised but not handled"
            );
        }
        assert_eq!(method::ALL.len(), 39, "a new method needs adding to ALL");
    }

    /// Asking for nothing asks for everything, so a client does not have to
    /// track the topic list to keep receiving what it always did.
    #[test]
    fn an_empty_topic_list_means_every_topic() {
        let request = of(method::SUBSCRIBE, serde_json::json!({})).unwrap();
        assert_eq!(
            request,
            Request::Subscribe {
                topics: crate::Topic::ALL.to_vec()
            }
        );
    }

    /// Absent params, not merely an empty object: `sub.subscribe` with nothing
    /// at all is the shape a client hand-typing at a socket sends.
    #[test]
    fn subscribing_with_no_params_at_all_means_every_topic() {
        assert_eq!(
            Request::of(method::SUBSCRIBE, None),
            Ok(Request::Subscribe {
                topics: crate::Topic::ALL.to_vec()
            })
        );
    }

    #[test]
    fn a_named_topic_list_is_taken_as_given() {
        let request = of(method::SUBSCRIBE, serde_json::json!({"topics":["queue"]})).unwrap();
        assert_eq!(
            request,
            Request::Subscribe {
                topics: vec![crate::Topic::Queue]
            }
        );
    }

    #[test]
    fn an_unknown_topic_is_invalid_params_rather_than_being_ignored() {
        let e = of(method::SUBSCRIBE, serde_json::json!({"topics":["weather"]})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
    }

    /// **Every library operation is in `ALL`.** It is what `engine.status`
    /// advertises and what the CLI matches a first word against, so one missing
    /// from it is one no caller can discover or reach — which is exactly what
    /// happened: `library.scan` typed at a terminal went to the `:` line's
    /// parser and came back `unknown command "library.scan"`.
    #[test]
    fn every_library_method_is_advertised() {
        for op in LibraryOp::ALL {
            assert!(
                method::ALL.contains(&op.method()),
                "{} is handled but not advertised",
                op.method()
            );
        }
    }

    /// §8.4's eight, each reachable and each reporting its own name.
    #[test]
    fn every_library_operation_is_a_method() {
        for op in LibraryOp::ALL {
            let request = Request::of(op.method(), Some(serde_json::json!({}))).unwrap();
            assert_eq!(request.method(), op.method());
            assert!(matches!(request, Request::Library { operation, .. } if operation == *op));
        }
    }

    /// **The read-only subcommands must not be reachable** — they read directly
    /// with no engine (I15), and several are reported-never-decided, which is
    /// not a thing to put behind an RPC that returns a result.
    #[test]
    fn no_read_only_subcommand_has_a_method() {
        for name in [
            "library.query",
            "library.render",
            "library.duplicates",
            "library.copies",
            "library.slots",
            "library.verify",
            "library.artists",
            "library.names",
            "library.tree",
        ] {
            let e = Request::of(name, None).unwrap_err();
            assert_eq!(
                e.code,
                code::METHOD_NOT_FOUND,
                "{name} must not be a write method"
            );
        }
    }

    #[test]
    fn the_long_running_ones_are_scan_and_compact() {
        let long: Vec<_> = LibraryOp::ALL
            .iter()
            .filter(|op| op.is_long_running())
            .map(|op| op.as_str())
            .collect();
        assert_eq!(long, vec!["scan", "compact"]);
    }

    #[test]
    fn an_unknown_method_is_not_found_rather_than_bad_params() {
        let e = of("deck.load", serde_json::json!({})).unwrap_err();
        assert_eq!(e.code, code::METHOD_NOT_FOUND);
        assert!(e.message.contains("deck.load"), "{}", e.message);
    }

    /// **The two failures are told apart.** A client that sent the wrong shape
    /// to a real method and one that invented a method need different fixes.
    #[test]
    fn a_known_method_with_the_wrong_shape_is_invalid_params() {
        let e = of(method::EXEC_LINE, serde_json::json!({"lien":"pause"})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
        assert_eq!(e.data.unwrap()["field"], "params");
    }

    #[test]
    fn a_method_that_takes_nothing_does_not_mind_absent_params() {
        assert_eq!(Request::of(method::TIME_NOW, None), Ok(Request::TimeNow));
    }

    /// A line is carried verbatim: it is parsed by the command language, and
    /// mangling it here would be a second parser in the place I4 forbids one.
    #[test]
    fn an_exec_line_is_carried_through_untouched() {
        let request = of(
            method::EXEC_LINE,
            serde_json::json!({"line":"  seek +10  "}),
        )
        .unwrap();
        assert_eq!(
            request,
            Request::ExecLine {
                line: "  seek +10  ".into()
            }
        );
    }
}

/// **Malformed input produces errors, never panics** — #215's standing
/// requirement for every parse boundary.
///
/// A hand-rolled generator rather than a fuzzing dependency: §12 says not to
/// add one without a Decision Log entry, and what this needs is coverage of the
/// shapes that actually reach a boundary — truncations of valid input, wrong
/// types in the right fields, and bytes that are not JSON at all. A random byte
/// string almost never gets past `serde_json` and so tests the same branch
/// repeatedly.
#[cfg(test)]
mod no_input_panics {
    use super::*;
    use crate::envelope::parse_line;

    /// A cheap deterministic sequence. Seeded per-case so a failure is
    /// reproducible from the test name alone.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    const SEEDS: &[&str] = &[
        r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":"gmuse/0"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"cmd.exec_line","params":{"line":"seek +10"}}"#,
        r#"{"jsonrpc":"2.0","method":"time.now"}"#,
        r#"{"jsonrpc":"2.0","id":"x","method":"engine.status","params":null}"#,
    ];

    /// Every prefix of a valid request. Truncation is what a closed socket and
    /// a half-flushed writer both look like.
    #[test]
    fn no_truncation_of_a_valid_request_panics() {
        for seed in SEEDS {
            for cut in 0..seed.len() {
                let _ = parse_line(&seed[..cut]);
            }
        }
    }

    /// One byte replaced, everywhere, with the characters that carry meaning in
    /// JSON. This is where a hand-written parser would fall over.
    #[test]
    fn no_single_byte_corruption_panics() {
        for seed in SEEDS {
            for at in 0..seed.len() {
                for byte in *br#""{}[]:,\0"# {
                    let mut bytes = seed.as_bytes().to_vec();
                    bytes[at] = byte;
                    if let Ok(line) = std::str::from_utf8(&bytes) {
                        let _ = parse_line(line);
                    }
                }
            }
        }
    }

    /// The typed boundary itself, with every method against every shape of
    /// params — including the ones that belong to a different method.
    #[test]
    fn no_method_and_params_combination_panics() {
        let shapes = [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!([]),
            serde_json::json!(0),
            serde_json::json!("line"),
            serde_json::json!({"line": 7}),
            serde_json::json!({"protocol": []}),
            serde_json::json!({"line": {"nested": {"deeply": true}}}),
        ];
        let methods = ["", "hello", "cmd.exec_line", "time.now", "nope", "hello\0"];
        for method in methods {
            for shape in &shapes {
                let _ = Request::of(method, Some(shape.clone()));
                let _ = Request::of(method, None);
            }
        }
    }

    /// Random bytes, as the belt to the braces above.
    #[test]
    fn no_random_line_panics() {
        let mut state = 0x2545_F491_4F6C_DD1D;
        for _ in 0..2_000 {
            let len = (xorshift(&mut state) % 64) as usize;
            let line: String = (0..len)
                .map(|_| {
                    // Weighted towards structural characters; uniform bytes are
                    // rejected by the first `serde_json` call every time.
                    const ALPHABET: &[u8] = br#"{}[]":,0123456789abcdefghijklmnop \n\t"#;
                    ALPHABET[(xorshift(&mut state) as usize) % ALPHABET.len()] as char
                })
                .collect();
            let _ = parse_line(&line);
        }
    }
}
