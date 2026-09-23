//! The command layer — DESIGN §6.
//!
//! `:bind`, the rc file, and the `:` command line are **one mechanism**. Every
//! one of them turns text into a [`Command`], and nothing downstream ever
//! handles a raw string: this is the parse-don't-validate boundary of the
//! application (§12).
//!
//! Execution is likewise one place — `ui::execute` — so all state mutation
//! happens in a single function rather than being spread across key handlers.

pub mod bind;
pub mod complete;
pub mod instruction;
pub mod keys;
pub mod parse;

use std::path::PathBuf;

use crate::audio::resample::Quality;
use crate::next::Scope;

/// Which view a binding applies to. `Common` is the fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    Common,
    Library,
    Playlist,
    Queue,
    /// View 7, the guides.
    Guide,
    /// View 8, the info panel (#126).
    Info,
    /// Transport mode's keymap (#33). Not a view — a *mode*, so it is the one
    /// context that does not fall through to `Common`.
    Transport,
}

impl Context {
    pub fn name(self) -> &'static str {
        match self {
            Context::Common => "common",
            Context::Library => "library",
            Context::Playlist => "playlist",
            Context::Queue => "queue",
            Context::Guide => "guide",
            Context::Info => "info",
            Context::Transport => "transport",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "common" => Context::Common,
            "library" | "tree" => Context::Library,
            "playlist" => Context::Playlist,
            "queue" => Context::Queue,
            "guide" => Context::Guide,
            "info" => Context::Info,
            "transport" => Context::Transport,
            _ => return None,
        })
    }

    /// Whether a key not bound here falls through to [`Context::Common`].
    ///
    /// **A view is an overlay on the common keymap; a mode is a keymap of its
    /// own.** vim's insert mode does not inherit normal mode's, and if this
    /// inherited, `j` would still navigate in the mode where it is volume —
    /// which is the whole thing the mode exists to stop.
    pub fn inherits_common(self) -> bool {
        !matches!(self, Context::Transport)
    }
}

/// What the keyboard currently means.
///
/// vim's arrangement, not inverted: **normal is navigation, transport is
/// entered.** Playback running is the common *state*, but the common
/// *interaction* is looking for something to play — transport is what your
/// hands want once you have found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Transport,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Transport => "transport",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "normal" => Mode::Normal,
            "transport" => Mode::Transport,
            _ => return None,
        })
    }

    pub const ALL: &'static [Mode] = &[Self::Normal, Self::Transport];
}

/// The views, by their binding digit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewId {
    Library = 1,
    Sorted = 2,
    Playlist = 3,
    Queue = 4,
    // 5 was the file browser, removed: gmuse navigates the library, not a
    // filesystem, and the digit is left unbound rather than renumbering 6 and 7.
    Filters = 6,
    Guide = 7,
    /// The info panel (#126).
    Info = 8,
}

impl ViewId {
    pub fn from_number(n: u8) -> Option<Self> {
        Some(match n {
            8 => ViewId::Info,
            1 => ViewId::Library,
            2 => ViewId::Sorted,
            3 => ViewId::Playlist,
            4 => ViewId::Queue,
            6 => ViewId::Filters,
            7 => ViewId::Guide,
            _ => return None,
        })
    }
}

/// `seek +10`, `seek -1:30`, `seek 90`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SeekSpec {
    Absolute(f64),
    Relative(f64),
}

/// `vol +10%`, `vol 50%`, `vol -5`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VolSpec {
    Absolute(f32),
    Relative(f32),
}

/// A pane a command can be aimed at, rather than acting on whatever has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneTarget {
    Tree,
    Tracks,
}

impl PaneTarget {
    pub fn name(self) -> &'static str {
        match self {
            PaneTarget::Tree => "tree",
            PaneTarget::Tracks => "tracks",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "tree" | "left" => PaneTarget::Tree,
            "tracks" | "right" => PaneTarget::Tracks,
            _ => return None,
        })
    }
}

/// Where `:add` puts things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddTarget {
    Library,
    Playlist,
    Queue,
    /// **The front of the queue — "play this next"** rather than the end of it.
    ///
    /// `add-queue` appends, which is right and is what cmus does: a queue is a list you are
    /// working through. But a queue is saved and restored, so it accumulates, and appending to
    /// a hundred entries puts a track somewhere you will reach next week. "Next" is the other
    /// thing a person means by queueing, and the two want different keys rather than a setting.
    ///
    /// **Index 0 is genuinely next**, because playing *consumes* the queue: `advance` pops the
    /// front once it commits. So this does not have to know what is sounding, and cannot
    /// insert ahead of it.
    ///
    /// **One list, not two.** `playback-programme.md` §"Reading it is a page" records the
    /// two-tier queue being replaced by a single programme; this is a position in that list,
    /// not a second tier being smuggled back.
    QueueNext,
    /// **Into the view itself** — assert whatever makes it true (#96).
    ///
    /// The idea no comparable player has: an edit to a view is an edit to the
    /// facts that made it true. iTunes and Roon make a derived list read-only;
    /// this can go further for one reason — membership is caused by *named*
    /// facts, so the translation is inspectable rather than magic.
    View,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDir {
    Forward,
    Backward,
}

/// Settings reachable from `:set` and `:toggle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingKey {
    Scope,
    Shuffle,
    Repeat,
    RepeatCurrent,
    Continue,
    SmartArtistSort,
    ResampleQuality,
    AlbumHeaders,
    RelativeNumbers,
    ReplayGain,
    WhichKeyDelay,
}

/// Which loudness tag to follow, per ReplayGain 2.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplayGainMode {
    Off,
    /// The track's own level. What shuffle wants: it jumps between masters.
    Track,
    /// The album's level, which preserves the *relative* levels within an
    /// album — the thing a continuous mix depends on.
    Album,
    /// Album when playing in order, track when shuffling.
    #[default]
    Auto,
}

impl ReplayGainMode {
    pub fn label(self) -> &'static str {
        match self {
            ReplayGainMode::Off => "off",
            ReplayGainMode::Track => "track",
            ReplayGainMode::Album => "album",
            ReplayGainMode::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "off" | "false" | "no" => ReplayGainMode::Off,
            "track" => ReplayGainMode::Track,
            "album" => ReplayGainMode::Album,
            "auto" => ReplayGainMode::Auto,
            _ => return None,
        })
    }

    /// Cycled by `:toggle replaygain`, in the order they are worth trying.
    pub fn cycle(self) -> Self {
        match self {
            ReplayGainMode::Off => ReplayGainMode::Auto,
            ReplayGainMode::Auto => ReplayGainMode::Track,
            ReplayGainMode::Track => ReplayGainMode::Album,
            ReplayGainMode::Album => ReplayGainMode::Off,
        }
    }
}

impl SettingKey {
    /// **Who owns this setting** — §6, #233.
    ///
    /// `None` means the engine can carry the change out on its own; `Some` is the
    /// reason it cannot, for a client to be told rather than left guessing.
    ///
    /// This exists because the old socket's `is_remotable` said *every* `set` was fine to
    /// send and `engine::control::apply` handled none of them, so
    /// `gmuse ctl set shuffle=true` answered `{"accepted": true}` and did nothing.
    /// Two lists that drift is the failure; one property of the setting is the
    /// fix, and `every_remotable_command_is_one_the_engine_carries_out` keeps the
    /// gate and the doing in step.
    pub fn not_the_engines(self) -> Option<&'static str> {
        match self {
            // **What plays next**, which §6 assigns to the control thread and
            // `engine::queue::Playing` holds.
            SettingKey::Scope
            | SettingKey::Shuffle
            | SettingKey::Repeat
            | SettingKey::RepeatCurrent => None,

            // **A view's own**, and meaningless to a process with no screen.
            SettingKey::AlbumHeaders => Some("album headers are a view's layout"),
            SettingKey::RelativeNumbers => Some("relative numbers are a view's gutter"),
            SettingKey::WhichKeyDelay => Some("the which-key delay is a view's timing"),
            SettingKey::SmartArtistSort => Some("smart artist sort is how a view files its rows"),

            // **The engine's, and not wired.** Said with the reason rather than
            // lumped in with a view's, because the answer changes: these want
            // plumbing, not a different owner.
            SettingKey::ReplayGain => {
                Some("replaygain decides a load's gain and the engine does not read it yet")
            }
            SettingKey::ResampleQuality => Some("the resampler is fixed when the stream opens"),
            // **Nothing reads it at all**, here or in the engine: `next.rs` takes
            // scope, shuffle, repeat and repeat_current, and there is no
            // `continue` in `NextInput`. Refusing it over the wire is honest
            // about a setting that does nothing anywhere.
            SettingKey::Continue => Some("nothing reads `continue` yet, in either process"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SettingKey::Scope => "scope",
            SettingKey::Shuffle => "shuffle",
            SettingKey::Repeat => "repeat",
            SettingKey::RepeatCurrent => "repeat_current",
            SettingKey::Continue => "continue",
            SettingKey::SmartArtistSort => "smart_artist_sort",
            SettingKey::ResampleQuality => "resample_quality",
            SettingKey::AlbumHeaders => "album_headers",
            SettingKey::RelativeNumbers => "relative_numbers",
            SettingKey::ReplayGain => "replaygain",
            SettingKey::WhichKeyDelay => "which_key_delay",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scope" => SettingKey::Scope,
            "shuffle" => SettingKey::Shuffle,
            "repeat" => SettingKey::Repeat,
            "repeat_current" => SettingKey::RepeatCurrent,
            "continue" => SettingKey::Continue,
            "smart_artist_sort" => SettingKey::SmartArtistSort,
            "resample_quality" => SettingKey::ResampleQuality,
            "album_headers" => SettingKey::AlbumHeaders,
            "relative_numbers" => SettingKey::RelativeNumbers,
            "replaygain" => SettingKey::ReplayGain,
            "which_key_delay" => SettingKey::WhichKeyDelay,
            _ => return None,
        })
    }

    pub const ALL: &'static [SettingKey] = &[
        SettingKey::Scope,
        SettingKey::Shuffle,
        SettingKey::Repeat,
        SettingKey::RepeatCurrent,
        SettingKey::Continue,
        SettingKey::SmartArtistSort,
        SettingKey::ResampleQuality,
        SettingKey::AlbumHeaders,
        SettingKey::RelativeNumbers,
        SettingKey::ReplayGain,
        SettingKey::WhichKeyDelay,
    ];
}

/// What `:rate` was asked to do.
///
/// Three states rather than an `Option<u8>`, because "report" and "clear" are
/// different requests and nesting them reads as a puzzle. Zero is not one of
/// them: a rating is 1 to 5 or absent, in the command language exactly as in
/// the filter language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateSpec {
    /// `:rate` — say what it is.
    Report,
    /// `:rate none` — unrate it.
    Clear,
    /// `:rate 4`.
    Set(u8),
}

/// A setting's value, already typed. Which variant is valid depends on the key,
/// and the parser is what enforces that.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Scope(Scope),
    Quality(Quality),
    ReplayGain(ReplayGainMode),
    /// A duration in milliseconds. The first setting that is a *number* rather
    /// than a name, and deliberately so: `off | short | long` would be a number
    /// wearing a disguise, and the next timing setting would reopen the
    /// question. The cost is that this is the one value `:` completion cannot
    /// enumerate — see `complete::setting_values`.
    Millis(u16),
}

impl std::fmt::Display for SettingValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingValue::Bool(b) => write!(f, "{b}"),
            SettingValue::Scope(a) => write!(f, "{}", a.label()),
            SettingValue::Quality(q) => write!(f, "{}", quality_name(*q)),
            SettingValue::ReplayGain(m) => write!(f, "{}", m.label()),
            SettingValue::Millis(ms) => write!(f, "{ms}"),
        }
    }
}

pub fn quality_name(q: Quality) -> &'static str {
    match q {
        Quality::Fast => "fast",
        Quality::Balanced => "balanced",
        Quality::High => "high",
    }
}

/// What `:playlist` was asked to do.
///
/// Subcommands rather than a bare name, so everything playlist-shaped is
/// discoverable from one verb and `:playlist <Tab>` shows the lot. `open`
/// takes a smart playlist or a saved one without caring which — the whole
/// point of #15 was that a rule and a list are both play *sources*, so they
/// share a vocabulary rather than colliding in two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistSpec {
    /// Fill view 3 from a rule (#15) or a saved `.m3u8` (#22).
    ///
    /// A *source*, not a narrowing — the library stays exactly as it was. The
    /// narrowing spelling of the same idea is `:catalog`.
    Open(String),
    /// Copy the open one under a new name. Not "write it out" — edits are
    /// already written through — but save-as, for forking a list or keeping a
    /// rule's current contents.
    Save(String),
    /// Start an empty playlist, so adding to one does not mean modifying an
    /// existing one.
    New(String),
    /// What there is to open. Also what a bare `:playlist` does.
    List,
}

/// Everything the application can be asked to do.
///
/// One enum for keys, the `:` line and the rc file, per §6.
/// A motion that uses the list's **structure** rather than counting rows.
///
/// The same eight ideas read in a tree and in a flat list, which is the point:
/// in view 1 a "sibling" is a row at the cursor's own depth, and in view 2 it
/// is the next album boundary — the divider you can already see. One concept,
/// two shapes, rather than a second set of keys for flat lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// `}` and `{` — the next or previous row at the cursor's own depth,
    /// stepping over everything nested under it.
    NextSibling,
    PrevSibling,
    /// `]]` and `[[` — the next or previous top-level row, whatever depth the
    /// cursor is at.
    NextSection,
    PrevSection,
    /// `][` and `[]` — the *end* of a top-level group rather than its start.
    /// vim's section motions are four keys, not two: `]]` `[[` land on starts
    /// and `][` `[]` on ends, so a section can be left from either side.
    NextSectionEnd,
    PrevSectionEnd,
    /// `-` and `[z` — the row this one is filed under.
    Parent,
    /// `]z` — the last row of the group this one is in.
    GroupEnd,
    /// `zj` and `zk` — the next or previous row that can be opened at all.
    NextFold,
    PrevFold,
}

impl Motion {
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "next-sibling" => Self::NextSibling,
            "prev-sibling" => Self::PrevSibling,
            "next-section" => Self::NextSection,
            "prev-section" => Self::PrevSection,
            "next-section-end" => Self::NextSectionEnd,
            "prev-section-end" => Self::PrevSectionEnd,
            "parent" => Self::Parent,
            "group-end" => Self::GroupEnd,
            "next-fold" => Self::NextFold,
            "prev-fold" => Self::PrevFold,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::NextSibling => "next-sibling",
            Self::PrevSibling => "prev-sibling",
            Self::NextSection => "next-section",
            Self::PrevSection => "prev-section",
            Self::NextSectionEnd => "next-section-end",
            Self::PrevSectionEnd => "prev-section-end",
            Self::Parent => "parent",
            Self::GroupEnd => "group-end",
            Self::NextFold => "next-fold",
            Self::PrevFold => "prev-fold",
        }
    }

    pub const ALL: &'static [Motion] = &[
        Self::NextSibling,
        Self::PrevSibling,
        Self::NextSection,
        Self::PrevSection,
        Self::NextSectionEnd,
        Self::PrevSectionEnd,
        Self::Parent,
        Self::GroupEnd,
        Self::NextFold,
        Self::PrevFold,
    ];
}

/// Where to put the cursor's row in its pane — vim's `zt`, `zz` and `zb`.
///
/// Not a fold operation, but it shares the `z` prefix because vim puts it
/// there: both are about what you can see rather than about where you are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Top,
    Centre,
    Bottom,
}

impl Place {
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "top" => Self::Top,
            "centre" | "center" => Self::Centre,
            "bottom" => Self::Bottom,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Top => "top",
            // British spelling to match the rest of the prose; `center` parses
            // too, because half the world will type it.
            Self::Centre => "centre",
            Self::Bottom => "bottom",
        }
    }

    pub const ALL: &'static [Place] = &[Self::Top, Self::Centre, Self::Bottom];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// `zo`, and `l` on a closed node.
    Open,
    /// `zc`, and `h`. On something already closed it closes the parent, so
    /// repeated presses walk up and out.
    Close,
    /// `za`, and `o`.
    Toggle,
    /// `zO` — this node and everything under it.
    OpenDeep,
    /// `zC` — this node and everything under it, so reopening does not spill
    /// the subtree you had expanded inside.
    CloseDeep,
    /// `zA` — `zO` or `zC` depending on which way this node is currently set.
    /// vim has the toggle for every fold operation that comes in a pair, and
    /// shipping `zO`/`zC` without it left the recursive trio one short.
    ToggleDeep,
    /// `zR` — every fold in the tree.
    All,
    /// `zM` — none of them.
    None,
    /// `zr` — one level deeper. Artists, then albums.
    More,
    /// `zm` — one level shallower.
    Less,
    /// `zx` — close every fold, then reopen just the ones that were holding
    /// the cursor's row. "Close all but where I am."
    Only,
}

impl Fold {
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "open" => Self::Open,
            "close" => Self::Close,
            "toggle" => Self::Toggle,
            "open-deep" => Self::OpenDeep,
            "close-deep" => Self::CloseDeep,
            "toggle-deep" => Self::ToggleDeep,
            "all" => Self::All,
            "none" => Self::None,
            "more" => Self::More,
            "less" => Self::Less,
            "only" => Self::Only,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Close => "close",
            Self::Toggle => "toggle",
            Self::OpenDeep => "open-deep",
            Self::CloseDeep => "close-deep",
            Self::ToggleDeep => "toggle-deep",
            Self::All => "all",
            Self::None => "none",
            Self::More => "more",
            Self::Less => "less",
            Self::Only => "only",
        }
    }

    /// Whether this form moves the whole tree's level, as against acting on
    /// one node. These are the ones that insert and remove rows *above* the
    /// cursor, so they are the ones whose screen position has to be held.
    pub fn levels(self) -> bool {
        matches!(self, Self::All | Self::None | Self::More | Self::Less)
    }

    /// Whether this form puts the cursor's row back on screen after closing
    /// what was around it.
    pub fn reveals(self) -> bool {
        matches!(self, Self::Only)
    }

    pub const ALL: &'static [Fold] = &[
        Self::Open,
        Self::Close,
        Self::Toggle,
        Self::OpenDeep,
        Self::CloseDeep,
        Self::ToggleDeep,
        Self::All,
        Self::None,
        Self::More,
        Self::Less,
        Self::Only,
    ];
}

/// A thing named on the `:` line — #129.
///
/// **The shape is parsed at the boundary and the name resolved later** (§12).
/// Which field was written is a fact about the text and settles here; *which*
/// artist it means is a fact about the library and cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    /// The exact form, `recording:0192…` — no library needed.
    Exactly(crate::model::focus::Focus),
    /// `artist="Bodzin"`, `in="Dance"`, `genre="Techno"`.
    Called(crate::model::about::Kind, String),
}

/// What `:view` was asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewTerm {
    /// `:view` alone — take the composition apart.
    Clear,
    /// `:view <n>` — drop one term by its position, 1-based as displayed.
    Drop(usize),
    /// `:view [+&-]<term>` — set, widen, narrow or exclude.
    ///
    /// Carries a [`crate::view::What`] rather than a `Filter`, because a term
    /// is not always a predicate: `in="Jazz"` names a collection and
    /// `saved="mornings"` names a definition (#119).
    Compose(crate::model::view::Op, Box<crate::view::What>, bool),
    /// `:view save <name>` — name what is composed, as a fact.
    Save(String),
    /// `:view levels <outer>,<inner>` — how the same set is drawn (#97).
    ///
    /// Presentation, not membership: the rows are the same either way, and
    /// this says how they are grouped. `:view levels` with no argument is the
    /// tree as it always was.
    Levels(crate::shelf::Levels),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    // navigation
    Up,
    Down,
    PageUp,
    PageDown,
    /// `^d` and `^u` — half a screen, recentred. vim's, and the pair actually
    /// used: half a page is the distance at which you can still see where you
    /// came from.
    HalfPageUp,
    HalfPageDown,
    Top,
    Bottom,
    Activate,
    /// Open or close what is under the cursor, or move the whole tree's fold
    /// level. vim's `z` family, spelled out as one command with an argument
    /// rather than nine commands, because they are nine forms of one idea.
    Fold(Fold),
    /// Scroll so the cursor's row sits at the top, middle or bottom.
    Place(Place),
    /// Move by the list's structure. Takes a count: `3}`.
    Go(Motion),
    /// Put the cursor on the playing track, opening whatever folds were hiding
    /// it. Answers "where is the thing I am listening to?", which is a question
    /// neither selection nor search asks.
    Playing,
    /// **Empty the queue.**
    ///
    /// A queue is appended to and drained by playing, so without this the only way to shorten
    /// one is `dd` per row — and a queue accumulates across sessions, because it is saved and
    /// restored. A hundred entries nobody remembers choosing is the ordinary case, not the
    /// pathological one.
    ///
    /// **Undoable like every other queue edit**, which is what makes it safe to reach for: it
    /// takes the same `undo::Key::Queue` snapshot `dd` and `e` do, so `u` puts it back.
    ClearQueue,
    /// Remove the item under the cursor. Takes a count: `3dd`.
    Remove,
    /// Copy rows into the register without removing them. Takes a count.
    ///
    /// vim's `yy`. The register is on the `Ui`, not on a playlist, because
    /// vim's registers are **global** — that is what makes yank here, switch
    /// there, put work at all.
    Yank,
    /// Copy from the cursor to the end of the list. vim's `Y`, mirroring `D`.
    YankToEnd,
    /// Put the register after the cursor (`p`) or before it (`P`).
    Put {
        before: bool,
    },
    /// Step back through this list's edits (`u`), or forward again (`^r`).
    Undo,
    Redo,
    /// Remove from the cursor to the end of the list.
    ///
    /// `D` in vim is delete-to-end-of-line. There is no "inside a line" here,
    /// so the row is the line and the *list* is what there is an end of —
    /// which makes the analogy hold rather than merely rhyme.
    RemoveToEnd,
    PaneNext,
    /// Close the fold under the cursor — `h`.
    ///
    /// There is no matching `FoldOpen`: opening *is* [`Command::Activate`],
    /// which `l` and `Enter` are both bound to, and on a track that means
    /// play. snacks spells it the same way — `l` is `confirm`, `h` is
    /// `explorer_close` — and the asymmetry is real rather than an oversight:
    /// closing is one act, while opening bottoms out in doing the thing.
    PaneLeft,
    PaneRight,
    /// Run a command against a named pane instead of the focused one.
    ///
    /// The wrapper rather than a variant per pane per motion: it composes with
    /// every existing command, and with the `:` line, for free.
    In {
        pane: PaneTarget,
        cmd: Box<Command>,
    },
    // playback
    Play,
    Pause,
    /// **Return to the reference point, or set it** — #229, D-14.
    ///
    /// The verb that absorbed `stop`. D-10's argument is that stop was never its
    /// own thing: it is pause plus a return to the reference, and with the
    /// reference defaulting to the top of the track that is exactly cue. So the
    /// player reading ("back to the start and wait") and the DJ reading ("mark
    /// here, or drop back to the mark") are one command, and which one happens
    /// is the engine's to decide from the run state.
    Cue,
    /// **Preview from the reference while a key is held** — #228, §5's third
    /// case and the one `KeyPhase` exists for.
    ///
    /// Momentary, which is a property of the *command* rather than of the
    /// binding: the view asks "is this what the key resolves to" and handles a
    /// release only then, so every other binding keeps the press-only path it
    /// has always had.
    CuePreview,
    Next,
    Prev,
    Seek(SeekSpec),
    Vol(VolSpec),
    // library
    Add {
        target: AddTarget,
        path: Option<PathBuf>,
    },
    Rescan,
    // state
    Set {
        key: SettingKey,
        value: SettingValue,
    },
    Toggle(SettingKey),
    Bind {
        ctx: Context,
        key: keys::Seq,
        /// **A list, because a key binds one** — #266.
        ///
        /// `:bind` parses exactly one and this is a one-element vector; a TOML
        /// list is the only thing that makes it longer, which is the macro
        /// `config.rs` has documented all along and never delivered.
        cmds: Vec<Command>,
    },
    Unbind {
        ctx: Context,
        key: keys::Seq,
    },
    // views
    View(ViewId),
    ViewNext,
    ViewPrev,
    /// `]b` and `[b` — step the view, **and a count repeats rather than
    /// selects**.
    ///
    /// Two commands rather than a key that reinterprets [`Command::ViewNext`],
    /// because the count's meaning is the difference between them and a
    /// `Command` has to carry its own meaning — a key that changed what a
    /// variant meant would put the semantics back in the keymap, which §12's
    /// "parse at the boundary" exists to prevent.
    ///
    /// `3gt` is view 3; `3]b` is three views forward. Both are vim: `gt` takes
    /// an absolute count in nvim and `]b` a repeat in unimpaired. Every other
    /// bracket motion here already repeats — `3]]` is three sections — so the
    /// surprising one would have been a bracket key that did not.
    ViewForward,
    ViewBack,
    /// `<leader>i` — show or hide the info panel beside the list (#273).
    ///
    /// **Not a new surface.** #275 gave windows their own view and #276 made the
    /// focus follow the cursor, which between them *are* a live detail pane —
    /// this is the one keystroke that arranges it, and closing it again. A
    /// second renderer was the thing to avoid, and the way to avoid it is to
    /// have no second surface at all.
    InfoWindow,
    /// `gq` — show or hide the programme in the bottom panel (#274).
    ///
    /// A panel and not a view: toggling it leaves the active projection alone,
    /// which is what lets you collect into the queue while looking at the
    /// library. vim's `:copen`, where the quickfix window appears and the
    /// buffer you were editing stays put.
    QueuePanel,
    /// `]q` and `[q` — step the cursor **within the programme**, from whatever
    /// projection is active.
    ///
    /// **Not next and previous track.** `z` and `v` are those, they are
    /// DJ-derived, and they take a deck as an operand
    /// (`transport-keymap-spec.md` §7). Two vocabularies for one verb is what
    /// that spec exists to prevent, so these move a cursor and play nothing.
    QueueNext,
    QueuePrev,
    /// `i` and `<Esc>` — which keymap the keyboard is in (#33).
    Mode(Mode),
    /// `50%` — seek to that much of the way through the track, as vim's `50%`
    /// goes to that much of the way through a file. Takes the count, like
    /// `12G`; bare, it means halfway.
    SeekPercent,
    ViewLast,

    Playlist(PlaylistSpec),
    /// Turn the loaded smart playlist into an ordinary, editable one.
    ///
    /// A rule-defined list cannot have a track removed from it without growing
    /// an exception list bolted onto the rule, which is how these become
    /// unexplainable. Dropping the rule and keeping the tracks is the honest
    /// version of the same wish.
    /// `:collect <name>` — file what the view holds into a collection.
    ///
    /// **The write the model never had** (#118). Collections have been facts
    /// since #92 and nothing in the player could make one: `add_to_collection`
    /// had callers only in tests, so the Dance collection had to be seeded by
    /// a tool pass reading a folder.
    Collect(String),
    /// `:file [name]` — file what is here into a crate, or pick one to file it into.
    ///
    /// **The per-thing write, beside `:collect`'s per-view one.** `:collect` files a whole
    /// composed view; this files what the cursor is on — the rows under it in a list, or
    /// what the page stands for in the info panel, where an artist fans out to their
    /// recordings. With no name it opens the crate picker, which is what `a` in the panel
    /// runs.
    File(Option<String>),
    /// `:export <path>` — write what view 3 holds as an `.m3u8`.
    ///
    /// **The only thing m3u is for** (#118). It was the playlist's storage,
    /// which made a path authoritative for what a list contained; a playlist
    /// is an ordered collection now, and m3u is how one leaves the building.
    Export(String),
    /// `:split` / `:vsplit` — a second pane, for looking at two lists at
    /// once (#97).
    ///
    /// What `g<Tab>` does not cover: it moves *between* two things, and this
    /// is for digging in one while collecting into the other.
    Split(crate::ui::Axis),
    /// `:only` — back to one pane.
    Only,
    /// Move the focus to the other pane.
    OtherPane,
    /// `:find` / `:find groups` — the fuzzy picker (#54).
    Find(crate::ui::picker::Source),
    /// `:freeze <name>` — a question becomes an answer somebody kept.
    ///
    /// Materialises the composed view, or re-runs what a frozen one was frozen
    /// from and says what changed (#95).
    Freeze(String),
    Rate(RateSpec),
    /// Narrow the library to what matches. `None` clears it.
    ///
    /// Transient and anonymous, as against a catalog, which is the same idea
    /// named and persistent — see §7.
    Filter(Option<crate::query::filter::Filter>),
    /// **One verb and three operators** — `query-language.md` §2.
    ///
    /// Spelled `:view` too, and the verb dispatches on its argument: a bare
    /// number is the numbered view it has always been, and anything else
    /// composes. That is not a permanent arrangement — #97 removes the
    /// numbered views, and when it does the number case goes with them — but
    /// it is better than a second verb for one idea, or than breaking
    /// `:view 3` to introduce it.
    ///
    /// `:filter x` is `&x` written the way it always was. The two that had
    /// nowhere to live are `+` and `-`, and #28's missing cell is the second:
    /// a persistent *narrowing*, which a catalog could not be because a
    /// catalog unions.
    Compose(ViewTerm),
    /// `:info` — what the library says about a thing (#126, #129).
    ///
    /// `None` is the row under the cursor, which is what the key does.
    Info(Option<Named>),
    /// Back to where the info panel came from — #127. vim's `^o`.
    Back,
    /// `:edit` — the composed query in `$EDITOR` (#99).
    ///
    /// The third tier of one language: simple things inline, composed things
    /// in an editor, and the escape hatch is the same text.
    Edit,
    Search {
        pattern: String,
        dir: SearchDir,
    },
    SearchNext,
    SearchPrev,
    /// vim's `:nohlsearch`, which the local nvim config puts on `<Esc>`. The
    /// match stays *found* — `n` still works — it just stops being painted.
    NoHighlight,
    /// Show the keys that work here, in the bottom pane.
    ///
    /// The context's own bindings and `common`'s, which is what "what can I do
    /// here" actually means — the index guide's flat list of every binding in every
    /// context answers a different question.
    Help,
    Quit,
    /// **Quit, and stop the engine** — `:qa`, which is vim's "quit all" and spelled its way.
    ///
    /// `:q` leaves the engine playing, which is a detach in tmux's sense and is usually what
    /// is wanted: attaching is the default (#233) and the engine outlives any one view. This
    /// is the other one.
    ///
    /// **Refused while somebody else is attached**, because stopping the engine ends playback
    /// for every client rather than only this one — vim's `:qa` refuses over an unsaved buffer
    /// for the same reason, and takes `!` the same way.
    QuitAll {
        /// `:qa!` — stop the engine even though another client is attached.
        force: bool,
    },
}
