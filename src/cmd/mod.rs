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
pub mod keys;
pub mod parse;

use std::path::PathBuf;

use crate::audio::resample::Quality;
use crate::next::AaaMode;

/// Which view a binding applies to. `Common` is the fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    Common,
    Library,
    SortedLibrary,
    Playlist,
    Queue,
    Browser,
    Filters,
    Settings,
}

impl Context {
    pub fn name(self) -> &'static str {
        match self {
            Context::Common => "common",
            Context::Library => "library",
            Context::SortedLibrary => "sorted",
            Context::Playlist => "playlist",
            Context::Queue => "queue",
            Context::Browser => "browser",
            Context::Filters => "filters",
            Context::Settings => "settings",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "common" => Context::Common,
            "library" | "tree" => Context::Library,
            "sorted" | "sorted_library" => Context::SortedLibrary,
            "playlist" => Context::Playlist,
            "queue" => Context::Queue,
            "browser" => Context::Browser,
            "filters" => Context::Filters,
            "settings" => Context::Settings,
            _ => return None,
        })
    }
}

/// The views, by their binding digit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewId {
    Library = 1,
    Sorted = 2,
    Playlist = 3,
    Queue = 4,
    Browser = 5,
    Filters = 6,
    Settings = 7,
}

impl ViewId {
    pub fn from_number(n: u8) -> Option<Self> {
        Some(match n {
            1 => ViewId::Library,
            2 => ViewId::Sorted,
            3 => ViewId::Playlist,
            4 => ViewId::Queue,
            5 => ViewId::Browser,
            6 => ViewId::Filters,
            7 => ViewId::Settings,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDir {
    Forward,
    Backward,
}

/// Settings reachable from `:set` and `:toggle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingKey {
    AaaMode,
    Shuffle,
    Repeat,
    RepeatCurrent,
    Continue,
    SmartArtistSort,
    ResampleQuality,
    AlbumHeaders,
    RelativeNumbers,
    ReplayGain,
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
    pub fn name(self) -> &'static str {
        match self {
            SettingKey::AaaMode => "aaa_mode",
            SettingKey::Shuffle => "shuffle",
            SettingKey::Repeat => "repeat",
            SettingKey::RepeatCurrent => "repeat_current",
            SettingKey::Continue => "continue",
            SettingKey::SmartArtistSort => "smart_artist_sort",
            SettingKey::ResampleQuality => "resample_quality",
            SettingKey::AlbumHeaders => "album_headers",
            SettingKey::RelativeNumbers => "relative_numbers",
            SettingKey::ReplayGain => "replaygain",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "aaa_mode" => SettingKey::AaaMode,
            "shuffle" => SettingKey::Shuffle,
            "repeat" => SettingKey::Repeat,
            "repeat_current" => SettingKey::RepeatCurrent,
            "continue" => SettingKey::Continue,
            "smart_artist_sort" => SettingKey::SmartArtistSort,
            "resample_quality" => SettingKey::ResampleQuality,
            "album_headers" => SettingKey::AlbumHeaders,
            "relative_numbers" => SettingKey::RelativeNumbers,
            "replaygain" => SettingKey::ReplayGain,
            _ => return None,
        })
    }

    pub const ALL: &'static [SettingKey] = &[
        SettingKey::AaaMode,
        SettingKey::Shuffle,
        SettingKey::Repeat,
        SettingKey::RepeatCurrent,
        SettingKey::Continue,
        SettingKey::SmartArtistSort,
        SettingKey::ResampleQuality,
        SettingKey::AlbumHeaders,
        SettingKey::RelativeNumbers,
        SettingKey::ReplayGain,
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
    Aaa(AaaMode),
    Quality(Quality),
    ReplayGain(ReplayGainMode),
}

impl std::fmt::Display for SettingValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingValue::Bool(b) => write!(f, "{b}"),
            SettingValue::Aaa(a) => write!(f, "{}", a.label()),
            SettingValue::Quality(q) => write!(f, "{}", quality_name(*q)),
            SettingValue::ReplayGain(m) => write!(f, "{}", m.label()),
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
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    // navigation
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Activate,
    Expand,
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
    ExpandAll,
    CollapseAll,
    // playback
    Play,
    Pause,
    Stop,
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
        cmd: Box<Command>,
    },
    Unbind {
        ctx: Context,
        key: keys::Seq,
    },
    // views
    View(ViewId),
    ViewNext,
    ViewPrev,
    ViewLast,
    Catalog(crate::catalog::Spec),
    Playlist(PlaylistSpec),
    /// Turn the loaded smart playlist into an ordinary, editable one.
    ///
    /// A rule-defined list cannot have a track removed from it without growing
    /// an exception list bolted onto the rule, which is how these become
    /// unexplainable. Dropping the rule and keeping the tracks is the honest
    /// version of the same wish.
    Freeze,
    Rate(RateSpec),
    /// Narrow the library to what matches. `None` clears it.
    ///
    /// Transient and anonymous, as against a catalog, which is the same idea
    /// named and persistent — see §7.
    Filter(Option<crate::filter::Filter>),
    Search {
        pattern: String,
        dir: SearchDir,
    },
    SearchNext,
    SearchPrev,
    /// Show the keys that work here, in the bottom pane.
    ///
    /// The context's own bindings and `common`'s, which is what "what can I do
    /// here" actually means — view 7's flat list of every binding in every
    /// context answers a different question.
    Help,
    Quit,
}
