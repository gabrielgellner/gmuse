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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettingKey {
    AaaMode,
    Shuffle,
    Repeat,
    RepeatCurrent,
    Continue,
    SmartArtistSort,
    ResampleQuality,
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
            _ => return None,
        })
    }

    pub const ALL: [SettingKey; 7] = [
        SettingKey::AaaMode,
        SettingKey::Shuffle,
        SettingKey::Repeat,
        SettingKey::RepeatCurrent,
        SettingKey::Continue,
        SettingKey::SmartArtistSort,
        SettingKey::ResampleQuality,
    ];
}

/// A setting's value, already typed. Which variant is valid depends on the key,
/// and the parser is what enforces that.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Aaa(AaaMode),
    Quality(Quality),
}

impl std::fmt::Display for SettingValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingValue::Bool(b) => write!(f, "{b}"),
            SettingValue::Aaa(a) => write!(f, "{}", a.label()),
            SettingValue::Quality(q) => write!(f, "{}", quality_name(*q)),
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

/// Everything the application can be asked to do.
///
/// One enum for keys, the `:` line and the rc file, per §6.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    // navigation
    WinUp,
    WinDown,
    WinPageUp,
    WinPageDown,
    WinTop,
    WinBottom,
    WinActivate,
    WinToggle,
    WinRemove,
    WinUpdate,
    WinNextPane,
    WinExpandAll,
    WinCollapseAll,
    // playback
    PlayerPlay,
    PlayerPause,
    PlayerStop,
    PlayerNext,
    PlayerPrev,
    Seek(SeekSpec),
    Vol(VolSpec),
    // library
    Add {
        target: AddTarget,
        path: Option<PathBuf>,
    },
    Update,
    // state
    Set {
        key: SettingKey,
        value: SettingValue,
    },
    Toggle(SettingKey),
    Bind {
        ctx: Context,
        key: keys::Key,
        cmd: Box<Command>,
    },
    Unbind {
        ctx: Context,
        key: keys::Key,
    },
    // views
    View(ViewId),
    Search {
        pattern: String,
        dir: SearchDir,
    },
    SearchNext,
    SearchPrev,
    Quit,
}
