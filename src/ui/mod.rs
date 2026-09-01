//! The terminal UI — DESIGN §8 `ui/`.
//!
//! M4 scope: views 1–5, navigation, `Tab`, `Space`. Read-only; nothing here
//! starts playback, which is M5's job.

pub mod fold;
pub mod list;
pub mod statusline;
pub mod theme;
pub mod tree;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};

use crate::audio::PlayerHandle;
use crate::cmd::bind::{self, Bindings};
use crate::cmd::keys::Key;
use crate::cmd::{
    AddTarget, Command, Fold, Motion, SearchDir, SeekSpec, SettingKey, SettingValue, ViewId,
    VolSpec, parse,
};
use crate::library::Library;
use crate::next::{self, ActiveSource, Playback, Source};
use crate::playlist::{Playlist, Queue};
use crate::store::{Store, TrackId};
use tree::{Pane, TreeState};

/// The five views M4 covers. cmus numbers them, and so do we — the digits are
/// the binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Tree = 1,
    Sorted = 2,
    Playlist = 3,
    Queue = 4,
    Browser = 5,
    Filters = 6,
    /// Prose: how to do the things, as against which key does what — plus the
    /// generated index of every binding, which is what view 7 used to be (#47).
    Guide = 7,
}

impl View {
    pub fn title(self) -> &'static str {
        match self {
            View::Tree => "library",
            View::Sorted => "sorted",
            View::Playlist => "playlist",
            View::Queue => "queue",
            View::Browser => "browser",
            View::Filters => "scope",
            View::Guide => "guide",
        }
    }

    pub fn from_id(id: ViewId) -> Self {
        match id {
            ViewId::Library => View::Tree,
            ViewId::Sorted => View::Sorted,
            ViewId::Playlist => View::Playlist,
            ViewId::Queue => View::Queue,
            ViewId::Browser => View::Browser,
            ViewId::Filters => View::Filters,
            ViewId::Guide => View::Guide,
        }
    }

    /// The binding context this view looks up keys in.
    /// The next view in the tab strip, wrapping.
    pub fn next(self) -> Self {
        let all = Self::ALL;
        let i = all.iter().position(|v| *v == self).unwrap_or(0);
        all[(i + 1) % all.len()]
    }

    pub fn prev(self) -> Self {
        let all = Self::ALL;
        let i = all.iter().position(|v| *v == self).unwrap_or(0);
        all[(i + all.len() - 1) % all.len()]
    }

    pub const ALL: &'static [View] = &[
        View::Tree,
        View::Sorted,
        View::Playlist,
        View::Queue,
        View::Browser,
        View::Filters,
        View::Guide,
    ];

    pub fn context(self) -> crate::cmd::Context {
        use crate::cmd::Context as C;
        match self {
            View::Tree => C::Library,
            View::Sorted => C::SortedLibrary,
            View::Playlist => C::Playlist,
            View::Queue => C::Queue,
            View::Browser => C::Browser,
            View::Filters => C::Filters,
            View::Guide => C::Guide,
        }
    }
}

/// What the open prompt line is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Command,
    Search(SearchDir),
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    pub buffer: String,
    /// The completion menu: `None` when closed, `Some(vec![])` when open with
    /// nothing matching.
    ///
    /// An `Option` rather than a `Vec` whose emptiness means "closed", because
    /// those are different states: typing past the last match should leave the
    /// menu up saying so, not silently dismiss it and look like the keystroke
    /// was swallowed.
    pub menu: Option<Vec<String>>,
    /// Which candidate the menu is on. Meaningless when the menu is closed or
    /// empty; there is otherwise always a selection, because `C-y` acts on one
    /// whether or not it has been moved — so the menu has to show which.
    pub selected: usize,
}

impl Prompt {
    /// The candidates on show. Empty covers both "closed" and "nothing
    /// matches" — the distinction only matters to the renderer.
    pub fn candidates(&self) -> &[String] {
        self.menu.as_deref().unwrap_or_default()
    }

    /// Move the menu selection by one, wrapping.
    fn step(&mut self, forward: bool) {
        let n = self.candidates().len();
        if n == 0 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % n
        } else {
            (self.selected + n - 1) % n
        };
    }

    /// Replace the word under construction with `candidate`, leaving a
    /// trailing space so the next word can be typed straight away.
    fn accept(&mut self, candidate: &str) {
        let last = self
            .buffer
            .rsplit(char::is_whitespace)
            .next()
            .unwrap_or("")
            .to_string();
        // `set key=value` completes the value half, which is inside the word
        // rather than after a space — so the tail to replace is what follows
        // the `=`, not the whole word.
        let keep = match last.rsplit_once('=') {
            Some((head, _)) => self.buffer.len() - last.len() + head.len() + 1,
            None => self.buffer.len() - last.len(),
        };
        self.buffer.truncate(keep);
        self.buffer.push_str(candidate);
        self.buffer.push(' ');
        self.dismiss();
    }

    /// Close the menu, leaving the line alone.
    fn dismiss(&mut self) {
        self.menu = None;
        self.selected = 0;
    }

    pub fn sigil(&self) -> char {
        match self.kind {
            PromptKind::Command => ':',
            PromptKind::Search(SearchDir::Forward) => '/',
            PromptKind::Search(SearchDir::Backward) => '?',
        }
    }
}

/// A directory listing for view 5.
#[derive(Debug, Default)]
pub struct BrowserState {
    pub cwd: PathBuf,
    pub entries: Vec<PathBuf>,
    pub selected: usize,
}

impl BrowserState {
    pub fn at(dir: PathBuf) -> Self {
        let mut b = BrowserState {
            cwd: dir,
            ..Default::default()
        };
        b.refresh();
        b
    }

    /// Directories first, then files, each alphabetical — the order that makes
    /// a browser navigable rather than a lucky dip.
    pub fn refresh(&mut self) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(read) = std::fs::read_dir(&self.cwd) {
            for entry in read.flatten() {
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
                {
                    continue;
                }
                if path.is_dir() { &mut dirs } else { &mut files }.push(path);
            }
        }
        dirs.sort();
        files.sort();
        self.entries = Vec::with_capacity(dirs.len() + files.len() + 1);
        if self.cwd.parent().is_some() {
            self.entries.push(self.cwd.join(".."));
        }
        self.entries.append(&mut dirs);
        self.entries.append(&mut files);
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
    }

    fn enter(&mut self) {
        let Some(path) = self.entries.get(self.selected).cloned() else {
            return;
        };
        if path.file_name().and_then(|n| n.to_str()) == Some("..") {
            if let Some(parent) = self.cwd.parent() {
                self.cwd = parent.to_path_buf();
                self.selected = 0;
                self.refresh();
            }
        } else if path.is_dir() {
            self.cwd = path;
            self.selected = 0;
            self.refresh();
        }
    }
}

/// Which playlist view 3 has open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Open {
    /// A rule (#15). Editing is refused — an exception list bolted onto a rule
    /// is how these become unexplainable — and `:freeze` is the way out.
    Rule(String),
    /// A saved `.m3u8` (#22). Edits are written straight through.
    Saved(String),
}

impl Open {
    pub fn name(&self) -> &str {
        match self {
            Open::Rule(n) | Open::Saved(n) => n,
        }
    }

    pub fn is_rule(&self) -> bool {
        matches!(self, Open::Rule(_))
    }
}

/// One row of view 3's left pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistRow {
    pub name: String,
    /// A track count, or the rule for a smart one — the same thing view 6
    /// shows for a catalog, and for the same reason: a name alone does not say
    /// what you are about to open.
    pub detail: String,
    /// Defined by a rule rather than saved as a file.
    pub rule: bool,
}

/// Everything the UI draws and navigates.
pub struct Ui {
    pub view: View,
    pub tree: TreeState,
    pub sorted: usize,
    pub playlist_sel: usize,
    pub queue_sel: usize,
    pub browser: BrowserState,
    pub status: String,
    /// Views 3 and 4 live here rather than in the application, because the UI
    /// is what mutates them.
    pub playlist: Playlist,
    pub queue: Queue,
    pub playback: Playback,
    /// Shown briefly after a volume change, so the level is visible without a
    /// permanent indicator taking up room.
    pub volume_hint: Option<f32>,
    pub bindings: Bindings,
    /// What `<leader>` stands for. Substituted when a binding is inserted, so
    /// the lookup path never has to know about it.
    pub leader: crate::cmd::keys::Key,
    /// The `:` or `/` line, when one is open.
    pub prompt: Option<Prompt>,
    /// The keys-that-work-here popup, when it is up. Dismissed by any key.
    pub help: Option<Vec<String>>,
    /// Per-list edit history, for `u` and `^r`.
    pub undo: crate::undo::Undo,
    /// vim's unnamed register, holding whole tracks.
    ///
    /// On the `Ui` rather than on a playlist, because vim's registers are
    /// **global**: that is what makes `yy` here, open another playlist, `p`
    /// there work at all, and it is the whole reason this is worth having over
    /// a nudge key.
    ///
    /// One register, unnamed. vim has named and numbered ones too; whether
    /// they earn their surface when the "text" is tracks is a question best
    /// answered by missing them.
    pub register: Vec<TrackId>,
    /// The last search, for `n` and `N`.
    pub last_search: Option<(String, SearchDir)>,
    /// What is highlighted: the pattern in force, from `/` or the last search.
    /// Cleared by `Esc`, as vim's `nohlsearch` does.
    pub highlight: Option<crate::text::Needle>,
    /// Where the cursor was when a `/` prompt opened.
    ///
    /// vim's incsearch is a **preview**: it moves the view while you type and
    /// puts you back if you cancel. Only `Enter` commits. Without this an
    /// abandoned search is an unintentional jump.
    search_origin: Option<usize>,
    /// Keys typed so far that have not resolved to a binding, and the count
    /// prefix in front of them.
    /// Which keymap the keyboard is in (#33). Not persisted across a restart —
    /// see the Decision Log.
    pub mode: crate::cmd::Mode,
    pub pending: Vec<Key>,
    /// When the chord in `pending` began — **not** when its last key arrived.
    ///
    /// which-key charges its delay once per chord rather than once per key
    /// (`view.lua:99`: an already-open popup redraws with no delay at all), and
    /// keeping the *first* key's instant is what implements that: the elapsed
    /// time only grows, so once the popup is up, going deeper cannot take it
    /// away again.
    pending_since: Option<Instant>,
    /// How far the popup in the bottom slot is scrolled, in laid-out rows.
    ///
    /// One field for both tenants — `<leader>?` and the prefix panel — because
    /// they are one slot and only ever one of them is up. Reset wherever the
    /// content changes, so a fresh popup always opens at its top.
    help_scroll: usize,
    /// The popup's visible rows and its full height as of the last frame, so
    /// `^d` can move by half a screen of *this* popup and stop at its end.
    /// A `Cell` for the same reason `body_height` is one: only `draw` knows.
    help_view: std::cell::Cell<(usize, usize)>,
    /// How long a prefix must be held before its popup is drawn. Zero shows it
    /// on the next frame, which is what this did before it was configurable.
    pub which_key_delay: Duration,
    pub count: Option<usize>,
    /// The view before the current one, for `g<Tab>`.
    pub previous_view: View,
    /// Show a relative-distance gutter beside list rows.
    pub relative_numbers: bool,
    /// Session log. Disabled unless `--log` was given.
    pub audit: crate::audit::Audit,
    /// Where to write "where I was". `None` disables resumption entirely.
    pub session_path: Option<PathBuf>,
    /// Play counts, ratings and first-seen dates — the folded state.
    ///
    /// Held in memory so the UI answers immediately; what makes it durable is
    /// the event appended beside each change, not this.
    pub history: crate::history::History,
    /// Where to append events. `None` disables recording entirely.
    ///
    /// Nothing here ever rewrites `history.toml`: the base is immutable, and
    /// that is what lets two machines share a directory without one's whole
    /// file losing to the other's (#26).
    pub events_path: Option<PathBuf>,
    /// The current track has already been counted, so a long listen does not
    /// count twice. Cleared whenever a different track becomes audible.
    counted_current: bool,
    /// A restored track has been loaded but its seek has not landed yet.
    ///
    /// Loading emits `Loaded`, which is a save point — and at that instant the
    /// position is still 0, because the decoder has not processed the seek
    /// that follows. Saving there would overwrite the very position being
    /// resumed, losing it to any crash before the next save.
    resuming: bool,
    /// Underruns already logged, so only new ones are recorded.
    seen_underruns: u32,
    /// The track handed to the player for a gapless splice, held until the
    /// playhead reaches it. It is not `playback.current` yet — the listener is
    /// still hearing the track before it — but the pool has already moved on,
    /// so this is where the answer lives in between.
    pending_next: Option<TrackId>,
    /// Cursor in view 6, the catalog list.
    pub catalog_sel: usize,
    /// Which guide is open, and how far down it we are.
    pub guide_sel: usize,
    pub guide_scroll: usize,
    /// The last drawn guide's line count and visible height.
    ///
    /// A `Cell` because only `draw` can know them — they depend on the pane's
    /// width, which nothing outside a frame has. Guessing instead was worse
    /// than it looked: an over-estimate let `G` reach the end but left the
    /// cursor tens of rows past anything visible, so `k` did nothing until it
    /// had walked back through them.
    pub guide_view: std::cell::Cell<(usize, usize)>,
    /// The height of the body area as of the last frame, borders excluded.
    ///
    /// **Every split in this UI is side by side**, so a pane is exactly as
    /// tall as the body — which makes this one number right for all of them,
    /// including the three panes that draw as a `Paragraph` and have no
    /// `Scroll` to record a height on: the playlist list, the scope view, and
    /// the guide body.
    ///
    /// A `Cell` for the same reason `guide_view` is one: only `draw` knows it.
    body_height: std::cell::Cell<usize>,
    /// How far each list is scrolled. One per list rather than one shared:
    /// switching views must not scroll the view you came from.
    pub tree_scroll: Scroll,
    pub sorted_scroll: Scroll,
    pub playlist_scroll: Scroll,
    pub queue_scroll: Scroll,
    pub browser_scroll: Scroll,
    pub guides_scroll: Scroll,
    /// Which half of view 7 has focus: the list of guides, or the guide.
    pub guide_pane: Pane,
    /// Cursor in view 3's left pane, the list of playlists.
    pub playlists_sel: usize,
    /// Which half of view 3 has focus: the set, or the open one.
    ///
    /// `Pane::Tree` is the list of playlists and `Pane::Tracks` its contents,
    /// named for view 1's panes because they are the same idea — pick on the
    /// left, see it on the right — and `tab`, `pane-left` and `in <pane>` all
    /// work here for free.
    pub playlist_pane: Pane,
    /// What view 7 shows, rebuilt when it is entered or when a playlist is
    /// written.
    ///
    /// Cached rather than derived per frame: counting a saved playlist's
    /// tracks means reading the file, and doing that on every redraw would be
    /// I/O at the UI tick rate.
    pub playlist_rows: Vec<PlaylistRow>,
    pub theme: theme::Theme,
    /// How the now-playing description is written. Parsed at config load; the
    /// status line only renders it.
    pub now_playing: crate::format::Template,
    /// The transport panel's headline (#34).
    pub transport_line: crate::format::Template,
    /// How a track row is written in the library tree.
    ///
    /// Its own template because a track sits beside an artist row there and
    /// has to read differently — the same reason a file explorer icons files
    /// apart from folders.
    pub track_line: crate::format::Template,
    /// The library projection. Owned rather than borrowed, because changing
    /// the active catalogs — or `smart_artist_sort` — rebuilds it.
    pub library: Library,
    pub catalogs: crate::catalog::Catalogs,
    /// Smart playlists, from the config (§7, issue #15).
    pub playlists: crate::smart::SmartPlaylists,
    /// Saved playlists on disk, under `data_dir/playlists` (#22).
    ///
    /// `None` when there is nowhere to keep them, which is the same guard the
    /// event log uses — the feature is simply absent rather than half-working.
    pub saved: Option<crate::playlist::Playlists>,
    /// What view 3 is showing, if anything.
    ///
    /// The *name*, not the tracks — those live in `playlist` like any other
    /// list. One enum rather than two mutually exclusive `Option`s, because
    /// a rule and a file cannot both be open and a pair of them could say so.
    pub open: Option<Open>,
    /// A transient narrowing on top of the active catalogs. Anonymous and
    /// not saved — a named, persistent one is a catalog (§7).
    pub filter: Option<crate::filter::Filter>,
    /// Keep playing past the end of the current album/artist pool. Stored here
    /// because it is a setting the user toggles, not a property of a track.
    pub continue_playing: bool,
    /// Divide track lists by album. cmus does; the alternative is one
    /// undifferentiated column.
    pub album_headers: bool,
    pub smart_artist_sort: bool,
    /// Which loudness tag to follow. Harmless when files are untagged: the
    /// gain is then 1.0 and nothing changes.
    pub replaygain: crate::cmd::ReplayGainMode,
    /// When the transient `status` message should stop being shown. Without
    /// this a one-off notice sits in the status line for the rest of the
    /// session, hiding what is actually playing.
    status_until: Option<Instant>,
    quit: bool,
}

impl Ui {
    pub fn new(start_dir: PathBuf, library: Library) -> Self {
        let mut ui = Self::bare(start_dir, library);
        // Resolve the placeholder the defaults were written with, so the
        // shipped `<leader> ?` works before any config is read.
        ui.set_leader(crate::config::default_leader());
        ui
    }

    fn bare(start_dir: PathBuf, library: Library) -> Self {
        Self {
            view: View::Tree,
            tree: TreeState::default(),
            tree_scroll: Scroll::default(),
            sorted_scroll: Scroll::default(),
            playlist_scroll: Scroll::default(),
            queue_scroll: Scroll::default(),
            browser_scroll: Scroll::default(),
            guides_scroll: Scroll::default(),
            sorted: 0,
            playlist_sel: 0,
            queue_sel: 0,
            browser: BrowserState::at(start_dir),
            status: String::new(),
            playlist: Playlist::new(),
            playlists: crate::smart::SmartPlaylists::default(),
            saved: None,
            open: None,
            queue: Queue::new(),
            playback: Playback::default(),
            volume_hint: None,
            bindings: bind::defaults(),
            leader: crate::config::default_leader(),
            help: None,
            prompt: None,
            undo: crate::undo::Undo::default(),
            register: Vec::new(),
            last_search: None,
            highlight: None,
            search_origin: None,
            mode: crate::cmd::Mode::default(),
            pending: Vec::new(),
            pending_since: None,
            help_scroll: 0,
            help_view: std::cell::Cell::new((0, 0)),
            which_key_delay: Duration::from_millis(DEFAULT_WHICH_KEY_DELAY_MS),
            count: None,
            previous_view: View::Tree,
            relative_numbers: false,
            pending_next: None,
            session_path: None,
            history: crate::history::History::default(),
            events_path: None,
            counted_current: false,
            resuming: false,
            audit: crate::audit::Audit::disabled(),
            seen_underruns: 0,
            playlists_sel: 0,
            playlist_pane: Pane::Tree,
            playlist_rows: Vec::new(),
            catalog_sel: 0,
            guide_sel: 0,
            guide_scroll: 0,
            guide_view: std::cell::Cell::new((0, 0)),
            body_height: std::cell::Cell::new(0),
            guide_pane: Pane::Tree,
            theme: theme::Theme::default(),
            now_playing: crate::format::Template::default(),
            transport_line: crate::format::Template::transport_default(),
            track_line: crate::format::Template::parse("{track:2}  {display_title}  {duration}")
                .expect("a literal template"),
            library,
            catalogs: crate::catalog::Catalogs::default(),
            filter: None,
            continue_playing: true,
            album_headers: true,
            smart_artist_sort: true,
            replaygain: crate::cmd::ReplayGainMode::default(),
            status_until: None,
            quit: false,
        }
    }

    /// What completion can offer, from the running application.
    /// History, the clock, and the shuffle seed, for the sort language.
    pub fn sort_env(&self) -> crate::sort::SortEnv<'_> {
        crate::sort::SortEnv {
            env: crate::filter::Env {
                history: Some(&self.history),
                now: crate::history::now(),
            },
            smart_artist_sort: self.smart_artist_sort,
            // Shared with shuffle, so `sort = "random"` gives a new order on
            // the same occasions a shuffled pool does.
            seed: self.playback.rng_state,
        }
    }

    /// Set the leader, re-keying every binding written with `<leader>`.
    ///
    /// The defaults are built by `bind::defaults()`, which cannot know the
    /// leader, so the placeholder survives into the table and is resolved
    /// here. Called once at startup and again if the config names one — a
    /// binding added later goes through `execute`, which resolves as it
    /// inserts.
    pub fn set_leader(&mut self, leader: crate::cmd::keys::Key) {
        self.leader = leader;
        let unresolved: Vec<(crate::cmd::Context, crate::cmd::keys::Seq)> = self
            .bindings
            .keys()
            .filter(|(_, seq)| seq.contains(&crate::cmd::keys::LEADER))
            .cloned()
            .collect();
        for key in unresolved {
            if let Some(cmd) = self.bindings.remove(&key) {
                let resolved = crate::cmd::keys::resolve_leader(&key.1, leader);
                self.bindings.insert((key.0, resolved), cmd);
            }
        }
    }

    pub fn completion_world(&self) -> crate::cmd::complete::World {
        crate::cmd::complete::World {
            catalogs: self
                .catalogs
                .defined()
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            playlists: known_playlists(self),
            bound: bind::sorted(&self.bindings)
                .into_iter()
                .map(|(ctx, key, _)| (ctx, key))
                .collect(),
            numbers: vec![(
                SettingKey::WhichKeyDelay,
                self.which_key_delay.as_millis().to_string(),
            )],
        }
    }

    /// The keys typed so far, for the status line — vim's `showcmd`.
    ///
    /// Derived rather than stored. An earlier version wrote this into
    /// `status`, which meant a completed chord left its own indicator behind
    /// for ever and hid the now-playing line.
    pub fn pending_label(&self) -> Option<String> {
        if self.pending.is_empty() && self.count.is_none() {
            return None;
        }
        let keys = crate::cmd::keys::format_seq(&self.pending);
        Some(match self.count {
            Some(n) => format!("{n}{keys}"),
            None => keys,
        })
    }

    /// Show a message for a few seconds, then get out of the way.
    pub fn notify(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_until = Some(Instant::now() + Duration::from_secs(4));
    }

    /// How long the loop may wait for a keystroke.
    ///
    /// The ordinary tick, unless a prefix is part-way through its delay — in
    /// which case only what is left of it, so the popup appears when it is due
    /// rather than when the tick next comes round.
    fn tick(&self, now: Instant) -> Duration {
        const TICK: Duration = Duration::from_millis(200);
        if self.pending.is_empty() {
            return TICK;
        }
        // Once it is due there is nothing left to wait for, so fall back to the
        // ordinary tick. Returning the remaining zero would spin the loop for
        // as long as the popup stayed up.
        let Some(started) = self.pending_since else {
            return TICK;
        };
        match self
            .which_key_delay
            .checked_sub(now.saturating_duration_since(started))
        {
            Some(left) if !left.is_zero() => left.min(TICK),
            _ => TICK,
        }
    }

    /// The keymap a key is looked up in: the mode's when there is one,
    /// otherwise the view's.
    ///
    /// A mode outranks a view because it is the coarser statement — "these
    /// keys mean playback" is true whichever list you happen to be looking at.
    pub fn context(&self) -> crate::cmd::Context {
        match self.mode {
            crate::cmd::Mode::Normal => self.view.context(),
            crate::cmd::Mode::Transport => crate::cmd::Context::Transport,
        }
    }

    fn expire_status(&mut self) {
        if let Some(until) = self.status_until
            && Instant::now() >= until
        {
            self.status.clear();
            self.status_until = None;
            self.volume_hint = None;
        }
    }

    /// The track the cursor is on in the current view, if any.
    pub fn selected_track(&self, ctx: &Context<'_>) -> Option<TrackId> {
        match self.view {
            // The row itself when it is a track, and otherwise the first
            // track underneath — so `x` on a closed album plays it from the
            // top rather than doing nothing.
            View::Tree => self
                .tree
                .selected_track(&self.library, ctx.store)
                .or_else(|| {
                    self.tree
                        .tracks_for(&self.library, ctx.store)
                        .first()
                        .copied()
                }),
            View::Sorted => self.library.sorted().get(self.sorted).copied(),
            View::Playlist => self.playlist.entries().get(self.playlist_sel).copied(),
            View::Queue => self.queue.entries().nth(self.queue_sel).copied(),
            View::Browser | View::Filters | View::Guide => None,
        }
    }
}

/// What the UI needs to render. Borrowed, never owned — the library is the
/// application's, not the UI's.
pub struct Context<'a> {
    pub store: &'a Store,
    /// `None` in tests, which have no audio device.
    pub player: Option<&'a PlayerHandle>,
    /// `None` in tests, and when the socket could not be bound.
    pub remote: Option<&'a crate::remote::Server>,
}

/// which-key's own default is 200ms; this is the value the config that
/// prompted the setting uses, and a prefix held on purpose outlasts it easily.
const DEFAULT_WHICH_KEY_DELAY_MS: u64 = 300;

/// Whether a prefix has been held long enough for its popup to be drawn.
///
/// A pure function of the three things that decide it, so the rule is testable
/// without a clock — the same separation `record_play_if_earned` makes, and for
/// the same reason.
///
/// A zero delay is due immediately, so turning the wait off restores exactly
/// the behaviour this replaced rather than approximating it.
fn which_key_due(since: Option<Instant>, now: Instant, delay: Duration) -> bool {
    match since {
        Some(started) => now.saturating_duration_since(started) >= delay,
        None => false,
    }
}

/// Run the event loop until the user quits.
pub fn run<B>(terminal: &mut Terminal<B>, ui: &mut Ui, ctx: &Context<'_>) -> io::Result<()>
where
    B: ratatui::backend::Backend<Error = io::Error>,
{
    while !ui.quit {
        terminal.draw(|frame| draw(frame, ui, ctx))?;
        // Poll rather than block: the position display has to tick even when
        // nobody is pressing anything.
        //
        // While a prefix is waiting to be revealed, poll only for what is left
        // of its delay. Otherwise the popup appears whenever the 200ms tick
        // next comes round, which is a different moment depending on where in
        // the cycle the key landed — a 300ms delay showing at anywhere up to
        // 500ms, and visibly inconsistent between presses.
        if event::poll(ui.tick(Instant::now()))?
            && let Event::Key(key) = event::read()?
            && key.is_press()
        {
            handle_key(key, ui, ctx);
        }
        drain_player_events(ui, ctx);
        serve_remote(ui, ctx);
        note_play_progress(ui, ctx);
        audit_underruns(ui, ctx);
        ui.expire_status();
    }
    Ok(())
}

/// Apply what the socket received, and publish what it should answer with.
///
/// Both halves live on this thread on purpose. Commands are applied through
/// [`execute`] — the same door the `:` line and every key binding use, so a
/// remote `pause` is not a second implementation of pausing. And the status is
/// *published* rather than served: the listener thread reads the snapshot and
/// replies on its own, so a status bar polling once a second never waits for a
/// render and can never hold one up.
fn serve_remote(ui: &mut Ui, ctx: &Context<'_>) {
    let Some(server) = ctx.remote else { return };

    // `try_iter` and not `for`: the channel stays open for the program's life,
    // so iterating it would block here until the player shut down.
    let pending: Vec<_> = server.commands.try_iter().collect();
    for cmd in pending {
        ui.audit.line("remote", &format!("{cmd:?}"));
        execute(ui, ctx, cmd);
    }

    if let Some(player) = ctx.player {
        let track = ui
            .playback
            .current
            .and_then(|id| ctx.store.get(id))
            .map(|t| ui.now_playing.render(t));
        let next = crate::remote::Status {
            state: Some(player.state()),
            track,
            position: player.position(),
        };
        // A poisoned lock is not worth losing the status over: the only writer
        // is this line and the only reader formats a string.
        match server.status.lock() {
            Ok(mut slot) => *slot = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }
}

/// Count a play once enough of the track has actually been heard.
///
/// Polled from the render loop rather than triggered by an event, because the
/// interesting moment — half the track — is not an event the player has. The
/// same reason the underrun count is polled.
fn note_play_progress(ui: &mut Ui, ctx: &Context<'_>) {
    let Some(player) = ctx.player else {
        return;
    };
    record_play_if_earned(ui, ctx, player.position());
}

/// The decision, separated from where the position comes from — otherwise the
/// once-only guard is only reachable with a real audio device, which is to say
/// untestable. An earlier version had exactly that hole.
fn record_play_if_earned(ui: &mut Ui, ctx: &Context<'_>, position: Duration) {
    if ui.counted_current || ui.events_path.is_none() {
        return;
    }
    let Some(id) = ui.playback.current else {
        return;
    };
    let Some(track) = ctx.store.get(id) else {
        return;
    };
    if !crate::history::counts_as_played(position, track.duration) {
        return;
    }
    ui.counted_current = true;
    let now = crate::history::now();
    ui.history.note_played(&track.path, now);
    let plays = ui.history.get(&track.path).map(|e| e.plays).unwrap_or(0);
    ui.audit.line(
        "played",
        &format!("{} (play {plays})", track.path.display()),
    );
    record(ui, crate::events::Kind::Played(track.path.clone()));
    // A rule mentioning `play_count` or `last_played` now answers differently.
    refresh_smart(ui, ctx);
}

/// Rate the track being heard, or — with nothing playing — the one under the
/// cursor. Rating is something you do to a song you are listening to, and
/// falling back to the selection is what makes it work while browsing too.
fn rate(ui: &mut Ui, ctx: &Context<'_>, spec: crate::cmd::RateSpec) {
    use crate::cmd::RateSpec;

    let Some(id) = ui.playback.current.or_else(|| ui.selected_track(ctx)) else {
        ui.notify("nothing to rate");
        return;
    };
    let Some(track) = ctx.store.get(id) else {
        return;
    };
    let path = track.path.clone();
    let title = format!("{} — {}", track.file_under(), track.display_title());

    if spec == RateSpec::Report {
        let current = match ui.history.get(&path).and_then(|e| e.rating) {
            Some(n) => format!("{n}/5"),
            None => "unrated".into(),
        };
        ui.notify(format!("{title}: {current}"));
        return;
    }

    if ui.events_path.is_none() {
        ui.notify("ratings need somewhere to record them");
        return;
    }
    // `note_rating` takes zero as "clear", which is the storage-level spelling
    // of `RateSpec::Clear`; the command language does not have a zero.
    let stars = match spec {
        RateSpec::Set(n) => n,
        _ => 0,
    };
    ui.history.note_rating(&path, stars, crate::history::now());
    record(
        ui,
        crate::events::Kind::Rated(path.clone(), (stars > 0).then_some(stars)),
    );

    let said = match spec {
        RateSpec::Set(n) => format!("{n}/5"),
        _ => "unrated".into(),
    };
    ui.audit
        .line("rated", &format!("{said} {}", path.display()));
    ui.notify(format!("{title}: {said}"));
    // A rule mentioning `rating` now answers differently.
    refresh_smart(ui, ctx);
}

/// Write the history if anything changed. Failures are logged, never shown:
/// the atomic write means the previous file survives, and a full disk should
/// not interrupt listening.
/// Append one event, and treat the in-memory state as saved once it lands.
///
/// Written immediately rather than at exit: a rating is worth more than a play
/// count, and losing one to a crash would be worse than the cost of an open.
pub fn record(ui: &mut Ui, kind: crate::events::Kind) {
    let Some(path) = ui.events_path.clone() else {
        return;
    };
    let event = crate::events::Event {
        at: crate::history::now(),
        kind,
    };
    match crate::events::append(&path, &event) {
        Ok(()) => ui.history.mark_saved(),
        Err(e) => ui.audit.line("history", &format!("could not record: {e}")),
    }
}

/// Record any underruns since the last tick.
///
/// Polled from the UI loop rather than pushed from the callback, which may not
/// allocate or write. A 200ms tick is plenty: what matters is which track and
/// roughly when, not the exact millisecond.
fn audit_underruns(ui: &mut Ui, ctx: &Context<'_>) {
    if !ui.audit.is_enabled() {
        return;
    }
    let Some(player) = ctx.player else { return };
    let total = player.underruns();
    if total <= ui.seen_underruns {
        return;
    }
    let new = total - ui.seen_underruns;
    ui.seen_underruns = total;

    let track = ui
        .playback
        .current
        .and_then(|id| ctx.store.get(id))
        .map(|t| format!("{} — {}", t.file_under(), t.display_title()))
        .unwrap_or_else(|| "nothing playing".into());

    ui.audit.line(
        "underrun",
        &format!(
            "+{new} (total {total}) at {:.3}s, {} frames short, in {track}",
            player.position().as_secs_f64(),
            player.last_short_frames(),
        ),
    );
}

/// Turn player events into UI state, and advance when a track ends.
fn drain_player_events(ui: &mut Ui, ctx: &Context<'_>) {
    let Some(player) = ctx.player else { return };
    let mut finished = false;
    while let Ok(event) = player.events().try_recv() {
        match event {
            // Answered the moment it is asked, so the resolution reflects the
            // pool as it stands right now. Nothing is announced ahead of time
            // and nothing can go stale between announcing and playing.
            crate::audio::PlayerEvent::NeedNext => {
                let next = announce_next(ui, ctx);
                ui.audit.line(
                    "next",
                    &match next.as_ref() {
                        Some(s) => format!("splicing {}", s.path.display()),
                        None => "end of pool, letting playback stop".into(),
                    },
                );
                player.set_next(next);
            }
            crate::audio::PlayerEvent::Finished => {
                ui.audit.line("finished", "track played to its end");
                // A track announced but never played means the player could
                // not open it. Adopt it so `advance` resolves past it rather
                // than handing back the same broken file.
                if let Some(id) = ui.pending_next.take() {
                    ui.playback.current = Some(id);
                }
                finished = true;
            }
            crate::audio::PlayerEvent::Error(e) => {
                ui.audit.line("error", &e);
                ui.notify(format!("error: {e}"));
            }
            crate::audio::PlayerEvent::DeviceLost(e) => {
                ui.audit.line("device", &format!("lost: {e}"));
                ui.notify(format!("device lost: {e}"));
            }
            crate::audio::PlayerEvent::DeviceChanged => {
                ui.audit.line("device", "output rerouted by the OS");
                ui.notify("audio output changed");
            }
            crate::audio::PlayerEvent::Loaded {
                path,
                duration,
                source_rate,
                channels,
                resampling,
                gain,
            } => {
                ui.audit.line(
                    "loaded",
                    &format!(
                        "{} — {source_rate} Hz, {channels} ch, {}{}, gain {}",
                        path.display(),
                        duration
                            .map(|d| format!("{:.1}s", d.as_secs_f64()))
                            .unwrap_or_else(|| "unknown length".into()),
                        if resampling { ", resampling" } else { "" },
                        db_label(gain),
                    ),
                );
                // A `Loaded` arrives when a track becomes audible, so this is
                // the moment a gapless splice actually takes effect.
                if let Some(id) = ui.pending_next.take() {
                    ui.playback.current = Some(id);
                    ui.status.clear();
                }
                ui.counted_current = false;
                // A track boundary is the cheapest honest save point: the
                // queue and current track have just changed, and writing on
                // quit alone would lose the whole session to a crash — which
                // is exactly the session worth having.
                //
                // The first `Loaded` after a restore is the exception: it is
                // this program's own doing and arrives before the seek has
                // landed. Skip it once, then resume saving normally.
                if ui.resuming {
                    ui.resuming = false;
                } else {
                    save_session(ui, ctx);
                }
            }
        }
    }
    if finished {
        advance(ui, ctx, true);
    }
}

/// Resolve and start the next (or previous) track.
///
/// The queue pop lives here rather than in `next()`, which stays pure — see
/// DESIGN §5 and the note on [`Source::Queue`].
fn advance(ui: &mut Ui, ctx: &Context<'_>, forward: bool) {
    let decision = if forward {
        next::next_with_source(next_input(ui))
    } else {
        next::prev(next_input(ui)).map(|id| (id, Source::Pool))
    };

    let Some((id, source)) = decision else {
        ui.notify("end of pool");
        ui.playback.current = None;
        return;
    };

    match source {
        Source::Queue => {
            ui.queue.pop_front();
        }
        // A shuffled lap that wrapped would otherwise replay in the same
        // order; reseed so the next pass differs.
        Source::Wrapped if ui.playback.shuffle => ui.playback.reshuffle(),
        _ => {}
    }
    play(ui, ctx, id);
}

/// Resolve what follows the current track and hand back its path.
///
/// This is [`advance`] minus the load: the pool side effects happen here, so
/// the queue is popped and a wrapped shuffle reseeded exactly once, at the
/// point the decision is made. `playback.current` deliberately stays put —
/// the track it names is still the one being heard.
fn announce_next(ui: &mut Ui, ctx: &Context<'_>) -> Option<crate::audio::TrackSource> {
    let (id, source) = next::next_with_source(next_input(ui))?;
    match source {
        Source::Queue => {
            ui.queue.pop_front();
        }
        Source::Wrapped if ui.playback.shuffle => ui.playback.reshuffle(),
        _ => {}
    }
    let source = source_for(ui, ctx.store.get(id)?);
    ui.pending_next = Some(id);
    Some(source)
}

/// Write the session, if one is being kept. Failures are logged, never shown:
/// a full disk should not interrupt playback, and the atomic write means the
/// previous session survives anyway.
pub fn save_session(ui: &mut Ui, ctx: &Context<'_>) {
    if ui.resuming {
        return;
    }
    let Some(path) = ui.session_path.clone() else {
        return;
    };
    let snapshot = capture_session(ui, ctx);
    if let Err(e) = crate::session::save(&path, &snapshot) {
        ui.audit.line("session", &format!("could not save: {e}"));
    }
}

/// Capture where we are, for [`crate::session`].
///
/// Paths rather than `TrackId`s — see the note on `Session::queue`.
pub fn capture_session(ui: &Ui, ctx: &Context<'_>) -> crate::session::Session {
    let path_of = |id: TrackId| ctx.store.get(id).map(|t| t.path.clone());
    crate::session::Session {
        version: 0, // stamped by `save`
        queue: ui.queue.entries().filter_map(|id| path_of(*id)).collect(),
        open_playlist: ui.open.as_ref().map(|o| o.name().to_string()),
        view: Some(ui.view as u8),
        current: ui.playback.current.and_then(path_of),
        position_secs: ctx
            .player
            .map(|p| p.position().as_secs_f64())
            .unwrap_or(0.0),
        modes: crate::session::Modes {
            shuffle: ui.playback.shuffle,
            repeat: ui.playback.repeat,
            repeat_current: ui.playback.repeat_current,
            continue_playing: ui.continue_playing,
            aaa_mode: ui.playback.aaa_mode.label().into(),
            replaygain: ui.replaygain.label().into(),
            active: match ui.playback.active {
                next::ActiveSource::Playlist => "playlist".into(),
                next::ActiveSource::Library => "library".into(),
            },
            rng_state: ui.playback.rng_state,
        },
    }
}

/// Restore a saved session, leaving playback **paused**.
///
/// `fixed` names the settings the config spoke about; those are left alone.
/// The rule is "if you wrote it in `config.toml` you meant it, and anything it
/// does not mention remembers what you last did" — so editing the config takes
/// effect immediately rather than being shadowed by a stale session file.
///
/// Nothing here is an error worth interrupting a launch for: a track that has
/// moved or gone is skipped, and an unparseable mode falls back to the default.
pub fn restore_session(
    ui: &mut Ui,
    ctx: &Context<'_>,
    saved: &crate::session::Session,
    fixed: &std::collections::HashSet<SettingKey>,
) {
    use crate::cmd::ReplayGainMode;
    use next::{AaaMode, ActiveSource};

    let m = &saved.modes;
    if !fixed.contains(&SettingKey::Shuffle) {
        ui.playback.shuffle = m.shuffle;
    }
    if !fixed.contains(&SettingKey::Repeat) {
        ui.playback.repeat = m.repeat;
    }
    if !fixed.contains(&SettingKey::RepeatCurrent) {
        ui.playback.repeat_current = m.repeat_current;
    }
    if !fixed.contains(&SettingKey::Continue) {
        ui.continue_playing = m.continue_playing;
    }
    if !fixed.contains(&SettingKey::AaaMode)
        && let Some(mode) = AaaMode::parse(&m.aaa_mode)
    {
        ui.playback.aaa_mode = mode;
    }
    if !fixed.contains(&SettingKey::ReplayGain)
        && let Some(mode) = ReplayGainMode::parse(&m.replaygain)
    {
        ui.replaygain = mode;
    }
    ui.playback.active = match m.active.as_str() {
        "playlist" => ActiveSource::Playlist,
        _ => ActiveSource::Library,
    };
    // Restored *after* shuffle, because setting shuffle reseeds.
    ui.playback.rng_state = m.rng_state;

    ui.queue.clear();
    let mut missing = 0;
    for path in &saved.queue {
        match ctx.store.id_for_path(path) {
            Some(id) => ui.queue.push_back(id),
            None => missing += 1,
        }
    }

    // The playlist comes back by *name*: its contents live in a file, and a
    // copy in the session could only be a stale one.
    refresh_playlist_rows(ui);
    if let Some(name) = &saved.open_playlist {
        open_playlist(ui, ctx, &name.clone());
    }

    // **After** the playlist, because opening one switches to view 3 — which
    // is exactly why every launch used to land there.
    if let Some(view) = saved
        .view
        .and_then(crate::cmd::ViewId::from_number)
        .map(View::from_id)
    {
        switch_view(ui, view);
        // Landing somewhere with no history of having been anywhere else.
        ui.previous_view = view;
    }

    if let Some(path) = saved.current.as_ref()
        && let Some(id) = ctx.store.id_for_path(path)
        && let Some(track) = ctx.store.get(id)
    {
        ui.playback.current = Some(id);
        // Held until the resumed track's `Loaded` has been seen, so that the
        // save it triggers does not record a position of 0.
        ui.resuming = true;
        if let Some(player) = ctx.player {
            // Load, seek, then pause: resuming playback unbidden on launch
            // would be obnoxious. The commands queue in order, so the decoder
            // opens the file, moves to where you were, and stops there.
            player.load(source_for(ui, track));
            if saved.position_secs > 0.0 {
                player.seek(crate::audio::SeekTo::Absolute(
                    std::time::Duration::from_secs_f64(saved.position_secs),
                ));
            }
            player.pause();
        }
    } else if saved.current.is_some() {
        missing += 1;
    }

    if missing > 0 {
        ui.notify(format!("{missing} remembered track(s) are gone"));
    }
}

/// A gain as decibels, for the log. Silence has no decibel value.
fn db_label(gain: f32) -> String {
    if gain <= 0.0 {
        return "silent".into();
    }
    format!("{:+.1} dB", 20.0 * gain.log10())
}

/// What to play, and at what gain — the ReplayGain tags turned into a number.
///
/// Untagged files come back at unity, so this is inert until something has
/// been tagged. Analysis is deliberately somebody else's job: §1 puts tag
/// *editing* out of scope, and `rsgain` already does it well.
fn source_for(ui: &Ui, track: &crate::store::Track) -> crate::audio::TrackSource {
    crate::audio::TrackSource {
        path: track.path.clone(),
        gain: replay_gain(ui, track),
    }
}

fn replay_gain(ui: &Ui, track: &crate::store::Track) -> f32 {
    use crate::cmd::ReplayGainMode as Mode;

    let Some(rg) = track.replaygain.as_ref() else {
        return 1.0;
    };
    let mode = match ui.replaygain {
        Mode::Off => return 1.0,
        // Shuffle jumps between masters, so each track's own level is what
        // matters. Playing in order, the album's internal balance is the
        // thing worth preserving — turning it into a per-track correction
        // would flatten exactly what the mastering engineer intended.
        Mode::Auto if ui.playback.shuffle => Mode::Track,
        Mode::Auto => Mode::Album,
        m => m,
    };
    let (db, peak) = match mode {
        Mode::Album => (
            rg.album_gain.or(rg.track_gain),
            rg.album_peak.or(rg.track_peak),
        ),
        _ => (
            rg.track_gain.or(rg.album_gain),
            rg.track_peak.or(rg.album_peak),
        ),
    };
    let Some(db) = db else { return 1.0 };
    let gain = 10f32.powf(db / 20.0);

    // Never push a known peak past full scale. A positive gain on a quiet
    // master is exactly where this bites, and clipping is worse than being a
    // decibel under the target.
    match peak {
        Some(p) if p > 0.0 => gain.min(1.0 / p),
        _ => gain,
    }
}

fn next_input(ui: &Ui) -> next::NextInput<'_> {
    next::NextInput {
        queue: &ui.queue,
        active: ui.playback.active,
        library: &ui.library,
        playlist: &ui.playlist,
        current: ui.playback.current,
        aaa_mode: ui.playback.aaa_mode,
        shuffle: ui.playback.shuffle,
        repeat: ui.playback.repeat,
        repeat_current: ui.playback.repeat_current,
        rng_state: ui.playback.rng_state,
    }
}

fn play(ui: &mut Ui, ctx: &Context<'_>, id: TrackId) {
    let Some(track) = ctx.store.get(id) else {
        return;
    };
    ui.playback.current = Some(id);
    ui.status.clear();
    // An explicit play discards whatever was queued up to be spliced on; the
    // player drops it too, on `Load`.
    ui.pending_next = None;
    if let Some(player) = ctx.player {
        player.load(source_for(ui, track));
    }
}

/// Whether a popup is up **and has something hidden to scroll to**.
///
/// The narrower question on purpose: it makes the key exception exactly as
/// wide as the affordance. A popup that fits shows no `^d ^u scroll` footer
/// and does not take the key either, so `^d` still means half a page there —
/// and there is never a press that looks like it should scroll and does not.
///
/// Not the completion menu: that has its own keys and its own scrolling
/// question (#43), where `^d` would have to argue with the `:` line.
fn popup_is_scrollable(ui: &Ui) -> bool {
    let (visible, total) = ui.help_view.get();
    ui.prompt.is_none() && total > visible
}

/// `^d`/`^u` inside a popup — half of what it is showing, clamped at both
/// ends, exactly as which-key's own scroll does (`win.lua:104`).
///
/// Returns whether the key was one of them, so the caller knows to stop.
fn scroll_popup(ui: &mut Ui, key: KeyEvent) -> bool {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    let (visible, total) = ui.help_view.get();
    let half = (visible / 2).max(1);
    let furthest = total.saturating_sub(visible);
    match key.code {
        KeyCode::Char('d') => ui.help_scroll = (ui.help_scroll + half).min(furthest),
        KeyCode::Char('u') => ui.help_scroll = ui.help_scroll.saturating_sub(half),
        _ => return false,
    }
    true
}

/// Route a key.
///
/// A prompt swallows everything while it is open. Otherwise the key is
/// normalised, looked up in the bindings (context first, then `Common`), and
/// the resulting [`Command`] is handed to [`execute`] — which is the only
/// place UI state is mutated, per §6.
fn handle_key(key: KeyEvent, ui: &mut Ui, ctx: &Context<'_>) {
    // `^d`/`^u` belong to the popup while one is up, and act *on* it rather
    // than through it — which is why they are the exception to the
    // any-key-dismisses rule below rather than a hole in it. which-key does
    // the same, and gets away without the exception only because its popup is
    // not sharing a keymap with anything.
    //
    // Nothing is shadowed by this: a chord cannot continue with `^d` (no
    // binding starts with a prefix and then a control key), so the alternative
    // was not "half a page" but "the chord is abandoned".
    if popup_is_scrollable(ui) && scroll_popup(ui, key) {
        return;
    }

    // Any key closes the help, and is swallowed doing it. Acting on the same
    // press would mean reading the popup and pressing something is different
    // from reading it and pressing nothing.
    if ui.help.is_some() {
        ui.help = None;
        ui.help_scroll = 0;
        return;
    }
    if ui.prompt.is_some() {
        handle_prompt_key(key, ui, ctx);
        return;
    }

    // Opening a prompt is a UI mode, not a command: there is nothing for it to
    // do until a line has been typed.
    //
    // **Unless a chord is part-typed.** `<leader> ?` cannot work if `?` always
    // means reverse search, and the whole point of a leader is that the second
    // key keeps its own meaning when pressed alone.
    // `/` and `?` are navigation, so they belong to normal mode — in transport
    // mode there is no list you are looking through, and `?` is the mode's own
    // "what works in here". `:` stays available from both: it is the way to
    // everything, and a mode should not be able to trap you away from it.
    let opens_prompt = ui.pending.is_empty()
        && (ui.mode == crate::cmd::Mode::Normal || key.code == KeyCode::Char(':'));
    match if opens_prompt {
        key.code
    } else {
        KeyCode::Null
    } {
        KeyCode::Char(':') => {
            ui.prompt = Some(Prompt {
                kind: PromptKind::Command,
                buffer: String::new(),
                menu: None,
                selected: 0,
            });
            return;
        }
        KeyCode::Char('/') => {
            ui.search_origin = Some(current_cursor(ui));
            ui.prompt = Some(Prompt {
                kind: PromptKind::Search(SearchDir::Forward),
                buffer: String::new(),
                menu: None,
                selected: 0,
            });
            return;
        }
        KeyCode::Char('?') => {
            ui.search_origin = Some(current_cursor(ui));
            ui.prompt = Some(Prompt {
                kind: PromptKind::Search(SearchDir::Backward),
                buffer: String::new(),
                menu: None,
                selected: 0,
            });
            return;
        }
        _ => {}
    }

    let key = Key::from_event(key);

    // A digit starts or extends a count, but only while no chord is part-typed
    // and only from 1-9 — `0` continues a count and is a motion of its own in
    // vim, so it may never start one.
    if ui.pending.is_empty()
        && let KeyCode::Char(c) = key.code
        && !key.ctrl
        && !key.alt
        && c.is_ascii_digit()
        && (c != '0' || ui.count.is_some())
    {
        let digit = c.to_digit(10).unwrap() as usize;
        ui.count = Some(
            ui.count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(digit),
        );
        return;
    }

    // Set when a chord *begins*, and deliberately never cleared: it is only
    // ever read while `pending` is non-empty, and every chord that starts
    // overwrites it here. Clearing it at each of the two sites that empty
    // `pending` would be a second copy of the same truth to keep in step.
    if ui.pending.is_empty() {
        ui.pending_since = Some(Instant::now());
    }
    // Going deeper shows a different list, so it opens at its top. The delay
    // above deliberately does *not* restart; only the content did.
    ui.help_scroll = 0;
    ui.pending.push(key);
    match bind::resolve(&ui.bindings, ui.context(), &ui.pending) {
        bind::Resolved::Exact(cmd) => {
            let cmd = cmd.clone();
            let count = ui.count.take();
            ui.pending.clear();
            execute_counted(ui, ctx, cmd, count);
        }
        // Wait for the rest of the chord. The status line renders the pending
        // keys straight from this state — see `Ui::pending_label` — rather than
        // being written into, so there is nothing left to clear afterwards.
        bind::Resolved::Prefix => {}
        bind::Resolved::None => {
            ui.pending.clear();
            ui.count = None;
        }
    }
}

/// Recompute an open menu from the line.
///
/// The candidate list is a **view of the buffer**, so it is derived afresh on
/// every keystroke rather than whittled down. That is what makes Backspace
/// restore candidates for nothing: a shorter buffer simply matches more, and
/// there is no undo to implement. Filtering a stored list instead would need
/// both the original set and the prefix the menu opened on.
///
/// Deliberately does **not** extend the line. `Tab` is the only key that edits
/// what you typed.
fn refilter(ui: &mut Ui) {
    if ui.prompt.as_ref().is_none_or(|p| p.menu.is_none()) {
        return;
    }
    let buffer = ui.prompt.as_ref().expect("checked above").buffer.clone();
    let world = ui.completion_world();
    let matches = crate::cmd::complete::matches(&buffer, &world);
    if let Some(prompt) = ui.prompt.as_mut() {
        // The candidate under the cursor has usually gone; starting again at
        // the top is less surprising than landing somewhere arbitrary.
        prompt.selected = 0;
        prompt.menu = Some(matches);
    }
}

fn handle_prompt_key(key: KeyEvent, ui: &mut Ui, ctx: &Context<'_>) {
    let Some(prompt) = ui.prompt.as_mut() else {
        return;
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let menu_open = prompt.menu.is_some();

    // The menu keys come first: while it is open these belong to it, and must
    // not fall through to being typed into the line.
    //
    // The arrows are synonyms for C-n/C-p rather than the primary binding.
    // They are not what this UI is for, but a menu that ignores them feels
    // broken — and because candidates run *down* the columns, Down really is
    // the next candidate rather than an approximation of it.
    if menu_open {
        let handled = match key.code {
            KeyCode::Char('n') if ctrl => {
                prompt.step(true);
                true
            }
            KeyCode::Char('p') if ctrl => {
                prompt.step(false);
                true
            }
            KeyCode::Down => {
                prompt.step(true);
                true
            }
            KeyCode::Up => {
                prompt.step(false);
                true
            }
            KeyCode::Char('y') if ctrl => {
                if let Some(c) = prompt.candidates().get(prompt.selected).cloned() {
                    prompt.accept(&c);
                }
                true
            }
            _ => false,
        };
        if handled {
            return;
        }
    }

    match key.code {
        // Esc closes the menu first and the line second, so dismissing a menu
        // does not throw away what has been typed.
        KeyCode::Esc => {
            if menu_open {
                prompt.dismiss();
            } else {
                ui.prompt = None;
                abandon_search(ui);
            }
        }
        KeyCode::Backspace => {
            if prompt.buffer.pop().is_none() {
                // Backspacing past the sigil closes the line, as in vi.
                ui.prompt = None;
                abandon_search(ui);
            } else {
                // Widens the menu again, rather than dismissing it.
                refilter(ui);
                incsearch(ui, ctx);
            }
        }
        // Guarded on ctrl, or every chord types its letter: `C-n` on a bare
        // line put an "n" in the buffer, because a `Char` arm that ignores
        // modifiers cannot tell a keystroke from a command.
        KeyCode::Char(c) if !ctrl => {
            prompt.buffer.push(c);
            // Narrows the menu rather than closing it: you type to home in on
            // something, and throwing the list away is the opposite of that.
            refilter(ui);
            incsearch(ui, ctx);
        }
        // With a menu already open Tab cycles it, as vim's wildmenu does.
        // Re-completing would be a no-op — the candidate set has not changed —
        // so cycling is strictly the more useful reading of a second Tab.
        KeyCode::Tab if menu_open => prompt.step(true),
        KeyCode::BackTab if menu_open => prompt.step(false),
        // Tab completes. Search has no closed vocabulary to complete against,
        // so it is left alone.
        KeyCode::Tab if prompt.kind == PromptKind::Command => {
            let buffer = prompt.buffer.clone();
            let world = ui.completion_world();
            // `set key=value` is one word, so its value half needs separate
            // handling — the key/value split is inside the word, not at a
            // space.
            let last = buffer.rsplit(char::is_whitespace).next().unwrap_or("");
            let done = if last.contains('=') {
                crate::cmd::complete::complete_setting_value(last, &ui.completion_world()).map(
                    |c| {
                        let head = &buffer[..buffer.len() - last.len()];
                        (format!("{head}{}", c.line), c.options)
                    },
                )
            } else {
                crate::cmd::complete::complete(&buffer, &world).map(|c| (c.line, c.options))
            };
            match done {
                Some((line, options)) => {
                    if let Some(prompt) = ui.prompt.as_mut() {
                        prompt.buffer = line;
                        // One candidate is already applied, so a menu of one
                        // would be noise.
                        // Opens on the first candidate: `C-y` acts on a
                        // selection whether or not it has been moved, so the
                        // menu must show which one that is.
                        //
                        // A single match is already applied, so a menu of one
                        // would be noise — that stays closed rather than open
                        // and empty, which means something else now.
                        prompt.menu = (options.len() > 1).then_some(options);
                        prompt.selected = 0;
                    }
                }
                None => ui.notify("no completion"),
            }
        }
        KeyCode::Enter => {
            let prompt = ui.prompt.take().expect("checked above");
            match prompt.kind {
                PromptKind::Command => match parse::line(&prompt.buffer) {
                    Ok(Some(cmd)) => execute(ui, ctx, cmd),
                    Ok(None) => {}
                    Err(e) => ui.notify(e.to_string()),
                },
                PromptKind::Search(dir) => {
                    // The preview is over; where it landed is where we stay.
                    ui.search_origin = None;
                    execute(
                        ui,
                        ctx,
                        Command::Search {
                            pattern: prompt.buffer,
                            dir,
                        },
                    );
                }
            }
        }
        _ => {}
    }
}

/// The one place all state mutation happens (DESIGN §6).
pub fn execute(ui: &mut Ui, ctx: &Context<'_>, cmd: Command) {
    execute_counted(ui, ctx, cmd, None);
}

/// As [`execute`], with a vim-style count prefix.
///
/// `None` and `Some(1)` differ: `gt` is "next view" while `1gt` is "view 1",
/// exactly as in vim. A command that ignores counts treats both the same.
pub fn execute_counted(ui: &mut Ui, ctx: &Context<'_>, cmd: Command, count: Option<usize>) {
    let len = current_len(ui, ctx);
    let cursor = current_cursor(ui);
    let n = count.unwrap_or(1).max(1);

    // One place, so a new editing command cannot forget to be undoable.
    remember(ui, &cmd);

    match cmd {
        // ---- navigation
        Command::Down => set_cursor(ui, (cursor + n).min(len.saturating_sub(1))),
        Command::Up => set_cursor(ui, cursor.saturating_sub(n)),
        Command::Top => set_cursor(ui, 0),
        // `G` goes to the end; `<count>G` goes to that row, 1-based, as in vim.
        Command::Bottom => match count {
            Some(row) => set_cursor(ui, row.saturating_sub(1).min(len.saturating_sub(1))),
            None => set_cursor(ui, len.saturating_sub(1)),
        },
        Command::PageDown => {
            let page = page_height(ui);
            set_cursor(ui, (cursor + page * n).min(len.saturating_sub(1)));
        }
        Command::PageUp => {
            let page = page_height(ui);
            set_cursor(ui, cursor.saturating_sub(page * n));
        }
        // `^d`/`^u`, then `zz`. The recentring is what keeps the cursor pinned
        // mid-screen so the text slides past it, instead of the cursor walking
        // to an edge and staying there — which makes each press show a
        // different amount of new context than the one before.
        //
        // In the guide body there is nothing to recentre: the "cursor" *is*
        // the scroll offset there, so moving it half a screen already is the
        // scroll. `focused_scroll` returns `None` for that pane and the
        // recentre is skipped, which is right rather than a gap.
        Command::HalfPageDown => {
            let half = half_page_height(ui);
            set_cursor(ui, (cursor + half * n).min(len.saturating_sub(1)));
            recentre(ui);
        }
        Command::HalfPageUp => {
            let half = half_page_height(ui);
            set_cursor(ui, cursor.saturating_sub(half * n));
            recentre(ui);
        }
        Command::PaneNext => {
            if ui.view == View::Guide {
                ui.guide_pane = match ui.guide_pane {
                    Pane::Tree => Pane::Tracks,
                    Pane::Tracks => Pane::Tree,
                };
            }
            if ui.view == View::Playlist {
                ui.playlist_pane = match ui.playlist_pane {
                    Pane::Tree => Pane::Tracks,
                    Pane::Tracks => Pane::Tree,
                };
            }
        }
        // vim's `z` family. Only view 1 has folds; elsewhere `fold toggle`
        // is what `o` means on a checkbox, which is the one form that has a
        // sensible reading outside a tree.
        Command::Fold(what) => {
            if ui.view != View::Tree {
                if what == Fold::Toggle {
                    // One catalog on or off. The union of what is on is the
                    // library, so this reads as "include this too".
                    if ui.view == View::Filters {
                        toggle_catalog_under_cursor(ui, ctx);
                    }
                }
                return;
            }
            // Hold the cursor still **on screen** across a level change.
            //
            // `keeping_cursor` keeps the cursor on the right *node*, which is
            // the correct thing logically and the wrong thing to watch: a
            // level change inserts or removes rows above the cursor, so the
            // node keeps its identity and jumps up or down the screen. What
            // the eye is tracking is the row it is looking at, and the tree
            // should open and close behind it.
            let screen_row = ui.tree.selected.saturating_sub(ui.tree_scroll.top());

            let lib = &ui.library;
            match what {
                Fold::Open => ui.tree.open(lib, ctx.store),
                Fold::Close => {
                    ui.tree.close(lib, ctx.store);
                }
                Fold::Toggle => ui.tree.toggle(lib, ctx.store),
                Fold::OpenDeep => ui.tree.open_deep(lib, ctx.store),
                Fold::CloseDeep => ui.tree.close_deep(lib, ctx.store),
                Fold::ToggleDeep => ui.tree.toggle_deep(lib, ctx.store),
                Fold::All => ui.tree.open_everything(lib, ctx.store),
                Fold::None => ui.tree.close_everything(lib, ctx.store),
                Fold::More => ui.tree.more(lib, ctx.store),
                Fold::Less => ui.tree.less(lib, ctx.store),
                Fold::Only => ui.tree.only(lib, ctx.store),
            }

            // Only the level forms: `zo` and friends act on the node under the
            // cursor, so everything they change is *below* it and the row does
            // not move on its own.
            // `zx` holds the row too: it closes everything around the cursor,
            // so the one thing that must not move is the cursor.
            if what.levels() || what.reveals() {
                ui.tree_scroll
                    .scroll_to(ui.tree.selected.saturating_sub(screen_row));
            }
        }
        // vim's `zt`, `zz`, `zb`. Measured from the last frame, so a pane
        // that has never been drawn does nothing rather than guessing.
        Command::Place(place) => {
            if let Some(scroll) = focused_scroll(ui) {
                scroll.place(place);
            }
        }
        // Structural motions — #31. In view 1 they read the tree's depths; in
        // a flat list the album and artist boundaries are the structure, which
        // is the same idea seen edge-on.
        Command::Go(motion) => {
            if ui.view == View::Tree {
                ui.tree.go(&ui.library, ctx.store, motion, n);
            } else if let Some(at) = flat_step(ui, ctx, motion, n) {
                set_cursor(ui, at);
            }
        }
        Command::Playing => go_to_playing(ui, ctx),
        Command::PaneLeft => match ui.view {
            View::Playlist => ui.playlist_pane = Pane::Tree,
            View::Guide => ui.guide_pane = Pane::Tree,
            _ => {}
        },
        Command::PaneRight => match ui.view {
            View::Playlist => ui.playlist_pane = Pane::Tracks,
            View::Guide => ui.guide_pane = Pane::Tracks,
            _ => {}
        },
        // Run a command as though the named pane had focus, then put focus
        // back. Reusing the ordinary command rather than duplicating every
        // motion per pane is the whole point of the wrapper.
        Command::In { pane, cmd } => {
            let want = match pane {
                crate::cmd::PaneTarget::Tree => Pane::Tree,
                crate::cmd::PaneTarget::Tracks => Pane::Tracks,
            };
            // View 3 is the only two-pane view now that the library tree
            // holds all three levels in one.
            if ui.view != View::Playlist {
                return;
            }
            let focused = std::mem::replace(&mut ui.playlist_pane, want);
            execute_counted(ui, ctx, *cmd, count);
            ui.playlist_pane = focused;
        }
        Command::Activate => match ui.view {
            View::Browser => ui.browser.enter(),
            // The list picks; Enter moves into what you picked, which is the
            // same thing it does everywhere else.
            View::Guide => ui.guide_pane = Pane::Tracks,
            // The left pane picks, so Enter there opens rather than plays —
            // and moves focus to the contents, which is what you wanted next.
            View::Playlist if ui.playlist_pane == Pane::Tree => {
                let Some(row) = ui.playlist_rows.get(ui.playlists_sel).cloned() else {
                    return;
                };
                open_playlist(ui, ctx, &row.name);
                ui.playlist_pane = Pane::Tracks;
            }
            // Enter makes this the only catalog — the common case, and one
            // keypress rather than clearing the others by hand.
            View::Filters => {
                let Some(name) = catalog_under_cursor(ui) else {
                    return;
                };
                apply_catalog_spec(ui, ctx, crate::catalog::Spec::Only(vec![name]));
            }
            // Enter *confirms*: it opens or closes a node, and on a track —
            // where there is nothing left to descend into — it plays. That is
            // snacks' rule, and with all three levels in one pane there is no
            // second pane to descend into instead.
            View::Tree => {
                let leaf = ui
                    .tree
                    .rows(&ui.library, ctx.store)
                    .get(ui.tree.selected)
                    .map(|r| r.is_leaf())
                    .unwrap_or(false);
                if leaf {
                    if let Some(id) = ui.selected_track(ctx) {
                        ui.playback.active = ActiveSource::Library;
                        play(ui, ctx, id);
                    }
                } else {
                    ui.tree.toggle(&ui.library, ctx.store);
                }
            }
            // Elsewhere there is nothing to confirm. Inert in a context is
            // already ordinary here — `h` outside the tree does nothing too.
            _ => {}
        },
        // `dd` takes a count, as in vim: `3dd` removes three. It cuts —
        // the rows go to the register, so `dd` … `p` moves them, here or in
        // another playlist.
        Command::Remove => remove_rows(ui, ctx, n),
        Command::Yank => yank_rows(ui, ctx, n),
        Command::Undo => step_history(ui, ctx, true),
        Command::Redo => step_history(ui, ctx, false),
        Command::YankToEnd => yank_rows(ui, ctx, len.saturating_sub(cursor)),
        Command::Put { before } => put_rows(ui, ctx, before, n),
        // `D` is delete-to-end. There is no "inside a line" here, so the list
        // is what has an end.
        Command::RemoveToEnd => remove_rows(ui, ctx, len.saturating_sub(cursor)),
        Command::Rescan => ui.notify("rescan lands with :rescan in the app"),
        Command::Playlist(spec) => match spec {
            crate::cmd::PlaylistSpec::Open(name) => open_playlist(ui, ctx, &name),
            crate::cmd::PlaylistSpec::Save(name) => save_playlist(ui, ctx, &name),
            crate::cmd::PlaylistSpec::New(name) => new_playlist(ui, ctx, &name),
            crate::cmd::PlaylistSpec::List => list_playlists(ui),
        },
        Command::Freeze => match ui.open.take() {
            Some(Open::Rule(name)) => ui.notify(format!(
                "froze {name} — {} tracks; `:playlist save <name>` keeps it",
                ui.playlist.len()
            )),
            other => {
                ui.open = other;
                ui.notify("only a rule can be frozen");
            }
        },

        // ---- transport
        // `x` plays **what is under the cursor**. It used to mean "resume if
        // something is playing, else play the selection" — two behaviours on
        // one key, the first of which is what `c` already does.
        Command::Play => match ui.selected_track(ctx) {
            Some(id) => {
                ui.playback.active = match ui.view {
                    View::Playlist => ActiveSource::Playlist,
                    _ => ActiveSource::Library,
                };
                // The queue is consumed as it plays, so playing from it takes
                // the entry rather than leaving a copy behind.
                if ui.view == View::Queue {
                    ui.queue.pop_front();
                }
                play(ui, ctx, id);
            }
            // Nothing selectable here — scope, settings, the browser. Resuming
            // is the only sensible reading of "play".
            None => {
                if let Some(p) = ctx.player {
                    p.play();
                }
            }
        },
        Command::Mode(want) => {
            ui.mode = want;
            // A part-typed chord belongs to the keymap it was started in.
            ui.pending.clear();
            ui.count = None;
        }
        // vim's `50%`, and the count is read the same way `12G` reads it.
        Command::SeekPercent => {
            let want = count.unwrap_or(50).min(100) as f64 / 100.0;
            if let (Some(player), Some(id)) = (ctx.player, ui.playback.current)
                && let Some(track) = ctx.store.get(id)
            {
                let to = track.duration.mul_f64(want);
                player.seek(crate::audio::SeekTo::Absolute(to));
            }
        }
        Command::Pause => {
            if let Some(p) = ctx.player {
                p.toggle();
            }
        }
        Command::Stop => {
            if let Some(p) = ctx.player {
                p.stop();
            }
        }
        Command::Next => {
            for _ in 0..n {
                advance(ui, ctx, true);
            }
        }
        Command::Prev => {
            for _ in 0..n {
                advance(ui, ctx, false);
            }
        }
        Command::Seek(spec) => {
            if let Some(p) = ctx.player {
                p.seek(match spec {
                    SeekSpec::Absolute(s) => {
                        crate::audio::SeekTo::Absolute(Duration::from_secs_f64(s.max(0.0)))
                    }
                    SeekSpec::Relative(s) => crate::audio::SeekTo::Relative(s),
                });
            }
        }
        Command::Vol(spec) => {
            if let Some(p) = ctx.player {
                // Stepping the *position*, not the gain: a step of the gain
                // is what made `-` do almost nothing at the top of the range
                // and fall off a cliff at the bottom.
                let next = match spec {
                    VolSpec::Absolute(v) => v,
                    VolSpec::Relative(d) => p.volume() + d,
                };
                p.set_volume(next);
                ui.volume_hint = Some(p.volume() * 100.0);
                ui.notify("");
            }
        }

        // ---- library
        Command::Add { target, .. } => add_selection(ui, ctx, target),

        // ---- settings
        Command::Set { key, value } => apply_setting(ui, ctx, key, value),
        Command::Toggle(key) => {
            let value = match key {
                SettingKey::AaaMode => SettingValue::Aaa(ui.playback.aaa_mode.cycle()),
                SettingKey::ReplayGain => SettingValue::ReplayGain(ui.replaygain.cycle()),
                SettingKey::Shuffle => SettingValue::Bool(!ui.playback.shuffle),
                SettingKey::Repeat => SettingValue::Bool(!ui.playback.repeat),
                SettingKey::RepeatCurrent => SettingValue::Bool(!ui.playback.repeat_current),
                SettingKey::Continue => SettingValue::Bool(!ui.continue_playing),
                SettingKey::AlbumHeaders => SettingValue::Bool(!ui.album_headers),
                SettingKey::RelativeNumbers => SettingValue::Bool(!ui.relative_numbers),
                SettingKey::SmartArtistSort => SettingValue::Bool(!ui.smart_artist_sort),
                SettingKey::ResampleQuality => {
                    ui.notify("resample_quality needs :set, it is not a toggle");
                    return;
                }
                SettingKey::WhichKeyDelay => {
                    ui.notify("which_key_delay needs :set, it is a number");
                    return;
                }
            };
            apply_setting(ui, ctx, key, value);
        }
        Command::Bind {
            ctx: bctx,
            key,
            cmd,
        } => {
            let key = crate::cmd::keys::resolve_leader(&key, ui.leader);
            ui.bindings.insert((bctx, key), *cmd);
        }
        Command::Unbind { ctx: bctx, key } => {
            let key = crate::cmd::keys::resolve_leader(&key, ui.leader);
            if ui.bindings.remove(&(bctx, key.clone())).is_none() {
                ui.notify(format!(
                    "{} is not bound in {}",
                    crate::cmd::keys::format_seq(&key),
                    bctx.name()
                ));
            }
        }

        // ---- views
        Command::View(id) => switch_view(ui, View::from_id(id)),
        // `gt` steps forward; `<count>gt` goes straight to that view.
        Command::ViewNext => match count {
            Some(n) => {
                if let Some(id) = ViewId::from_number(n as u8) {
                    switch_view(ui, View::from_id(id));
                }
            }
            None => switch_view(ui, ui.view.next()),
        },
        Command::ViewPrev => switch_view(ui, ui.view.prev()),
        Command::ViewLast => switch_view(ui, ui.previous_view),
        Command::Search { pattern, dir } => {
            if pattern.is_empty() {
                return;
            }
            ui.last_search = Some((pattern.clone(), dir));
            ui.highlight = Some(crate::text::Needle::new(&pattern));
            run_search(ui, ctx, &pattern, dir);
        }
        Command::NoHighlight => ui.highlight = None,
        Command::SearchNext | Command::SearchPrev => {
            let Some((pattern, dir)) = ui.last_search.clone() else {
                ui.notify("no previous search");
                return;
            };
            let dir = if cmd == Command::SearchPrev {
                match dir {
                    SearchDir::Forward => SearchDir::Backward,
                    SearchDir::Backward => SearchDir::Forward,
                }
            } else {
                dir
            };
            run_search(ui, ctx, &pattern, dir);
        }
        Command::Rate(spec) => rate(ui, ctx, spec),
        Command::Filter(filter) => {
            let label = match &filter {
                Some(f) => format!("filtered: {}", f.source),
                None => "filter cleared".into(),
            };
            ui.filter = filter;
            rebuild_library(ui, ctx);
            if ui.library.is_empty() {
                ui.notify("nothing matches");
            } else {
                ui.notify(format!("{label} ({} tracks)", ui.library.len()));
            }
        }
        Command::Catalog(spec) => match ui.catalogs.apply(spec) {
            Ok(()) => {
                rebuild_library(ui, ctx);
                match ui.catalogs.label() {
                    Some(label) => ui.notify(format!("catalog: {label}")),
                    None => ui.notify("catalog: all"),
                }
            }
            Err(e) => ui.notify(e),
        },
        Command::Help => show_help(ui),
        Command::Quit => ui.quit = true,
    }
}

/// Rebuild the library projection and put the cursors back somewhere valid.
///
/// Called whenever membership or ordering changes — a catalog switch, or
/// `smart_artist_sort`. The cursors have to be clamped: the row they pointed at
/// may not exist any more.
fn catalog_under_cursor(ui: &Ui) -> Option<String> {
    ui.catalogs
        .defined()
        .get(ui.catalog_sel)
        .map(|c| c.name.clone())
}

fn toggle_catalog_under_cursor(ui: &mut Ui, ctx: &Context<'_>) {
    let Some(name) = catalog_under_cursor(ui) else {
        return;
    };
    // `Spec::Adjust` already expands "all" to every name before applying the
    // delta, so turning one off while everything is active does the right
    // thing without a special case here — an earlier version had one, and a
    // mutation showed it was doing nothing.
    let add = !ui.catalogs.is_active(&name);
    apply_catalog_spec(ui, ctx, crate::catalog::Spec::Adjust(vec![(add, name)]));
}

fn apply_catalog_spec(ui: &mut Ui, ctx: &Context<'_>, spec: crate::catalog::Spec) {
    match ui.catalogs.apply(spec) {
        Ok(()) => {
            rebuild_library(ui, ctx);
            let label = ui.catalogs.label().unwrap_or_else(|| "all".into());
            ui.notify(format!("catalogs: {label} ({} tracks)", ui.library.len()));
        }
        Err(e) => ui.notify(e),
    }
}

/// Why an edit to view 3 was refused.
///
/// Names the escape hatch rather than only saying no: the wish is legitimate,
/// it just cannot be granted while a rule owns the list.
/// How far ahead view 4 projects. `repeat` makes the true answer infinite, so
/// a limit is not a cap on a finite thing — it is the contract.
const UP_NEXT: usize = 200;

/// What would play next, if nothing else were done (#56).
///
/// Costs nothing new: shuffle is a seeded permutation, so `next()` already
/// materialises the whole play order on every call and keeps one element. This
/// reads the rest of it.
fn up_next(ui: &Ui, ctx: &Context<'_>) -> Vec<(crate::store::TrackId, crate::next::Upcoming)> {
    let _ = ctx;
    crate::next::upcoming(&next_input(ui), UP_NEXT)
}

/// Whether the row under the cursor in view 4 is one the queue owns.
///
/// Only those can be edited. The rest is a **projection** — the pool's own
/// order, which has no seed that would yield "this permutation but with one
/// track moved", so an edit there has nowhere real to land.
fn queued_at_cursor(ui: &Ui, ctx: &Context<'_>) -> bool {
    up_next(ui, ctx)
        .get(ui.queue_sel)
        .is_some_and(|(_, from)| *from == crate::next::Upcoming::Queued)
}

fn projected(what: &str) -> String {
    format!("that row is what would play next, not a queued one — nothing to {what}")
}

fn rule_owned(what: &str) -> String {
    format!("this playlist is a rule — `:freeze` it to {what} it by hand")
}

/// Open a playlist by name, whichever kind it is.
///
/// A rule and a saved list are both play *sources* (#15), so they share one
/// verb rather than colliding in two. A rule wins a name clash, because it is
/// the one that cannot be renamed from inside the app.
fn open_playlist(ui: &mut Ui, ctx: &Context<'_>, name: &str) {
    if ui.playlists.get(name).is_some() {
        load_smart(ui, ctx, name);
        return;
    }
    if let Some(found) = ui.saved.as_ref().and_then(|s| s.find(name)) {
        load_saved(ui, ctx, &found);
        return;
    }
    let known = known_playlists(ui);
    ui.notify(if known.is_empty() {
        "no playlists — `:playlist save <name>` writes the current one".into()
    } else {
        format!("no playlist {name:?} — try one of: {}", known.join(", "))
    });
}

/// Rebuild view 7's rows.
///
/// Rules first, then saved files, each alphabetical — rules cannot be created
/// or renamed from inside the app, so they are the fixed part of the list and
/// keeping them at the top stops the interesting half jumping around as
/// playlists are saved.
fn refresh_playlist_rows(ui: &mut Ui) {
    let mut rows: Vec<PlaylistRow> = Vec::new();
    let mut rules: Vec<&crate::smart::SmartPlaylist> = ui.playlists.all().collect();
    rules.sort_by(|a, b| a.name.cmp(&b.name));
    for rule in rules {
        rows.push(PlaylistRow {
            name: rule.name.clone(),
            detail: rule.filter.source.clone(),
            rule: true,
        });
    }
    if let Some(saved) = &ui.saved {
        for s in saved.list() {
            if rows.iter().any(|r| r.name == s.name) {
                continue;
            }
            // Read once, here, rather than per frame. A playlist that cannot
            // be read still gets a row: it exists, and hiding it would be
            // stranger than saying so.
            let detail = match crate::m3u::read(&s.path) {
                Ok(paths) => match paths.len() {
                    1 => "1 track".to_string(),
                    n => format!("{n} tracks"),
                },
                Err(_) => "unreadable".to_string(),
            };
            rows.push(PlaylistRow {
                name: s.name,
                detail,
                rule: false,
            });
        }
    }
    ui.playlist_rows = rows;
    ui.playlists_sel = ui
        .playlists_sel
        .min(ui.playlist_rows.len().saturating_sub(1));
}

/// Every name `:playlist open` would accept.
fn known_playlists(ui: &Ui) -> Vec<String> {
    let mut names: Vec<String> = ui
        .playlists
        .names()
        .into_iter()
        .map(str::to_string)
        .collect();
    if let Some(saved) = &ui.saved {
        for s in saved.list() {
            if !names.contains(&s.name) {
                names.push(s.name);
            }
        }
    }
    names.sort();
    names
}

fn list_playlists(ui: &mut Ui) {
    let known = known_playlists(ui);
    if !known.is_empty() {
        ui.notify(known.join(", "));
        return;
    }
    // Naming the directory turns "there are none" into something actionable —
    // and it is where you would drop one in from another program.
    ui.notify(
        match ui.saved.as_ref().map(|s| s.dir().display().to_string()) {
            Some(dir) => format!("no playlists yet — they live in {dir}"),
            None => "no playlists — set `data_dir` in the config".into(),
        },
    );
}

/// Fill view 3 from a saved `.m3u8`.
fn load_saved(ui: &mut Ui, ctx: &Context<'_>, found: &crate::playlist::Saved) {
    let paths = match crate::m3u::read(&found.path) {
        Ok(p) => p,
        Err(e) => {
            ui.notify(e.to_string());
            return;
        }
    };
    // A playlist names paths; the store may not know all of them — the file
    // could predate a scan, or name music that has gone. Said out loud rather
    // than silently short, because a playlist quietly missing three tracks is
    // how you stop trusting it.
    let mut missing = 0;
    ui.playlist.clear();
    for path in &paths {
        match ctx.store.id_for_path(path) {
            Some(id) => ui.playlist.push(id),
            None => missing += 1,
        }
    }
    ui.playlist_sel = 0;
    ui.open = Some(Open::Saved(found.name.clone()));
    switch_view(ui, View::Playlist);
    select_open_row(ui);
    ui.playlist_pane = Pane::Tracks;
    ui.notify(match missing {
        0 => format!("{}: {} tracks", found.name, ui.playlist.len()),
        n => format!(
            "{}: {} tracks, {n} not in the library",
            found.name,
            ui.playlist.len()
        ),
    });
}

/// What works here, as `key  command` rows.
///
/// The current view's context and `common`, because that is what is actually
/// reachable — and the view's own bindings first, since those are the ones
/// that differ from everywhere else and so the ones worth learning.
fn show_help(ui: &mut Ui) {
    let here = ui.context();
    let mut rows: Vec<String> = Vec::new();
    // In a mode there *is* no fallback, so listing `common` would be listing
    // keys that do nothing — the same rule `lookup` follows.
    for want in [here, crate::cmd::Context::Common] {
        if want == crate::cmd::Context::Common
            && (here == crate::cmd::Context::Common || !here.inherits_common())
        {
            continue;
        }
        for (ctx, key, cmd) in bind::sorted(&ui.bindings) {
            if ctx == want {
                rows.push(format!("{key}  {}", crate::cmd::parse::format(cmd)));
            }
        }
    }
    ui.help = Some(rows);
    ui.help_scroll = 0;
}

/// The open playlist's history, if it has one.
///
/// Only a *saved* playlist: a rule cannot be edited, so it has nothing to
/// undo, and the library is a projection of the store rather than a list.
fn playlist_key(ui: &Ui) -> Option<crate::undo::Key> {
    match &ui.open {
        Some(Open::Saved(name)) => Some(crate::undo::Key::Playlist(name.clone())),
        _ => None,
    }
}

/// Which list `u` and `^r` act on: the one you are looking at.
fn undo_key(ui: &Ui) -> Option<crate::undo::Key> {
    match ui.view {
        View::Playlist => playlist_key(ui),
        View::Queue => Some(crate::undo::Key::Queue),
        _ => None,
    }
}

/// Which list a command *changes* — which is not always the one on screen.
///
/// `a` in the library adds to the open playlist, so recording by view would
/// have filed that edit under nothing and left it un-undoable. What a command
/// edits is a property of the command.
fn edited_key(ui: &Ui, cmd: &Command) -> Option<crate::undo::Key> {
    match cmd {
        Command::Add {
            target: AddTarget::Playlist,
            ..
        } => playlist_key(ui),
        Command::Add {
            target: AddTarget::Queue,
            ..
        } => Some(crate::undo::Key::Queue),
        Command::Remove | Command::RemoveToEnd | Command::Put { .. } => undo_key(ui),
        _ => None,
    }
}

fn snapshot_of(ui: &Ui, key: &crate::undo::Key) -> crate::undo::Snapshot {
    match key {
        crate::undo::Key::Queue => crate::undo::Snapshot {
            entries: ui.queue.entries().copied().collect(),
            selected: ui.queue_sel,
        },
        crate::undo::Key::Playlist(_) => crate::undo::Snapshot {
            entries: ui.playlist.entries().to_vec(),
            selected: ui.playlist_sel,
        },
    }
}

/// Remember the list as it is, before changing it.
///
/// Called from `execute` for every editing command rather than inside each
/// one, so a new edit cannot forget to be undoable — which is the failure that
/// makes an undo stack worse than none, because it is trusted until the once
/// it is not.
fn remember(ui: &mut Ui, cmd: &Command) {
    if let Some(key) = edited_key(ui, cmd) {
        let now = snapshot_of(ui, &key);
        ui.undo.record(key, now);
    }
}

/// `u` and `^r`.
fn step_history(ui: &mut Ui, ctx: &Context<'_>, back: bool) {
    let Some(key) = undo_key(ui) else {
        ui.notify("nothing here keeps an undo history");
        return;
    };
    let now = snapshot_of(ui, &key);
    let step = if back {
        ui.undo.undo(&key, now)
    } else {
        ui.undo.redo(&key, now)
    };
    let Some(step) = step else {
        ui.notify(if back {
            "already at the oldest change"
        } else {
            "already at the newest change"
        });
        return;
    };

    match key {
        crate::undo::Key::Queue => {
            ui.queue.clear();
            for id in &step.entries {
                ui.queue.push_back(*id);
            }
            ui.queue_sel = step.selected.min(ui.queue.len().saturating_sub(1));
        }
        crate::undo::Key::Playlist(_) => {
            ui.playlist.clear();
            for id in &step.entries {
                ui.playlist.push(*id);
            }
            ui.playlist_sel = step.selected.min(ui.playlist.len().saturating_sub(1));
            // An undo is an edit like any other: the file follows.
            persist_open(ui, ctx);
        }
    }
    ui.notify(if back { "undo" } else { "redo" });
}

/// The rows `n` from the cursor covers, in whichever list is focused.
fn rows_at_cursor(ui: &Ui, n: usize) -> Vec<TrackId> {
    let (entries, at) = match ui.view {
        View::Playlist if ui.playlist_pane == Pane::Tracks => {
            (ui.playlist.entries().to_vec(), ui.playlist_sel)
        }
        View::Playlist => return Vec::new(),
        View::Queue => (ui.queue.entries().copied().collect(), ui.queue_sel),
        View::Sorted => (ui.library.sorted().to_vec(), ui.sorted),
        _ => return Vec::new(),
    };
    entries.into_iter().skip(at).take(n).collect()
}

/// `yy` — copy without removing.
///
/// Works in the library too, not only in a playlist: yanking from view 2 and
/// putting into a playlist is the same act as `a`, spelled the way vim spells
/// it, and refusing it would be arbitrary.
fn yank_rows(ui: &mut Ui, ctx: &Context<'_>, n: usize) {
    let rows = rows_at_cursor(ui, n);
    if rows.is_empty() {
        ui.notify("nothing to yank here");
        return;
    }
    let count = rows.len();
    ui.register = rows;
    let _ = ctx;
    ui.notify(match count {
        1 => "yanked 1 track".to_string(),
        n => format!("yanked {n} tracks"),
    });
}

/// `p` / `P` — put the register after or before the cursor.
///
/// Only into a list that holds tracks of its own. The library is a projection
/// of the store, so there is nothing to put *into* — §4 makes that a rebuild,
/// not an edit.
fn put_rows(ui: &mut Ui, ctx: &Context<'_>, before: bool, times: usize) {
    if ui.register.is_empty() {
        ui.notify("the register is empty — `yy` or `dd` fills it");
        return;
    }
    let rows: Vec<TrackId> = ui
        .register
        .iter()
        .cycle()
        .take(ui.register.len() * times.max(1))
        .copied()
        .collect();
    let count = rows.len();

    match ui.view {
        View::Playlist if ui.playlist_pane == Pane::Tree => {
            ui.notify("put into the tracks — `tab` moves there")
        }
        View::Playlist if ui.open.as_ref().is_some_and(Open::is_rule) => {
            ui.notify(rule_owned("put into"))
        }
        View::Playlist if ui.open.is_none() => {
            ui.notify("no playlist open — `:playlist new <name>` starts one")
        }
        View::Playlist => {
            // After the cursor, or before it — vim's `p` and `P`. That is what
            // makes `dd` then `p` swap a track with the next one.
            let at = if ui.playlist.is_empty() {
                0
            } else if before {
                ui.playlist_sel
            } else {
                ui.playlist_sel + 1
            };
            for (i, id) in rows.into_iter().enumerate() {
                ui.playlist.insert(at + i, id);
            }
            // The cursor follows what was put, as vim's does.
            ui.playlist_sel = at;
            persist_open(ui, ctx);
            ui.notify(format!("put {count}"));
        }
        View::Queue => {
            let at = if ui.queue.is_empty() {
                0
            } else if before {
                ui.queue_sel
            } else {
                ui.queue_sel + 1
            };
            for (i, id) in rows.into_iter().enumerate() {
                ui.queue.insert(at + i, id);
            }
            ui.queue_sel = at;
            ui.notify(format!("put {count}"));
        }
        _ => ui.notify("nothing to put into here"),
    }
}

/// Remove `n` rows from the cursor down, in whichever list is focused.
///
/// Removing repeatedly at the same index is what "the next one moves up under
/// the cursor" means, so this needs no index arithmetic. `Playlist::remove`
/// bounds-checks and returns `None`, so an overrunning count is already safe;
/// the `break` only saves spinning through a large one.
fn remove_rows(ui: &mut Ui, ctx: &Context<'_>, n: usize) {
    match ui.view {
        View::Playlist if ui.playlist_pane == Pane::Tree => {
            ui.notify("that removes a track — `:playlist delete` is not a thing yet")
        }
        View::Playlist if ui.open.as_ref().is_some_and(Open::is_rule) => {
            ui.notify(rule_owned("remove from"))
        }
        View::Playlist => {
            // `dd` cuts: the rows land in the register, so `dd` … `p` moves
            // them — here, or into another playlist.
            ui.register = rows_at_cursor(ui, n);
            for _ in 0..n {
                if ui.playlist.remove(ui.playlist_sel).is_none() {
                    break;
                }
            }
            ui.playlist_sel = ui.playlist_sel.min(ui.playlist.len().saturating_sub(1));
            persist_open(ui, ctx);
        }
        View::Queue if !queued_at_cursor(ui, ctx) => ui.notify(projected("remove")),
        View::Queue => {
            ui.register = rows_at_cursor(ui, n);
            for _ in 0..n {
                if ui.queue.remove(ui.queue_sel).is_none() {
                    break;
                }
            }
            ui.queue_sel = ui.queue_sel.min(ui.queue.len().saturating_sub(1));
        }
        _ => ui.notify("nothing to remove here"),
    }
}

/// Start an empty playlist and open it.
///
/// The alternative to an invisible scratch list: adding to a playlist means
/// adding to *the one you picked*, so there has to be a way to pick a new one.
fn new_playlist(ui: &mut Ui, ctx: &Context<'_>, name: &str) {
    let Some(saved) = ui.saved.clone() else {
        ui.notify("nowhere to keep playlists — set `data_dir` in the config");
        return;
    };
    if saved.find(name).is_some() || ui.playlists.get(name).is_some() {
        ui.notify(format!("{name} already exists — `:playlist open {name}`"));
        return;
    }
    // Written empty rather than held in memory, so it survives a crash and so
    // it appears to anything else reading the directory.
    if let Err(e) = crate::m3u::write(&saved.path_for(name), &[]) {
        ui.notify(e.to_string());
        return;
    }
    ui.playlist.clear();
    ui.playlist_sel = 0;
    ui.open = Some(Open::Saved(name.to_string()));
    refresh_playlist_rows(ui);
    select_open_row(ui);
    switch_view(ui, View::Playlist);
    ui.playlist_pane = Pane::Tracks;
    let _ = ctx;
    ui.notify(format!("{name}: empty — add with `y`"));
}

/// Put the left pane's cursor on whatever is open.
fn select_open_row(ui: &mut Ui) {
    if let Some(open) = &ui.open
        && let Some(i) = ui.playlist_rows.iter().position(|r| r.name == open.name())
    {
        ui.playlists_sel = i;
    }
}

/// Write the open playlist back to its file.
///
/// Called after every edit rather than at exit: the same rule ratings and
/// plays follow, and for the same reason — losing what you meant is worse
/// than the cost of a write.
fn persist_open(ui: &mut Ui, ctx: &Context<'_>) {
    let (Some(Open::Saved(name)), Some(saved)) = (ui.open.clone(), ui.saved.clone()) else {
        return;
    };
    let tracks: Vec<&crate::store::Track> = ui
        .playlist
        .entries()
        .iter()
        .filter_map(|id| ctx.store.get(*id))
        .collect();
    if let Err(e) = crate::m3u::write(&saved.path_for(&name), &tracks) {
        ui.notify(e.to_string());
        return;
    }
    refresh_playlist_rows(ui);
}

/// Copy the open playlist under a new name.
fn save_playlist(ui: &mut Ui, ctx: &Context<'_>, name: &str) {
    let Some(saved) = ui.saved.clone() else {
        ui.notify("nowhere to save playlists — set `data_dir` in the config");
        return;
    };
    if ui.playlist.is_empty() {
        ui.notify("the playlist is empty — nothing to save");
        return;
    }
    let tracks: Vec<&crate::store::Track> = ui
        .playlist
        .entries()
        .iter()
        .filter_map(|id| ctx.store.get(*id))
        .collect();
    let at = saved.path_for(name);
    match crate::m3u::write(&at, &tracks) {
        Ok(()) => {
            // Saving a rule-defined list writes what it *currently* holds and
            // detaches it, which is what `:freeze` does — so say so rather
            // than leaving the title claiming a rule still owns it.
            // Saving over an existing playlist replaces its contents, so its
            // undo history no longer describes anything — `u` would restore a
            // list this name never held.
            ui.undo
                .forget(&crate::undo::Key::Playlist(name.to_string()));
            let was = ui.open.replace(Open::Saved(name.to_string()));
            refresh_playlist_rows(ui);
            ui.notify(match was {
                Some(Open::Rule(rule)) => {
                    format!("saved {} tracks as {name}, a copy of {rule}", tracks.len())
                }
                _ => format!("saved {} tracks as {name}", tracks.len()),
            });
        }
        Err(e) => ui.notify(e.to_string()),
    }
}

/// Fill view 3 from a named smart playlist.
fn load_smart(ui: &mut Ui, ctx: &Context<'_>, name: &str) {
    let Some(spec) = ui.playlists.get(name) else {
        return;
    };
    let ids = spec.members(ctx.store, ui.sort_env());
    let count = ids.len();
    // An empty result is the confusing case, so it says what the rule was
    // rather than leaving you to go and read the config.
    let said = if count == 0 {
        format!("{name}: nothing matches {}", spec.rule_label())
    } else {
        format!("{name}: {count} tracks")
    };
    ui.playlist.clear();
    for id in ids {
        ui.playlist.push(id);
    }
    ui.playlist_sel = 0;
    ui.open = Some(Open::Rule(name.to_string()));
    switch_view(ui, View::Playlist);
    select_open_row(ui);
    ui.playlist_pane = Pane::Tracks;
    ui.notify(said);
}

/// Recompute the loaded smart playlist, if there is one.
///
/// Called after anything that can change what the rule answers — a play, a
/// rating, a rescan. §15 settled that a smart playlist is a *live query*, and
/// this is what keeps that true without re-evaluating it on every redraw.
///
/// The cursor is kept where it was rather than reset: a list that jumped to
/// the top whenever a play count ticked over would be unusable.
fn refresh_smart(ui: &mut Ui, ctx: &Context<'_>) {
    let Some(Open::Rule(name)) = ui.open.clone() else {
        return;
    };
    let Some(spec) = ui.playlists.get(&name) else {
        return;
    };
    let ids = spec.members(ctx.store, ui.sort_env());
    ui.playlist.clear();
    for id in ids {
        ui.playlist.push(id);
    }
    ui.playlist_sel = ui.playlist_sel.min(ui.playlist.len().saturating_sub(1));
}

fn rebuild_library(ui: &mut Ui, ctx: &Context<'_>) {
    // §7's rule: active catalogs union, the transient filter intersects.
    // "Which bodies of music am I in" is a selection; "what am I looking for"
    // is a restriction.
    let members = {
        // Scoped so the borrows of `history` and `filter` end before the
        // library is replaced below.
        let env = crate::filter::Env {
            history: Some(&ui.history),
            now: crate::history::now(),
        };
        let mut m = ui.catalogs.members(ctx.store, env);
        if let Some(f) = &ui.filter {
            let matches = f.compile(env);
            m.retain(|id| ctx.store.get(*id).is_some_and(&matches));
        }
        m
    };
    ui.library = Library::from_members(ctx.store, members, ui.smart_artist_sort);
    ui.tree.selected = 0;
    ui.sorted = 0;
    // Playlists and the queue are deliberately untouched: they hold TrackIds
    // straight into the Store and are meant to cross catalogs.
    //
    // A smart playlist is re-evaluated rather than left alone, but for a
    // different reason: it did not cross a catalog boundary, the *store* may
    // have changed under it.
    refresh_smart(ui, ctx);
}

/// Change view, remembering where we came from so `g<Tab>` can go back.
fn switch_view(ui: &mut Ui, view: View) {
    // Entering view 3 is the moment the list matters — and the moment another
    // program may have dropped a playlist into the directory.
    if view == View::Playlist {
        refresh_playlist_rows(ui);
    }
    if view != ui.view {
        ui.previous_view = ui.view;
    }
    ui.view = view;
    ui.status.clear();
}

fn apply_setting(ui: &mut Ui, ctx: &Context<'_>, key: SettingKey, value: SettingValue) {
    // Every change, so the log carries the whole state trajectory rather than
    // just its effects. `replaygain=auto` resolves per track from `shuffle`,
    // so which figure a gain came from is only reconstructable if both are
    // recorded — and reconstructing it from the album ordering, as had to be
    // done once, is not a diagnostic.
    ui.audit
        .line("setting", &format!("{}={}", key.name(), value));

    match (key, value) {
        (SettingKey::AaaMode, SettingValue::Aaa(mode)) => ui.playback.aaa_mode = mode,
        (SettingKey::ReplayGain, SettingValue::ReplayGain(m)) => ui.replaygain = m,
        (SettingKey::Shuffle, SettingValue::Bool(v)) => {
            ui.playback.shuffle = v;
            ui.playback.reshuffle();
        }
        (SettingKey::Repeat, SettingValue::Bool(v)) => ui.playback.repeat = v,
        (SettingKey::RepeatCurrent, SettingValue::Bool(v)) => ui.playback.repeat_current = v,
        (SettingKey::Continue, SettingValue::Bool(v)) => ui.continue_playing = v,
        (SettingKey::AlbumHeaders, SettingValue::Bool(v)) => ui.album_headers = v,
        (SettingKey::RelativeNumbers, SettingValue::Bool(v)) => ui.relative_numbers = v,
        (SettingKey::WhichKeyDelay, SettingValue::Millis(ms)) => {
            ui.which_key_delay = Duration::from_millis(ms as u64);
        }
        (SettingKey::SmartArtistSort, SettingValue::Bool(v)) => {
            ui.smart_artist_sort = v;
            // The projection is ours now, so this takes effect immediately.
            rebuild_library(ui, ctx);
        }
        (SettingKey::ResampleQuality, SettingValue::Quality(_)) => {
            let _ = ctx;
            ui.notify("resample_quality applies to the next track loaded");
        }
        // The parser types values per key, so this is unreachable in practice.
        (k, v) => ui.notify(format!("{} cannot be {v}", k.name())),
    }
}

/// `a`/`y`/`e`: put the selection somewhere.
fn add_selection(ui: &mut Ui, ctx: &Context<'_>, target: AddTarget) {
    let ids: Vec<crate::store::TrackId> = match ui.view {
        // From the tree, add everything the cursor covers — an album or an
        // artist — which is the whole point of adding from a tree.
        View::Tree => ui.tree.tracks_for(&ui.library, ctx.store),
        _ => ui.selected_track(ctx).into_iter().collect(),
    };
    if ids.is_empty() {
        ui.notify("nothing selected");
        return;
    }
    let count = ids.len();
    match target {
        AddTarget::Playlist => {
            match &ui.open {
                Some(o) if o.is_rule() => {
                    ui.notify(rule_owned("add to"));
                    return;
                }
                // Adding means adding to the one you picked, so there has to
                // be one. Naming the remedy beats inventing a file.
                None => {
                    ui.notify("no playlist open — `:playlist new <name>` starts one");
                    return;
                }
                Some(_) => {}
            }
            for id in ids {
                ui.playlist.push(id);
            }
            let name = ui
                .open
                .as_ref()
                .map(|o| o.name().to_string())
                .unwrap_or_default();
            persist_open(ui, ctx);
            ui.notify(format!("added {count} to {name}"));
        }
        AddTarget::Queue => {
            for id in ids {
                ui.queue.push_back(id);
            }
            ui.notify(format!("queued {count}"));
        }
        AddTarget::Library => {
            ui.notify("the library is what you are looking at; use :add <path> to scan more")
        }
    }
}

/// A structural motion over a flat track list.
///
/// The album divider *is* the structure here — a "sibling" is the next album,
/// a "section" the next artist, and the "group" the run of tracks sharing an
/// album. Boundaries come from `list::album_starts`, the same key that decides
/// where a divider is drawn, so a motion always lands where the eye expects.
///
/// Returns a **track** index, which is what every flat view's cursor counts.
fn flat_step(ui: &Ui, ctx: &Context<'_>, motion: Motion, count: usize) -> Option<usize> {
    let owned;
    let ids: &[crate::store::TrackId] = match ui.view {
        View::Sorted => ui.library.sorted(),
        View::Playlist if ui.playlist_pane == Pane::Tracks => ui.playlist.entries(),
        View::Queue => {
            owned = ui.queue.entries().copied().collect::<Vec<_>>();
            &owned
        }
        _ => return None,
    };
    if ids.is_empty() {
        return None;
    }

    let starts = match motion {
        Motion::NextSection
        | Motion::PrevSection
        | Motion::NextSectionEnd
        | Motion::PrevSectionEnd => list::artist_starts(ctx.store, ids),
        _ => list::album_starts(ctx.store, ids),
    };
    let mut at = current_cursor(ui).min(ids.len() - 1);

    // The last row of the run `s` begins: everything up to the next start.
    let run_end = |s: usize| {
        starts
            .iter()
            .copied()
            .find(|n| *n > s)
            .map(|n| n - 1)
            .unwrap_or(ids.len() - 1)
    };
    // The start of the run the cursor is in.
    let run_start = |at: usize| starts.iter().rev().copied().find(|s| *s <= at);

    for _ in 0..count.max(1) {
        let next = match motion {
            Motion::NextSibling | Motion::NextSection | Motion::NextFold => {
                starts.iter().copied().find(|s| *s > at)
            }
            Motion::PrevSibling | Motion::PrevSection => {
                starts.iter().rev().copied().find(|s| *s < at)
            }
            // vim's `zk` is the end of the previous fold. In a flat list the
            // album run is the fold, so that is the row before this run began
            // — and landing there puts the cursor in the previous run, so a
            // second press walks back again.
            Motion::PrevFold => run_start(at).filter(|s| *s > 0).map(|s| s - 1),
            // The first track of the run the cursor is in — and if it is
            // already there, the one above, so `-` walks out rather than
            // sticking.
            Motion::Parent => starts
                .iter()
                .rev()
                .copied()
                .find(|s| *s < at)
                .filter(|_| starts.binary_search(&at).is_ok())
                .or_else(|| run_start(at))
                .filter(|s| *s != at),
            // The last track before the next run begins.
            Motion::GroupEnd => Some(run_end(at)).filter(|s| *s != at),
            // `][` — the end of this artist's run, then the end of the next.
            Motion::NextSectionEnd => match run_start(at).map(run_end) {
                Some(end) if end > at => Some(end),
                _ => starts.iter().copied().find(|s| *s > at).map(run_end),
            },
            // `[]` — the row before this run started is the last of the one
            // before it.
            Motion::PrevSectionEnd => run_start(at).filter(|s| *s > 0).map(|s| s - 1),
        };
        match next {
            Some(to) => at = to,
            None => break,
        }
    }
    Some(at)
}

/// The list `zz`, `zt` and `zb` act on: the one you are looking at.
///
/// `None` where the focused pane is not a scrolling list — view 3's set of
/// playlists and view 7's open guide both draw themselves, and neither is long
/// enough to want this.
/// `zz` on whatever pane has the cursor, when it has a `Scroll` to do it with.
///
/// Separate from `Command::Place` so a motion can recentre without going back
/// through `execute`, which would record a second undo step and a second entry
/// in the repeat state for one keystroke.
fn recentre(ui: &Ui) {
    if let Some(scroll) = focused_scroll(ui) {
        scroll.centre_next.set(true);
    }
}

/// How far a page motion moves, from the pane's real height (#45).
///
/// This used to be a hardcoded 10, so `^f` moved a third of a page on a tall
/// terminal and overshot what was on screen in a short one — the one thing a
/// page motion is for is landing somewhere you can relate to where you were.
///
/// vim keeps **two lines** of the old screen as an anchor (`:h CTRL-F`), and
/// that overlap is not incidental: it is what makes the jump readable.
fn page_height(ui: &Ui) -> usize {
    ui.body_height.get().saturating_sub(2).max(1)
}

/// Half a screen — `^d` and `^u`. No overlap to subtract: half a page already
/// leaves half the screen in common.
fn half_page_height(ui: &Ui) -> usize {
    (ui.body_height.get() / 2).max(1)
}

fn focused_scroll(ui: &Ui) -> Option<&Scroll> {
    Some(match ui.view {
        View::Tree => &ui.tree_scroll,
        View::Sorted => &ui.sorted_scroll,
        View::Playlist if ui.playlist_pane == Pane::Tracks => &ui.playlist_scroll,
        View::Queue => &ui.queue_scroll,
        View::Browser => &ui.browser_scroll,
        View::Guide if ui.guide_pane == Pane::Tree => &ui.guides_scroll,
        View::Playlist | View::Guide | View::Filters => return None,
    })
}

/// Put the cursor on the playing track, wherever the current view keeps it.
///
/// In view 1 that means opening the folds hiding it, and nothing else — the
/// point is to find it without losing the shape you built up looking for it.
fn go_to_playing(ui: &mut Ui, ctx: &Context<'_>) {
    let Some(id) = ui.playback.current else {
        ui.notify("nothing is playing");
        return;
    };
    let found = match ui.view {
        View::Tree => ui.tree.reveal(&ui.library, ctx.store, id),
        View::Sorted => match ui.library.sorted().iter().position(|t| *t == id) {
            Some(at) => {
                ui.sorted = at;
                true
            }
            None => false,
        },
        View::Playlist => match ui.playlist.entries().iter().position(|t| *t == id) {
            Some(at) => {
                ui.playlist_sel = at;
                ui.playlist_pane = Pane::Tracks;
                true
            }
            None => false,
        },
        View::Queue => match ui.queue.entries().position(|t| *t == id) {
            Some(at) => {
                ui.queue_sel = at;
                true
            }
            None => false,
        },
        _ => false,
    };
    if !found {
        ui.notify("the playing track is not in this view");
    }
}

/// Search from where the `/` prompt opened, on every keystroke.
///
/// vim's `incsearch`. Searching from the **origin** rather than from the
/// cursor is what makes it a preview: typing `bur` then `i` refines one search
/// rather than starting a second from wherever the first landed, so deleting a
/// character walks back to exactly where adding it went.
fn incsearch(ui: &mut Ui, ctx: &Context<'_>) {
    let Some(prompt) = ui.prompt.as_ref() else {
        return;
    };
    let PromptKind::Search(dir) = prompt.kind else {
        return;
    };
    let pattern = prompt.buffer.clone();
    if let Some(origin) = ui.search_origin {
        set_cursor(ui, origin);
    }
    if pattern.is_empty() {
        ui.highlight = None;
        return;
    }
    ui.highlight = Some(crate::text::Needle::new(&pattern));
    run_search(ui, ctx, &pattern, dir);
}

/// `Esc`, or backspacing past the sigil: put the cursor back and stop
/// highlighting. The search never happened.
fn abandon_search(ui: &mut Ui) {
    if let Some(origin) = ui.search_origin.take() {
        set_cursor(ui, origin);
    }
    ui.highlight = None;
}

/// Move the cursor to the next row matching `pattern`, wrapping once.
fn run_search(ui: &mut Ui, ctx: &Context<'_>, pattern: &str, dir: SearchDir) {
    let needle = crate::text::Needle::new(pattern);
    let haystack = search_rows(ui, ctx);
    if haystack.is_empty() {
        return;
    }
    let len = haystack.len();
    let start = current_cursor(ui);

    // Start one past the cursor so repeating a search advances.
    for offset in 1..=len {
        let index = match dir {
            SearchDir::Forward => (start + offset) % len,
            SearchDir::Backward => (start + len - (offset % len)) % len,
        };
        if haystack[index].contains(&needle) {
            set_cursor(ui, index);
            return;
        }
    }
    ui.notify(format!("no match for {pattern:?}"));
}

/// Folded text for each row of the current view, for searching. Folded here
/// rather than per comparison, so a search over the library does the work once
/// per row instead of once per row per keystroke.
fn search_rows(ui: &Ui, ctx: &Context<'_>) -> Vec<crate::text::Haystack> {
    let describe = |id: &crate::store::TrackId| {
        ctx.store
            .get(*id)
            .map(|t| {
                crate::text::Haystack::of(&format!(
                    "{} {} {}",
                    t.file_under(),
                    t.album_or_unknown(),
                    t.display_title()
                ))
            })
            .unwrap_or_else(|| crate::text::Haystack::of(""))
    };
    match ui.view {
        // Artists, albums and tracks are all rows now, so `/` searches the
        // lot — which is what a single buffer should do.
        View::Tree => ui
            .tree
            .rows(&ui.library, ctx.store)
            .iter()
            .map(|row| match row {
                tree::Row::Track { id, .. } => describe(id),
                other => crate::text::Haystack::of(&other.label()),
            })
            .collect(),
        View::Sorted => ui.library.sorted().iter().map(describe).collect(),
        View::Playlist => ui.playlist.entries().iter().map(describe).collect(),
        View::Queue => ui.queue.entries().map(describe).collect(),
        View::Browser => ui
            .browser
            .entries
            .iter()
            .map(|p| crate::text::Haystack::of(&p.display().to_string()))
            .collect(),
        View::Filters | View::Guide => Vec::new(),
    }
}

fn current_len(ui: &Ui, ctx: &Context<'_>) -> usize {
    match ui.view {
        View::Tree => ui.tree.rows(&ui.library, ctx.store).len(),
        View::Sorted => ui.library.sorted().len(),
        View::Playlist => match ui.playlist_pane {
            Pane::Tree => ui.playlist_rows.len(),
            Pane::Tracks => ui.playlist.len(),
        },
        // The whole projection, not just the queued part: the cursor ranges
        // over what would play next, and only the edits are confined.
        View::Queue => up_next(ui, ctx).len(),
        View::Browser => ui.browser.entries.len(),
        View::Filters => ui.catalogs.defined().len(),
        View::Guide => match ui.guide_pane {
            Pane::Tree => crate::guides::GUIDES.len(),
            // Scrolling *is* moving a cursor down the text, so the pane's
            // length is its line count and `G` reaches the end for free.
            // The furthest the text can scroll, from what was last drawn —
            // so `G` lands on the last screenful and `k` moves from there.
            Pane::Tracks => {
                let (lines, height) = ui.guide_view.get();
                lines.saturating_sub(height) + 1
            }
        },
    }
}

fn current_cursor(ui: &Ui) -> usize {
    match ui.view {
        View::Tree => ui.tree.selected,
        View::Sorted => ui.sorted,
        View::Playlist => match ui.playlist_pane {
            Pane::Tree => ui.playlists_sel,
            Pane::Tracks => ui.playlist_sel,
        },
        View::Queue => ui.queue_sel,
        View::Browser => ui.browser.selected,
        View::Filters => ui.catalog_sel,
        View::Guide => match ui.guide_pane {
            Pane::Tree => ui.guide_sel,
            Pane::Tracks => ui.guide_scroll,
        },
    }
}

fn set_cursor(ui: &mut Ui, next: usize) {
    match ui.view {
        View::Tree => ui.tree.selected = next,
        View::Sorted => ui.sorted = next,
        View::Playlist => match ui.playlist_pane {
            Pane::Tree => ui.playlists_sel = next,
            Pane::Tracks => ui.playlist_sel = next,
        },
        View::Queue => ui.queue_sel = next,
        View::Browser => ui.browser.selected = next,
        View::Filters => ui.catalog_sel = next,
        View::Guide => match ui.guide_pane {
            Pane::Tree => {
                ui.guide_sel = next;
                // A new guide starts at its top, not wherever the last one
                // happened to be scrolled to.
                ui.guide_scroll = 0;
            }
            Pane::Tracks => ui.guide_scroll = next,
        },
    }
}

/// View 7: the guides. Titles on the left, the guide on the right.
///
/// Two panes rather than one scrolling document, for the same reason view 1
/// and view 3 have them: the list is how you *choose*, and it must stay
/// visible while you read.
fn draw_guides(frame: &mut Frame, area: Rect, ui: &Ui) {
    let theme = &ui.theme;
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(28), Constraint::Percentage(72)]).areas(area);

    let mut titles: Vec<ListItem> = crate::guides::GUIDES
        .iter()
        .map(|g| ListItem::new(Line::from(Span::raw(format!("  {}", g.title)))))
        .collect();
    let mut state =
        ui.guides_scroll
            .state_within(Some(ui.guide_sel), titles.len(), view_height(left));
    pad_to_view(&mut titles, theme, left, state.offset());
    frame.render_stateful_widget(
        List::new(titles)
            .block(pane_block(theme, "guide", ui.guide_pane == Pane::Tree))
            .highlight_style(highlight(theme, ui.guide_pane == Pane::Tree)),
        left,
        &mut state,
    );
    ui.guides_scroll.remember(&state);

    let Some(guide) = crate::guides::GUIDES.get(ui.guide_sel) else {
        return;
    };
    let height = right.height.saturating_sub(2) as usize;
    let body = crate::guides::body(guide, &ui.bindings);
    let text = crate::guides::wrapped(&body, right.width.saturating_sub(3) as usize);
    // Recorded so the cursor's range matches what is actually on screen. Only
    // here are the pane's width and height known.
    ui.guide_view.set((text.len(), height));
    // Scrolled by the cursor, so `j`, `3j`, `gg` and `G` reach the text with
    // no scroll keys of its own.
    let first = ui
        .guide_scroll
        .min(text.len().saturating_sub(height.max(1)));
    let lines: Vec<Line> = text
        .iter()
        .skip(first)
        .take(height)
        .map(|(kind, raw)| {
            let style = match kind {
                crate::guides::Kind::Heading => theme.get(theme::Role::SettingKey),
                crate::guides::Kind::Example => theme.get(theme::Role::StatusNotice),
                crate::guides::Kind::Prose => theme.get(theme::Role::StatusText),
            };
            Line::from(Span::styled(format!(" {raw}"), style))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(pane_block(
            theme,
            guide.title,
            ui.guide_pane == Pane::Tracks,
        )),
        right,
    );
}

/// View 3's left pane: what there is to open.
fn draw_playlists(frame: &mut Frame, area: Rect, ui: &Ui) {
    let theme = &ui.theme;
    let mut lines: Vec<Line> = Vec::new();

    if ui.playlist_rows.is_empty() {
        let hint = match ui.saved.as_ref().map(|s| s.dir().display().to_string()) {
            Some(dir) => {
                format!("  no playlists yet — `:playlist save <name>` writes one to {dir}")
            }
            None => "  no playlists — set `data_dir` in the config".to_string(),
        };
        lines.push(Line::from(Span::styled(
            hint,
            theme.get(theme::Role::StatusDim),
        )));
    }

    for (i, row) in ui.playlist_rows.iter().enumerate() {
        let open = ui.open.as_ref().is_some_and(|o| o.name() == row.name);
        let body = if i == ui.playlists_sel {
            theme.get(theme::Role::SelectionFocused)
        } else {
            theme.get(theme::Role::StatusText)
        };
        lines.push(Line::from(vec![
            // A rule is marked, not hidden: opening one gives a list you
            // cannot edit, and that is worth knowing before you press enter.
            Span::styled(
                if open { "  > " } else { "    " },
                theme.get(theme::Role::StatusPlaying),
            ),
            Span::styled(format!("{:<20}", row.name), body),
            Span::styled(
                if row.rule { "rule  " } else { "      " },
                theme.get(theme::Role::StatusNotice),
            ),
            Span::styled(row.detail.clone(), theme.get(theme::Role::StatusDim)),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines).block(pane_block(
            theme,
            "playlists",
            ui.playlist_pane == Pane::Tree,
        )),
        area,
    );
}

fn draw_catalogs(frame: &mut Frame, area: Rect, ui: &Ui) {
    let theme = &ui.theme;
    let defined = ui.catalogs.defined();

    let mut lines: Vec<Line> = Vec::new();
    if let Some(f) = &ui.filter {
        lines.push(Line::from(vec![
            Span::styled("  filter  ", theme.get(theme::Role::StatusDim)),
            Span::styled(f.source.clone(), theme.get(theme::Role::StatusNotice)),
        ]));
        lines.push(Line::from(""));
    }

    if defined.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no catalogs defined — see [catalogs] in the config",
            theme.get(theme::Role::StatusDim),
        )));
    }

    for (i, catalog) in defined.iter().enumerate() {
        let on = ui.catalogs.is_active(&catalog.name);
        let selected = i == ui.catalog_sel;
        let body = if selected {
            theme.get(theme::Role::SelectionFocused)
        } else if on {
            theme.get(theme::Role::StatusText)
        } else {
            theme.get(theme::Role::StatusDim)
        };
        lines.push(Line::from(vec![
            Span::styled(
                if on { "  [x] " } else { "  [ ] " },
                theme.get(if on {
                    theme::Role::StatusPlaying
                } else {
                    theme::Role::StatusDim
                }),
            ),
            Span::styled(format!("{:<14}", catalog.name), body),
            Span::styled(catalog.rule_label(), theme.get(theme::Role::StatusDim)),
        ]));
    }

    let title = match ui.catalogs.label() {
        Some(l) => format!("catalogs — {l}"),
        None => "catalogs — all".to_string(),
    };
    frame.render_widget(
        Paragraph::new(lines).block(pane_block(theme, &title, true)),
        area,
    );
}

pub fn draw(frame: &mut Frame, ui: &Ui, ctx: &Context<'_>) {
    // A completion menu takes room from the bottom of the body, so the view
    // above stays visible rather than being replaced.
    // The help popup shares the completion menu's slot: same shape, same
    // reason — a list of things you might type next, without covering the view
    // it is helping you use.
    // A third occupant of the same slot: the keys that could follow what you
    // have typed so far. Derived from `pending` rather than stored, for the
    // same reason the pending-key indicator is — there is no second copy of
    // the truth to keep in step.
    let which = which_key_rows(ui, Instant::now());
    let menu_height = match (&ui.prompt, &ui.help, &which) {
        (Some(p), _, _) => menu_height(p, frame.area().width),
        (None, Some(rows), _) => rows_height(rows, frame.area().width),
        (None, None, Some(rows)) => rows_height(rows, frame.area().width),
        _ => 0,
    };

    let [tabs, body, menu, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(menu_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    ui.body_height.set(view_height(body));

    // Cleared when nothing is drawn, so the key exception above is asking
    // about the popup that is actually on screen rather than the last one.
    if ui.prompt.is_some() || (ui.help.is_none() && which.is_none()) {
        ui.help_view.set((0, 0));
    }

    if menu_height > 0 {
        match (&ui.prompt, &ui.help, &which) {
            (Some(prompt), _, _) => draw_completions(frame, menu, ui, prompt),
            (None, Some(rows), _) => draw_help(
                frame,
                menu,
                ui,
                rows,
                // The mode's name when there is one: in transport mode these
                // are the mode's keys and not the view's, and naming the view
                // would be naming the thing they are *not* coming from.
                &format!(
                    "{} — keys that work here",
                    match ui.mode {
                        crate::cmd::Mode::Normal => ui.view.title(),
                        crate::cmd::Mode::Transport => "transport",
                    }
                ),
            ),
            // Named for the prefix rather than the view: what you are looking
            // at is the continuation of something you started typing.
            (None, None, Some(rows)) => draw_help(
                frame,
                menu,
                ui,
                rows,
                &format!("after {}", crate::cmd::keys::format_seq(&ui.pending)),
            ),
            _ => {}
        }
    }

    frame.render_widget(statusline::tabs(&ui.theme, ui.view), tabs);
    match &ui.prompt {
        Some(prompt) => {
            frame.render_widget(statusline::prompt_line(&ui.theme, prompt), status);
            // Park the cursor after the typed text, so the line looks like the
            // input it is.
            frame.set_cursor_position((
                status.x + 1 + prompt.buffer.chars().count() as u16,
                status.y,
            ));
        }
        None => frame.render_widget(statusline::status(ui, ctx), status),
    }

    // A rule-owned list says whose rule it is, so "why can I not delete this
    // row" has an answer on screen rather than only in the refusal message.
    let playlist_title = match &ui.open {
        Some(o) => o.name().to_string(),
        None => "nothing open".to_string(),
    };

    match ui.view {
        View::Tree => draw_tree(frame, body, ui, ctx),
        View::Sorted => draw_tracks(
            frame,
            body,
            ui,
            ctx,
            TrackList {
                scroll: &ui.sorted_scroll,
                ids: ui.library.sorted(),
                selected: ui.sorted,
                title: "sorted",
                focused: true,
                divide: true,
                section: None,
            },
        ),
        View::Playlist => {
            // Pick on the left, see it on the right — view 1's shape, because
            // it is the same idea and `tab` already means this.
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
                    .areas(body);
            draw_playlists(frame, left, ui);
            draw_tracks(
                frame,
                right,
                ui,
                ctx,
                TrackList {
                    scroll: &ui.playlist_scroll,
                    ids: ui.playlist.entries(),
                    selected: ui.playlist_sel,
                    title: &playlist_title,
                    focused: ui.playlist_pane == Pane::Tracks,
                    divide: false,
                    section: None,
                },
            );
        }
        View::Queue => {
            let rows = up_next(ui, ctx);
            let entries: Vec<_> = rows.iter().map(|(id, _)| *id).collect();
            let queued = rows
                .iter()
                .take_while(|(_, from)| *from == crate::next::Upcoming::Queued)
                .count();
            draw_tracks(
                frame,
                body,
                ui,
                ctx,
                TrackList {
                    scroll: &ui.queue_scroll,
                    ids: &entries,
                    selected: ui.queue_sel,
                    title: "up next",
                    focused: true,
                    divide: false,
                    // Drawn above the first row the queue does not own, so the
                    // line between "you can move this" and "this is what would
                    // happen" is shown rather than described.
                    section: (queued < entries.len())
                        .then(|| (queued, "the pool, from here".to_string())),
                },
            )
        }
        View::Browser => draw_browser(frame, body, ui),
        View::Filters => draw_catalogs(frame, body, ui),
        View::Guide => draw_guides(frame, body, ui),
    }

    // **Last, and over the middle of the view.** #34 put this in the bottom
    // slot, where it read as one more transient helper beside the completion
    // menu — and a mode you do not notice is a mode you press navigation keys
    // in, which is exactly what happened in use.
    //
    // Floating over the centre is the difference between a panel that *reports*
    // the mode and one that *is* it. It covers the list on purpose: in transport
    // mode the list is not what your hands are doing.
    if ui.mode == crate::cmd::Mode::Transport {
        draw_transport(frame, body, ui, ctx);
    }
}

/// Rows the completion menu needs, borders included. Zero when there is
/// nothing to show.
/// How tall a popup of `rows` needs to be, borders included and capped.
/// The keys that could follow what has been typed so far, or `None` when
/// nothing is pending.
///
/// which-key, without a timeout: a chord here is always deliberate — no key is
/// both a command and a prefix, because `resolve` fires the shorter one — so
/// there is nothing to wait to find out. A fast `gg` flashes the panel for one
/// frame, which is what which-key looks like with `timeoutlen` at zero.
fn which_key_rows(ui: &Ui, now: Instant) -> Option<Vec<String>> {
    if ui.pending.is_empty() {
        return None;
    }
    // The wait is only for the popup nobody asked for. `<leader>?` sets
    // `ui.help` and takes a different path, because a panel you requested by
    // name should not make you wait for it — which is the same split which-key
    // makes between its prefix popup and its marks/registers plugins
    // (`config.lua:13`, `ctx.plugin and 0 or 200`).
    if !which_key_due(ui.pending_since, now, ui.which_key_delay) {
        return None;
    }
    let rows: Vec<String> = bind::continuations(&ui.bindings, ui.context(), &ui.pending)
        .into_iter()
        .map(|(key, next)| match next {
            bind::Next::Runs(cmd) => format!("{key}  {}", crate::cmd::parse::format(cmd)),
            // which-key's `+prefix`, and the `+` is why it is not mistaken for
            // a command name.
            bind::Next::More(n) => format!("{key}  +{n} more"),
        })
        .collect();
    (!rows.is_empty()).then_some(rows)
}

fn rows_height(rows: &[String], width: u16) -> u16 {
    if rows.is_empty() {
        return 3;
    }
    // The same cap the completion menu uses, and for the same reason: a
    // popup must not swallow the view it is helping you use.
    (completion_rows(rows, width).len() as u16 + 2).min(10)
}

/// The keys that work here.
///
/// Laid out in the same columns as the completion menu, from the same helper —
/// two popups in one slot that disagreed about their shape would read as two
/// different things.
fn draw_help(frame: &mut Frame, area: Rect, ui: &Ui, rows: &[String], title: &str) {
    let laid_out = completion_rows(rows, area.width);
    let column = completion_column(rows);
    let available = (area.height.saturating_sub(2)) as usize;
    let overflows = laid_out.len() > available;
    // One row goes to the footer when there is one, so the count it carries is
    // exact rather than off by the row it is written on.
    let shown = if overflows {
        available.saturating_sub(1)
    } else {
        available
    };
    // Clamped here rather than where the key is handled: only this function
    // knows how many rows the layout produced at this width, and the width can
    // change under a popup that is already open.
    let first = ui.help_scroll.min(laid_out.len().saturating_sub(shown));
    ui.help_view.set((shown, laid_out.len()));

    let text = ui.theme.get(theme::Role::StatusText);
    let mut lines: Vec<Line> = laid_out
        .iter()
        .skip(first)
        .take(shown)
        .map(|row| {
            let mut spans = Vec::with_capacity(row.len() * 2);
            for (n, &i) in row.iter().enumerate() {
                spans.push(Span::styled(rows[i].clone(), text));
                if n + 1 < row.len() {
                    spans.push(Span::raw(
                        " ".repeat(column.saturating_sub(rows[i].chars().count())),
                    ));
                }
            }
            Line::from(spans)
        })
        .collect();
    // The affordance appears only when it is true, which is which-key's rule
    // (`view.lua:448`, guarded on the content being taller than the window). A
    // popup that fits advertises nothing, and the hint does not become
    // furniture you stop reading.
    if overflows {
        let below = laid_out.len() - shown - first;
        let position = if below > 0 {
            format!("… {below} more")
        } else {
            "… end".to_string()
        };
        lines.push(Line::from(Span::styled(
            format!("{position}   ^d ^u scroll"),
            ui.theme.get(theme::Role::StatusDim),
        )));
    }

    let title = title.to_string();
    frame.render_widget(
        Paragraph::new(lines).block(pane_block(&ui.theme, &title, false)),
        area,
    );
}

/// How tall the transport panel is. Fixed, because it always says the same
/// four things and a panel that resized as a track's tags changed would be
/// worse than one that is simply this tall.
const TRANSPORT_PANEL: u16 = 6;
/// And how wide. Wide enough for a long artist — title line, narrow enough
/// that the list is visibly still there around it.
const TRANSPORT_WIDTH: u16 = 64;

/// A rect of `width` by `height` centred in `area`, or `None` if it would not
/// leave the view visible around it.
///
/// The margin is what makes this read as floating *over* something rather than
/// as a replacement for it — if the panel would fill the pane, showing nothing
/// is better than showing a panel that looks like the whole screen.
fn centred(area: Rect, width: u16, height: u16) -> Option<Rect> {
    const MARGIN: u16 = 2;
    if area.width < width + MARGIN || area.height < height + MARGIN {
        return None;
    }
    Some(Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    })
}

/// The panel that rises with the mode (#34).
///
/// **Scoped to the mode, which is what keeps it the right side of §1.** A
/// permanent now-playing panel is the "something to watch" that has been
/// refused twice; this is not there at all while you are browsing, and appears
/// only once you have said — by entering the mode — that playback is what you
/// are attending to. A panel you cannot dismiss is clutter; a panel that *is*
/// the thing you switched to is the screen doing what you asked.
fn draw_transport(frame: &mut Frame, body: Rect, ui: &Ui, ctx: &Context<'_>) {
    let Some(area) = centred(body, TRANSPORT_WIDTH, TRANSPORT_PANEL) else {
        // No room to float without covering the whole view. The status line
        // still says `-- TRANSPORT --`, so the mode is never invisible.
        return;
    };
    // Blank what is underneath, or the list shows through the panel's gaps.
    frame.render_widget(ratatui::widgets::Clear, area);
    let theme = &ui.theme;
    let text = theme.get(theme::Role::StatusText);
    let dim = theme.get(theme::Role::StatusDim);
    let mut lines: Vec<Line> = Vec::new();

    let state = ctx
        .player
        .map(|p| p.state())
        .unwrap_or(crate::audio::State::Stopped);
    let position = ctx.player.map(|p| p.position()).unwrap_or(Duration::ZERO);
    let track = ui.playback.current.and_then(|id| ctx.store.get(id));

    match track {
        Some(track) => {
            lines.push(Line::from(vec![
                // The same three glyphs the status line uses. `▶` is
                // single-width everywhere; `⏸` and `⏹` are in a range many
                // terminals draw double-width or not at all, so `■` stands in
                // for stop and the row cannot be shifted by a mis-measured
                // glyph.
                Span::styled(
                    match state {
                        crate::audio::State::Playing => "▶  ",
                        crate::audio::State::Paused => "⏸  ",
                        crate::audio::State::Stopped => "■  ",
                    },
                    theme.get(theme::Role::StatusPlaying),
                ),
                Span::styled(ui.transport_line.render(track), text),
            ]));
            lines.push(Line::from(Span::styled(
                progress_bar(position, track.duration, area.width.saturating_sub(2)),
                theme.get(theme::Role::StatusPlaying),
            )));
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{} / {}",
                        list::fmt_duration(position),
                        list::fmt_duration(track.duration)
                    ),
                    dim,
                ),
                Span::styled(
                    match ctx.player {
                        Some(p) => format!("   vol {:.0}%", p.volume() * 100.0),
                        None => String::new(),
                    },
                    dim,
                ),
            ]));
        }
        None => {
            lines.push(Line::from(Span::styled("nothing playing", dim)));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "x plays what the cursor is on",
                dim,
            )));
        }
    }

    // What is next is worth a line precisely because shuffle makes it
    // unguessable, and `next()` is pure so asking costs nothing.
    let next = crate::next::next_with_source(next_input(ui))
        .and_then(|(id, _)| ctx.store.get(id))
        .map(|t| format!("next  {}", ui.now_playing.render(t)))
        .unwrap_or_else(|| "next  —".to_string());
    lines.push(Line::from(Span::styled(next, dim)));

    // **Not `pane_block`.** A pane border says "this is one of the panes"; the
    // whole complaint was that the panel read as furniture rather than as the
    // mode. It takes the mode's own role — the same one the status line's
    // `-- TRANSPORT --` uses — and says so in its title, so the thing on screen
    // and the thing in the status line are visibly one fact.
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Double)
                .title(" -- TRANSPORT -- ")
                .title_style(theme.get(theme::Role::Mode))
                .border_style(theme.get(theme::Role::Mode)),
        ),
        area,
    );
}

/// A position bar drawn from block characters rather than with a gauge widget,
/// so it takes a theme role like everything else and cannot introduce a colour
/// of its own.
fn progress_bar(position: Duration, total: Duration, width: u16) -> String {
    let width = width.max(1) as usize;
    let fraction = if total.is_zero() {
        0.0
    } else {
        (position.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0)
    };
    let filled = (fraction * width as f64).round() as usize;
    let mut out = String::with_capacity(width * 3);
    for i in 0..width {
        out.push(if i < filled { '━' } else { '─' });
    }
    out
}

fn menu_height(prompt: &Prompt, width: u16) -> u16 {
    let Some(candidates) = prompt.menu.as_deref() else {
        return 0;
    };
    // Open with nothing matching: one row for the notice, plus borders.
    if candidates.is_empty() {
        return 3;
    }
    let rows = completion_rows(prompt.candidates(), width).len() as u16;
    // Capped, so a 63-verb list cannot swallow the view it is helping you
    // navigate.
    (rows + 2).min(10)
}

/// Width of one column, candidate plus its gutter.
fn completion_column(candidates: &[String]) -> usize {
    candidates
        .iter()
        .map(|c| c.chars().count())
        .max()
        .unwrap_or(1)
        + 2
}

/// Lay candidates out in columns, as a shell does — a single column of 63
/// verbs is unreadable and does not fit.
///
/// Returns *indices* rather than formatted text, because one candidate in a
/// row has to be styled differently from its neighbours and a pre-formatted
/// row is a single opaque string. It also makes the overflow count exact:
/// counting words in a rendered row miscounts any candidate containing a
/// space, which catalog names can.
fn completion_rows(candidates: &[String], width: u16) -> Vec<Vec<usize>> {
    let column = completion_column(candidates);
    let usable = width.saturating_sub(4).max(1) as usize;
    let columns = (usable / column).max(1);
    let rows = candidates.len().div_ceil(columns);

    (0..rows)
        .map(|r| {
            (0..columns)
                .map(|c| c * rows + r)
                .filter(|i| *i < candidates.len())
                .collect()
        })
        .collect()
}

fn draw_completions(frame: &mut Frame, area: Rect, ui: &Ui, prompt: &Prompt) {
    // Open with nothing matching. Saying so is the whole point of keeping the
    // menu up: a list that vanished would read as a swallowed keystroke.
    if prompt.candidates().is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no matches",
                ui.theme.get(theme::Role::StatusDim),
            )))
            .block(pane_block(&ui.theme, "completions", false)),
            area,
        );
        return;
    }
    let rows = completion_rows(prompt.candidates(), area.width);
    let column = completion_column(prompt.candidates());
    let available = (area.height.saturating_sub(2)) as usize;
    // Reserve a row for the overflow notice, or it is drawn outside the block
    // and silently clipped.
    let shown = if rows.len() > available {
        available.saturating_sub(1)
    } else {
        available
    };

    // Scroll so the selection is on screen. Cycling past the last visible row
    // would otherwise move a selection nobody can see.
    let first = if shown > 0 {
        let row = rows
            .iter()
            .position(|r| r.contains(&prompt.selected))
            .unwrap_or(0);
        row.saturating_sub(shown - 1)
            .min(rows.len() - shown.min(rows.len()))
    } else {
        0
    };

    let text = ui.theme.get(theme::Role::StatusText);
    let picked = ui.theme.get(theme::Role::CompletionSelected);
    let mut lines: Vec<Line> = rows
        .iter()
        .skip(first)
        .take(shown)
        .map(|row| {
            let mut spans = Vec::with_capacity(row.len() * 2);
            for (n, &i) in row.iter().enumerate() {
                let item = &prompt.candidates()[i];
                let style = if prompt.selected == i { picked } else { text };
                spans.push(Span::styled(item.clone(), style));
                // Pad between columns, not after the last one, so a selection
                // at the end of a row does not trail a highlighted block.
                if n + 1 < row.len() {
                    let pad = column.saturating_sub(item.chars().count());
                    spans.push(Span::raw(" ".repeat(pad)));
                }
            }
            Line::from(spans)
        })
        .collect();
    let hidden = prompt.candidates().len()
        - rows
            .iter()
            .skip(first)
            .take(shown)
            .map(Vec::len)
            .sum::<usize>();
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            format!("… {hidden} more"),
            ui.theme.get(theme::Role::StatusDim),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(pane_block(
            &ui.theme,
            &format!("{} completions", prompt.candidates().len()),
            true,
        )),
        area,
    );
}

fn draw_tree(frame: &mut Frame, area: Rect, ui: &Ui, ctx: &Context<'_>) {
    if ui.library.is_empty() {
        let dim = ui.theme.get(theme::Role::StatusDim);
        let lines = if ui.catalogs.defined().is_empty() {
            vec![
                Line::from(""),
                Line::from(Span::styled("  no library yet", dim)),
                Line::from(Span::styled(
                    "  start gmuse with --library <dir>, or set library = [\"~/Music\"] in your config",
                    dim,
                )),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from(Span::styled("  nothing in the active catalogs", dim)),
                Line::from(Span::styled("  :catalog all  shows everything", dim)),
            ]
        };
        frame.render_widget(
            Paragraph::new(lines).block(pane_block(&ui.theme, "library", true)),
            area,
        );
        return;
    }

    let rows = ui.tree.rows(&ui.library, ctx.store);
    // Artists, albums and tracks in one list. A track row is rendered through
    // the format template rather than labelled, because it has to look
    // different from the artist row above it — which is what makes a single
    // pane readable at all.
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut spans: Vec<Span> = Vec::new();
            if ui.relative_numbers {
                spans.push(Span::styled(
                    list::gutter(i, ui.tree.selected),
                    ui.theme.get(theme::Role::Gutter),
                ));
            }
            match row {
                tree::Row::Track { id, .. } => {
                    let style = if ui.playback.current == Some(*id) {
                        ui.theme.get(theme::Role::Playing)
                    } else {
                        Style::default()
                    };
                    spans.push(Span::raw(row.label()));
                    let text = ctx
                        .store
                        .get(*id)
                        .map(|t| ui.track_line.render(t))
                        .unwrap_or_default();
                    spans.push(Span::styled(text, style));
                }
                _ => spans.push(Span::raw(row.label())),
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut items = items;
    let mut state =
        ui.tree_scroll
            .state_within(Some(ui.tree.selected), rows.len(), view_height(area));
    pad_to_view(&mut items, &ui.theme, area, state.offset());
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&ui.theme, "library", true))
            .highlight_style(highlight(&ui.theme, true)),
        area,
        &mut state,
    );
    ui.tree_scroll.remember(&state);
}

/// What to draw a track list as.
/// Where a list is scrolled to, remembered between frames.
///
/// A fresh `ListState` starts at offset zero, and ratatui then scrolls the
/// least it can to bring the selection into view — which lands the selection
/// on the **last** visible row whenever it is past the first page. That reads
/// fine going down and wrong coming back: after `G`, `k` drags the whole view
/// up a line at a time with the cursor stuck to the bottom, instead of walking
/// the cursor up the page it is already on.
///
/// A `Cell` for the same reason `guide_view` is one — only `draw` knows the
/// pane's height, and nothing outside a frame does.
#[derive(Debug, Default)]
pub struct Scroll {
    top: std::cell::Cell<usize>,
    /// The cursor's **row**, and the pane's visible height, as of the last
    /// frame. `zz`/`zt`/`zb` need both as numbers, and only `draw` has them:
    /// the height because it depends on the pane, and the row because album
    /// dividers make it differ from the track index every other caller uses.
    row: std::cell::Cell<usize>,
    height: std::cell::Cell<usize>,
    /// A `zz` owed to the next frame, from a motion that moved the cursor and
    /// wants to recentre after it — `^d` and `^u`.
    ///
    /// Deferred rather than applied at once, because `row` above is the
    /// cursor's **screen row** as of the last frame, and a track list's rows
    /// and its track indices differ by however many album dividers are above
    /// the cursor. Recentring immediately would centre where the cursor *was*,
    /// and be wrong by a growing amount the further down the list you went.
    centre_next: std::cell::Cell<bool>,
}

impl Scroll {
    /// Called after rendering, with the state ratatui just adjusted.
    fn remember(&self, state: &ListState) {
        self.top.set(state.offset());
    }

    /// The first visible row, as of the last frame.
    fn top(&self) -> usize {
        self.top.get()
    }

    /// The state for this frame, with the remembered offset clamped so the
    /// pane can show filler *below* the list but never only filler.
    fn state_within(&self, selected: Option<usize>, len: usize, height: usize) -> ListState {
        self.row.set(selected.unwrap_or(0));
        self.height.set(height);
        // Now that the row is this frame's, honour any deferred `zz`.
        if self.centre_next.replace(false) {
            self.place(crate::cmd::Place::Centre);
        }
        let top = self.top.get().min(len.saturating_sub(1));
        self.top.set(top);
        ListState::default()
            .with_selected(selected)
            .with_offset(top)
    }

    /// Put the cursor's row at the top, the middle or the bottom of the pane —
    /// vim's `zt`, `zz` and `zb`.
    ///
    /// Measured from the last frame. Before the first one both numbers are
    /// zero, and every branch below then asks for row zero — which is where it
    /// already is, so an undrawn pane needs no guard of its own.
    ///
    /// `~` filler is what lets `zt` work on the last row rather than being
    /// quietly ignored.
    fn place(&self, place: crate::cmd::Place) {
        let (row, height) = (self.row.get(), self.height.get());
        self.top.set(match place {
            crate::cmd::Place::Top => row,
            crate::cmd::Place::Centre => row.saturating_sub(height / 2),
            crate::cmd::Place::Bottom => row.saturating_sub(height.saturating_sub(1)),
        });
    }

    /// Scroll so `row` is the first visible one. ratatui clamps at render, so
    /// this can ask for more than the list has.
    fn scroll_to(&self, row: usize) {
        self.top.set(row);
    }
}

/// A pane's visible height: its rows, less the border above and below.
fn view_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

/// vim's `~`: the rows past the end of the list.
///
/// Without them a list can only scroll until its last row sits on the bottom
/// line, so the cursor's screen position depends on how many rows happen to be
/// *below* it — collapse a fold near the end and the cursor drops to the last
/// line however carefully the fold code held it. Padding out to the viewport
/// makes the padding a property of the screen rather than of the list, which
/// is the whole reason vim draws `~` at all.
fn pad_to_view(items: &mut Vec<ListItem<'static>>, theme: &theme::Theme, area: Rect, top: usize) {
    let height = view_height(area);
    let filler = theme.get(theme::Role::Filler);
    while items.len() < top + height {
        items.push(ListItem::new(Line::from(Span::styled("~", filler))));
    }
}

struct TrackList<'a> {
    /// Where this list was scrolled to last frame. Threaded through rather
    /// than looked up, because one function draws view 2, view 3's right pane
    /// and the queue, and they scroll independently.
    scroll: &'a Scroll,
    ids: &'a [crate::store::TrackId],
    /// A **track** index, not a screen row.
    selected: usize,
    title: &'a str,
    focused: bool,
    /// Divide by album. Off for the playlist and queue: they are deliberate
    /// orderings, and dividing an interleaved one puts a header on nearly
    /// every row.
    divide: bool,
    /// A divider to draw above a given **track** index, for a list whose
    /// sections are not albums. View 4 uses one to say where the queue stops
    /// and the projection begins, which is the difference between a row you can
    /// move and a row you can only read.
    section: Option<(usize, String)>,
}

fn draw_tracks(frame: &mut Frame, area: Rect, ui: &Ui, ctx: &Context<'_>, spec: TrackList<'_>) {
    if spec.ids.is_empty() {
        frame.render_widget(
            Paragraph::new(empty_hint(&ui.theme, spec.title))
                .block(pane_block(&ui.theme, spec.title, true)),
            area,
        );
        return;
    }
    draw_track_list(frame, area, ui, ctx, spec);
}

/// Draw a track list, dividing it by album where that helps.
///
/// `selected` is a **track** index. Album dividers occupy a screen row but no
/// index, so this is the one place the two coordinate systems meet — every
/// other caller counts tracks.
fn draw_track_list(frame: &mut Frame, area: Rect, ui: &Ui, ctx: &Context<'_>, spec: TrackList<'_>) {
    let mut rows = if spec.divide && ui.album_headers {
        list::rows(ctx.store, spec.ids)
    } else {
        spec.ids.iter().copied().map(list::Row::Track).collect()
    };
    if let Some((at, ref text)) = spec.section {
        // Counted in tracks, then converted — the two coordinate systems meet
        // here, as they do for `selected` just below.
        let mut seen = 0usize;
        let screen_row = rows
            .iter()
            .position(|r| {
                matches!(r, list::Row::Track(_)) && {
                    let hit = seen == at;
                    seen += 1;
                    hit
                }
            })
            .unwrap_or(rows.len());
        rows.insert(screen_row, list::Row::Header(text.clone()));
    }
    let items = list::items(
        &ui.theme,
        ctx.store,
        &rows,
        list::RowStyle {
            relative: ui.relative_numbers,
            selected: spec.selected,
            playing: ui.playback.current,
            needle: ui.highlight.as_ref(),
        },
    );
    let mut items = items;
    let mut state = spec.scroll.state_within(
        list::row_of_track(&rows, spec.selected).or(Some(0)),
        rows.len(),
        view_height(area),
    );
    pad_to_view(&mut items, &ui.theme, area, state.offset());
    // A selection landing on the playing track keeps the playing colour, or
    // the cursor hides the one thing you are watching for. Only while focused:
    // an unfocused pane should still read as unfocused.
    let on_playing = ui.playback.current.is_some()
        && spec.ids.get(spec.selected).copied() == ui.playback.current;
    let selection = if on_playing && spec.focused {
        ui.theme.get(theme::Role::SelectionPlaying)
    } else {
        highlight(&ui.theme, spec.focused)
    };

    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&ui.theme, spec.title, spec.focused))
            .highlight_style(selection),
        area,
        &mut state,
    );
    spec.scroll.remember(&state);
}

fn draw_browser(frame: &mut Frame, area: Rect, ui: &Ui) {
    let items: Vec<ListItem> = ui
        .browser
        .entries
        .iter()
        .map(|p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("..")
                .to_string();
            let label = if p.is_dir() && name != ".." {
                format!("{name}/")
            } else {
                name
            };
            ListItem::new(Line::from(label))
        })
        .collect();
    let mut items = items;
    let mut state =
        ui.browser_scroll
            .state_within(Some(ui.browser.selected), items.len(), view_height(area));
    pad_to_view(&mut items, &ui.theme, area, state.offset());
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(
                &ui.theme,
                &ui.browser.cwd.display().to_string(),
                true,
            ))
            .highlight_style(highlight(&ui.theme, true)),
        area,
        &mut state,
    );
    ui.browser_scroll.remember(&state);
}

fn empty_hint(theme: &theme::Theme, title: &str) -> Vec<Line<'static>> {
    let dim = theme.get(theme::Role::StatusDim);
    vec![
        Line::from(""),
        Line::from(Span::styled(format!("  the {title} is empty"), dim)),
        Line::from(Span::styled(
            "  add to it with `a`, `y` or `e` from the library",
            dim,
        )),
    ]
}

fn pane_block<'a>(theme: &theme::Theme, title: &'a str, focused: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(theme.get(if focused {
            theme::Role::BorderFocused
        } else {
            theme::Role::BorderUnfocused
        }))
}

/// The unfocused pane keeps a dimmer highlight rather than losing it, so you
/// can still see where you left the cursor after `Tab`.
fn highlight(theme: &theme::Theme, focused: bool) -> Style {
    theme.get(if focused {
        theme::Role::SelectionFocused
    } else {
        theme::Role::SelectionUnfocused
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Track;
    use ratatui::backend::TestBackend;
    use std::time::{Duration, Instant};

    /// A library with the three things §9 says the tree must get right: a
    /// compilation, a multi-disc album, and a track with no tags at all.
    /// A library long enough that a page motion has somewhere to go, on one
    /// album so the flat list has no dividers to complicate the arithmetic.
    fn big_fixture(n: u32) -> Store {
        let mut store = Store::new();
        for i in 1..=n {
            store.upsert(Track {
                path: PathBuf::from(format!("/m/long/{i:04}.mp3")),
                artist: Some("One Artist".into()),
                albumartist: Some("One Artist".into()),
                album: Some("One Album".into()),
                title: Some(format!("Track {i:04}")),
                tracknumber: Some(i),
                duration: Duration::from_secs(200),
                ..Default::default()
            });
        }
        store
    }

    fn fixture() -> Store {
        let mut store = Store::new();

        // A normal album, with a guest credit on one track — the case that
        // would split the album if we filed by `artist`.
        for (n, title, artist) in [
            (1u32, "The Bird", "Anderson .Paak"),
            (3, "The Waters", "Anderson Paak, BJ The Chicago Kid"),
        ] {
            store.upsert(Track {
                path: PathBuf::from(format!("/m/paak/{n}.mp3")),
                artist: Some(artist.into()),
                albumartist: Some("Anderson .Paak".into()),
                album: Some("Malibu".into()),
                title: Some(title.into()),
                tracknumber: Some(n),
                duration: Duration::from_secs(180 + n as u64),
                ..Default::default()
            });
        }

        // A compilation: three different artists, one albumartist.
        for (n, title, artist) in [
            (1u32, "The Mountain", "Benjamin Tod"),
            (2, "Years", "Sierra Ferrell"),
        ] {
            store.upsert(Track {
                path: PathBuf::from(format!("/m/va/{n}.opus")),
                artist: Some(artist.into()),
                albumartist: Some("Various Artists".into()),
                album: Some("YouTube Sessions".into()),
                title: Some(title.into()),
                tracknumber: Some(n),
                duration: Duration::from_secs(200),
                ..Default::default()
            });
        }

        // A multi-disc album, inserted with disc 2 first.
        for (disc, n, title) in [
            (2u32, 1u32, "Second Disc Opener"),
            (1, 1, "First Disc Opener"),
        ] {
            store.upsert(Track {
                path: PathBuf::from(format!("/m/multi/d{disc}t{n}.flac")),
                artist: Some("The Wall Builders".into()),
                album: Some("Long Player".into()),
                title: Some(title.into()),
                discnumber: Some(disc),
                tracknumber: Some(n),
                duration: Duration::from_secs(240),
                ..Default::default()
            });
        }

        // No tags whatsoever.
        store.upsert(Track {
            path: PathBuf::from("/m/orphan/unlabelled.mp3"),
            duration: Duration::from_secs(65),
            ..Default::default()
        });

        store
    }

    fn render(ui: &Ui, ctx: &Context<'_>, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, ui, ctx)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn tree_view_renders_artists_collapsed() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
            remote: None,
        };
        let ui = ui_of(&store, true);
        let out = render(&ui, &ctx, 74, 12);

        // Smart sort files "The Wall Builders" under W, so it comes last.
        assert!(out.contains("▸ Anderson .Paak"), "{out}");
        assert!(out.contains("▸ Unknown Artist"), "{out}");
        assert!(out.contains("▸ Various Artists"), "{out}");
        assert!(out.contains("▸ The Wall Builders"), "{out}");
        let paak = out.find("Anderson .Paak").unwrap();
        let wall = out.find("The Wall Builders").unwrap();
        let unknown = out.find("Unknown Artist").unwrap();
        assert!(paak < unknown && unknown < wall, "sort order wrong:\n{out}");
        assert!(out.contains("1:library"), "{out}");
        assert!(out.contains("7 tracks"), "{out}");
    }

    #[test]
    fn compilation_stays_one_artist_with_every_track() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
            remote: None,
        };
        let mut ui = ui_of(&store, true);
        // Select "Various Artists" and expand it.
        let rows = ui.tree.rows(&ui.library, &store);
        ui.tree.selected = rows
            .iter()
            .position(|r| r.label().contains("Various Artists"))
            .unwrap();
        expand_to_tracks(&mut ui, &store);

        let out = render(&ui, &ctx, 74, 12);
        assert!(out.contains("▾ Various Artists"), "{out}");
        assert!(out.contains("YouTube Sessions"), "{out}");
        // Both tracks show, despite having different `artist` tags.
        assert!(out.contains("The Mountain"), "{out}");
        assert!(out.contains("Years"), "{out}");
    }

    #[test]
    fn multi_disc_album_does_not_interleave() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
            remote: None,
        };
        let mut ui = ui_of(&store, true);
        let rows = ui.tree.rows(&ui.library, &store);
        ui.tree.selected = rows
            .iter()
            .position(|r| r.label().contains("Wall Builders"))
            .unwrap();
        expand_to_tracks(&mut ui, &store);

        let out = render(&ui, &ctx, 90, 16);
        let first = out.find("First Disc Opener").expect(&out);
        let second = out.find("Second Disc Opener").expect(&out);
        assert!(first < second, "disc 2 sorted before disc 1:\n{out}");
    }

    #[test]
    fn untagged_track_shows_its_file_name() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
            remote: None,
        };
        let mut ui = ui_of(&store, true);
        let rows = ui.tree.rows(&ui.library, &store);
        ui.tree.selected = rows
            .iter()
            .position(|r| r.label().contains("Unknown Artist"))
            .unwrap();
        expand_to_tracks(&mut ui, &store);

        let out = render(&ui, &ctx, 90, 14);
        assert!(out.contains("unlabelled.mp3"), "{out}");
        assert!(out.contains("1:05"), "duration missing:\n{out}");
    }

    #[test]
    fn empty_playlist_says_so_rather_than_rendering_blank() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
            remote: None,
        };
        let mut ui = ui_of(&store, true);
        ui.view = View::Playlist;
        let out = render(&ui, &ctx, 90, 10);
        assert!(out.contains("3:playlist"), "{out}");
        // Both halves say what is going on rather than rendering blank.
        assert!(out.contains("nothing open"), "{out}");
        assert!(out.contains("no playlists"), "{out}");
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
            remote: None,
        };
        let mut ui = ui_of(&store, true);
        let rows = ui.tree.rows(&ui.library, &store).len();

        for _ in 0..rows * 2 {
            handle_key(KeyEvent::from(KeyCode::Char('j')), &mut ui, &ctx);
        }
        assert_eq!(ui.tree.selected, rows - 1, "ran off the bottom");

        for _ in 0..rows * 2 {
            handle_key(KeyEvent::from(KeyCode::Char('k')), &mut ui, &ctx);
        }
        assert_eq!(ui.tree.selected, 0, "ran off the top");
    }

    // ---- playback wiring (M5) -------------------------------------------
    //
    // `ctx.player` is None here: these cover the decisions the UI makes, which
    // is where the bugs are. Whether cpal then makes a sound is M1/M2's ground.

    fn ctx_of(store: &Store) -> Context<'_> {
        Context {
            store,
            player: None,
            remote: None,
        }
    }

    /// Open the artist under the cursor and its first album, so the tracks
    /// are on screen. Tracks live inline now, two folds down.
    fn expand_to_tracks(ui: &mut Ui, store: &Store) {
        let lib = std::mem::take(&mut ui.library);
        ui.tree.toggle(&lib, store);
        ui.tree.selected += 1;
        ui.tree.toggle(&lib, store);
        ui.library = lib;
    }

    /// A `Ui` over the whole fixture library.
    fn ui_of(store: &Store, smart: bool) -> Ui {
        Ui::new(PathBuf::from("/m"), Library::rebuild(store, smart))
    }

    #[test]
    fn x_plays_the_track_row_under_the_cursor() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        expand_to_tracks(&mut ui, &store);

        // Down onto the second track of the open album.
        press(&mut ui, &ctx, "j");
        press(&mut ui, &ctx, "j");
        let expected = ui.tree.tracks_for(&ui.library, &store)[0];
        press(&mut ui, &ctx, "<leader> x");

        let played = ui.playback.current.expect("nothing started playing");
        assert_eq!(played, expected);
    }

    #[test]
    fn enter_confirms_opening_a_node_and_playing_a_track() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let closed = ui.tree.rows(&ui.library, &store).len();

        // On an artist, Enter opens it — there is nothing to play there.
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
        assert!(ui.tree.rows(&ui.library, &store).len() > closed);
        assert!(ui.playback.current.is_none(), "Enter played an artist");

        // Down to the album, open it, down to the first track.
        press(&mut ui, &ctx, "j");
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
        assert!(ui.playback.current.is_none(), "Enter played an album");
        press(&mut ui, &ctx, "j");

        // At a track there is nothing left to open, so Enter plays it —
        // snacks' rule, and the reason `Enter` and `x` are not redundant.
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
        let rows = ui.tree.rows(&ui.library, &store);
        assert!(
            ui.playback.current.is_some(),
            "Enter did not play the track; rows: {:?}",
            rows.iter().map(|r| r.label()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn leader_b_and_leader_z_walk_forwards_and_backwards() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "<leader> x");
        let first = ui.playback.current.unwrap();

        press(&mut ui, &ctx, "<leader> b");
        let second = ui.playback.current.unwrap();
        assert_ne!(first, second);

        press(&mut ui, &ctx, "<leader> z");
        assert_eq!(
            ui.playback.current,
            Some(first),
            "<leader>z should come back"
        );
    }

    #[test]
    fn mode_keys_toggle_and_show_in_the_status_line() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        assert!(!ui.playback.shuffle);
        handle_key(KeyEvent::from(KeyCode::Char('s')), &mut ui, &ctx);
        assert!(ui.playback.shuffle);
        handle_key(KeyEvent::from(KeyCode::Char('r')), &mut ui, &ctx);
        assert!(ui.playback.repeat);
        handle_key(KeyEvent::from(KeyCode::Char('m')), &mut ui, &ctx);
        assert_eq!(ui.playback.aaa_mode, crate::next::AaaMode::Artist);

        let out = render(&ui, &ctx, 90, 8);
        assert!(out.contains("[artist]"), "{out}");
        assert!(out.contains("[shuffle]"), "{out}");
        assert!(out.contains("[repeat]"), "{out}");
    }

    /// The queue outranks the library, and playing from it consumes it — the
    /// §5 rule that most often goes wrong in a UI.
    #[test]
    fn finishing_a_track_takes_from_the_queue_and_pops_it() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        let sorted: Vec<_> = ui.library.sorted().to_vec();
        ui.playback.current = Some(sorted[0]);
        let queued = sorted[4];
        ui.queue.push_back(queued);

        advance(&mut ui, &ctx, true);

        assert_eq!(ui.playback.current, Some(queued));
        assert!(ui.queue.is_empty(), "the queue was not consumed");

        // With the queue drained, playback resumes after the library cursor —
        // which the queued track must not have moved.
        advance(&mut ui, &ctx, true);
        assert_eq!(ui.playback.current, Some(sorted[5]));
    }

    #[test]
    fn running_off_the_end_stops_rather_than_looping() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let last = *ui.library.sorted().last().unwrap();
        ui.playback.current = Some(last);

        advance(&mut ui, &ctx, true);
        assert_eq!(ui.playback.current, None);
        assert!(ui.status.contains("end of pool"), "{}", ui.status);

        // With repeat on, the same position wraps instead.
        ui.playback.current = Some(last);
        ui.playback.repeat = true;
        advance(&mut ui, &ctx, true);
        assert_eq!(ui.playback.current, Some(ui.library.sorted()[0]));
    }

    #[test]
    fn playing_from_the_playlist_view_makes_it_the_active_source() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let sorted: Vec<_> = ui.library.sorted().to_vec();
        ui.playlist.push(sorted[3]);
        ui.playlist.push(sorted[1]);

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "<leader> x");
        assert_eq!(ui.playback.active, ActiveSource::Playlist);
        assert_eq!(ui.playback.current, Some(sorted[3]));

        // `next` now walks the playlist, not the library.
        advance(&mut ui, &ctx, true);
        assert_eq!(ui.playback.current, Some(sorted[1]));
    }

    #[test]
    fn now_playing_replaces_the_library_summary() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let out = render(&ui, &ctx, 90, 8);
        assert!(out.contains("7 tracks"), "{out}");

        ui.playback.current = Some(ui.library.sorted()[0]);
        let out = render(&ui, &ctx, 90, 8);
        assert!(!out.contains("7 tracks"), "summary should give way:\n{out}");
        assert!(out.contains("Anderson .Paak"), "{out}");
    }
    #[test]
    fn a_transient_notice_expires_instead_of_sticking() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        ui.notify("audio output changed");
        assert!(render(&ui, &ctx, 90, 8).contains("audio output changed"));

        // Pretend the deadline passed.
        ui.status_until = Some(Instant::now() - Duration::from_secs(1));
        ui.expire_status();
        let out = render(&ui, &ctx, 90, 8);
        assert!(!out.contains("audio output changed"), "{out}");
        // The library summary comes back once the notice clears.
        assert!(out.contains("7 tracks"), "{out}");
    }

    #[test]
    fn a_device_reroute_is_not_reported_as_an_error() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.notify("audio output changed");
        let out = render(&ui, &ctx, 90, 8);
        // Switching headphones in must not look like a failure.
        assert!(!out.contains("error"), "{out}");
        assert!(!out.contains("device lost"), "{out}");
    }
    /// `v` then `c` must not lose the title: the decoder keeps the track
    /// loaded across a stop, so the UI has to as well or the two desync.
    #[test]
    fn stop_keeps_the_current_track() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "<leader> x");
        let playing = ui.playback.current.expect("nothing started");
        assert!(render(&ui, &ctx, 90, 8).contains("Anderson .Paak"));

        press(&mut ui, &ctx, "<leader> v");
        assert_eq!(
            ui.playback.current,
            Some(playing),
            "stop must leave the track current, or resuming loses the title"
        );

        // Still named in the status line while stopped.
        let out = render(&ui, &ctx, 90, 8);
        assert!(out.contains("Anderson .Paak"), "{out}");

        // And next still steps from where we stopped, rather than restarting.
        press(&mut ui, &ctx, "<leader> b");
        assert_ne!(ui.playback.current, Some(playing));
    }
    // ---- the command layer (M6) ------------------------------------------

    use crate::cmd::keys;
    use ratatui::crossterm::event::KeyModifiers;

    /// Feed a key sequence, e.g. `"j"`, `"g g"`, `"^f"`.
    /// Backdate the pending chord past its delay, so a test about what the
    /// popup *says* is not also a test of when it appears.
    ///
    /// Deliberately not "set the delay to zero in `ui_of`": that would make
    /// every render test run in a configuration no user has, and the delay
    /// would then be exercised by nothing that draws.
    fn waited(ui: &mut Ui) {
        ui.pending_since = ui
            .pending_since
            .map(|t| t - ui.which_key_delay - Duration::from_millis(1));
    }

    fn press(ui: &mut Ui, ctx: &Context<'_>, keys_text: &str) {
        let seq = keys::parse_seq(keys_text)
            .unwrap_or_else(|e| panic!("bad test key {keys_text:?}: {e}"));
        // `<leader>` is a sentinel until it is resolved, exactly as it is when
        // a binding is inserted — a test that pressed the placeholder would be
        // pressing a key no keyboard has.
        let seq = keys::resolve_leader(&seq, ui.leader);
        for k in seq {
            let mut mods = KeyModifiers::NONE;
            if k.ctrl {
                mods |= KeyModifiers::CONTROL;
            }
            if k.alt {
                mods |= KeyModifiers::ALT;
            }
            handle_key(KeyEvent::new(k.code, mods), ui, ctx);
        }
    }

    fn type_line(ui: &mut Ui, ctx: &Context<'_>, line: &str) {
        for c in line.chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), ui, ctx);
        }
        handle_key(KeyEvent::from(KeyCode::Enter), ui, ctx);
    }

    /// A `Ui` with two smart playlists defined and a history to test against.
    fn with_playlists(store: &Store) -> Ui {
        let mut ui = ui_of(store, true);
        ui.playlists.define(
            "electronic",
            crate::filter::Filter::parse("genre=\"electronic\"").unwrap(),
            Some(crate::sort::Sort::parse("title").unwrap()),
        );
        ui.playlists.define(
            "good",
            crate::filter::Filter::parse("rating>=4").unwrap(),
            None,
        );
        ui.events_path = Some(crate::events::log_path(&scratch("smart-playlists"), "test"));
        ui
    }

    /// A store where genre is set, so a rule has something to select on.
    fn genre_fixture() -> Store {
        let mut s = Store::new();
        for (path, title, genre) in [
            ("/m/a.mp3", "Zebra", "electronic"),
            ("/m/b.mp3", "Antelope", "electronic"),
            ("/m/c.mp3", "Marmot", "folk"),
        ] {
            s.upsert(Track {
                path: PathBuf::from(path),
                artist: Some("Someone".into()),
                album: Some("An Album".into()),
                title: Some(title.into()),
                genre: Some(genre.into()),
                duration: Duration::from_secs(200),
                ..Default::default()
            });
        }
        s
    }

    #[test]
    fn loading_a_smart_playlist_fills_view_three_in_its_order() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open electronic");

        assert_eq!(ui.view, View::Playlist);
        assert_eq!(ui.open.as_ref().map(Open::name), Some("electronic"));
        let titles: Vec<&str> = ui
            .playlist
            .entries()
            .iter()
            .map(|id| store.get(*id).unwrap().title.as_deref().unwrap())
            .collect();
        assert_eq!(titles, ["Antelope", "Zebra"], "sorted by title");
    }

    /// The design decision this whole feature turns on: a smart playlist is a
    /// **source**, not a narrowing. Loading one must leave the library exactly
    /// as it was — otherwise it is a catalog wearing another name.
    #[test]
    fn loading_a_smart_playlist_does_not_narrow_the_library() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);
        let before = ui.library.sorted().to_vec();

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open electronic");

        assert_eq!(ui.playlist.len(), 2, "the rule selected two");
        assert_eq!(
            ui.library.sorted(),
            before,
            "the library must be untouched — narrowing is what :catalog does"
        );
    }

    #[test]
    fn an_unknown_smart_playlist_lists_the_ones_there_are() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open nope");
        assert!(ui.status.contains("electronic"), "{}", ui.status);
        assert!(ui.status.contains("good"), "{}", ui.status);
        assert!(ui.open.is_none());
    }

    #[test]
    fn a_rule_matching_nothing_says_what_the_rule_was() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open good");
        assert!(ui.playlist.is_empty());
        assert!(ui.status.contains("rating>=4"), "{}", ui.status);
    }

    /// Editing is refused while a rule owns the list, and the refusal names
    /// the way out rather than only saying no.
    #[test]
    fn a_rule_owned_playlist_refuses_edits_and_names_freeze() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open electronic");
        let before = ui.playlist.len();

        execute(&mut ui, &ctx, Command::Remove);
        assert_eq!(ui.playlist.len(), before, "a rule-owned entry was removed");
        assert!(ui.status.contains(":freeze"), "{}", ui.status);

        ui.view = View::Sorted;
        execute(
            &mut ui,
            &ctx,
            Command::Add {
                target: crate::cmd::AddTarget::Playlist,
                path: None,
            },
        );
        assert_eq!(ui.playlist.len(), before, "a track was added to a rule");
        assert!(ui.status.contains(":freeze"), "{}", ui.status);
    }

    #[test]
    fn freezing_keeps_the_tracks_and_makes_them_editable() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open electronic");
        let before = ui.playlist.len();

        execute(&mut ui, &ctx, Command::Freeze);
        assert!(ui.open.is_none());
        assert_eq!(ui.playlist.len(), before, "freezing must keep the tracks");

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        execute(&mut ui, &ctx, Command::Remove);
        assert_eq!(ui.playlist.len(), before - 1, "still not editable");
    }

    #[test]
    fn freezing_an_ordinary_playlist_says_so() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);
        execute(&mut ui, &ctx, Command::Freeze);
        assert!(ui.status.contains("only a rule"), "{}", ui.status);
    }

    /// A live query, not a snapshot: rating a track has to change what a
    /// rating-based rule answers, without anyone reloading it by hand.
    #[test]
    fn rating_a_track_re_evaluates_the_loaded_rule() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open good");
        assert!(ui.playlist.is_empty(), "nothing is rated yet");

        // Rate the track under the cursor in view 2.
        ui.view = View::Sorted;
        ui.sorted = 0;
        execute(&mut ui, &ctx, Command::Rate(crate::cmd::RateSpec::Set(5)));

        assert_eq!(ui.playlist.len(), 1, "the rule did not notice the rating");
        assert_eq!(ui.open.as_ref().map(Open::name), Some("good"));
    }

    #[test]
    fn view_three_names_the_rule_it_is_showing() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open electronic");
        // The right pane is titled by what is open, and the left marks it.
        let out = render(&ui, &ctx, 90, 10);
        assert!(out.contains("electronic"), "{out}");

        execute(&mut ui, &ctx, Command::Freeze);
        let out = render(&ui, &ctx, 90, 10);
        assert!(out.contains("nothing open"), "{out}");
    }

    /// The defect #22 opened with — a playlist lost on quit while the
    /// throwaway queue survived. Now it comes back by **name**: the tracks
    /// live in a file, so a copy in the session could only go stale.
    #[test]
    fn the_open_playlist_comes_back_by_name() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "session-open");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist new roadtrip");
        ui.view = View::Sorted;
        execute(
            &mut ui,
            &ctx,
            Command::Add {
                target: crate::cmd::AddTarget::Playlist,
                path: None,
            },
        );
        assert_eq!(ui.playlist.len(), 1, "{}", ui.status);

        let snapshot = capture_session(&ui, &ctx);
        assert_eq!(snapshot.open_playlist.as_deref(), Some("roadtrip"));

        let mut fresh = with_playlists(&store);
        fresh.saved = ui.saved.clone();
        restore_session(&mut fresh, &ctx, &snapshot, &Default::default());
        assert_eq!(
            fresh.open.as_ref().map(Open::name),
            Some("roadtrip"),
            "the playlist did not reopen"
        );
        assert_eq!(fresh.playlist.len(), 1, "its tracks did not come back");
    }

    /// You land where you left. Reopening a playlist switches to view 3, so
    /// without this every launch arrived there whatever you had been on.
    #[test]
    fn the_view_comes_back_and_a_reopened_playlist_does_not_override_it() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "session-view");
        new_playlist(&mut ui, &ctx, "mix");
        assert_eq!(ui.view, View::Playlist, "`new` should open it");

        // Wander off to a different view and quit from there.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 7");
        let snapshot = capture_session(&ui, &ctx);
        assert_eq!(snapshot.view, Some(7));
        assert_eq!(snapshot.open_playlist.as_deref(), Some("mix"));

        let mut fresh = with_playlists(&store);
        fresh.saved = ui.saved.clone();
        restore_session(&mut fresh, &ctx, &snapshot, &Default::default());

        assert_eq!(fresh.view, View::Guide, "it landed on the playlist view");
        // And the playlist still came back — the view restore is on top of it,
        // not instead of it.
        assert_eq!(fresh.open.as_ref().map(Open::name), Some("mix"));
    }

    /// A session from before this existed has no view; it must not land on
    /// view 1 by surprise, it must simply not move.
    #[test]
    fn a_session_without_a_view_leaves_it_alone() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "session-no-view");
        let mut snapshot = capture_session(&ui, &ctx);
        snapshot.view = None;

        ui.view = View::Queue;
        restore_session(&mut ui, &ctx, &snapshot, &Default::default());
        assert_eq!(ui.view, View::Queue);
    }

    /// A rule is remembered by name too, and **recomputed** on open — which is
    /// the point of storing a name rather than a list.
    #[test]
    fn a_rule_is_remembered_by_name_and_recomputed() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "session-rule");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open electronic");
        let snapshot = capture_session(&ui, &ctx);
        assert_eq!(snapshot.open_playlist.as_deref(), Some("electronic"));

        let mut fresh = with_playlists(&store);
        fresh.saved = ui.saved.clone();
        restore_session(&mut fresh, &ctx, &snapshot, &Default::default());
        assert!(fresh.open.as_ref().is_some_and(Open::is_rule));
        assert_eq!(fresh.playlist.len(), 2, "the rule was not re-evaluated");
    }

    fn ui_with_saved(store: &Store, name: &str) -> (Ui, PathBuf) {
        let dir = scratch(name);
        let mut ui = with_playlists(store);
        ui.saved = Some(crate::playlist::Playlists::new(&dir));
        (ui, dir)
    }

    #[test]
    fn a_saved_playlist_round_trips_through_a_file() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "save-open");
        let ids: Vec<_> = store.iter().map(|(id, _)| id).collect();
        ui.playlist.push(ids[2]);
        ui.playlist.push(ids[0]);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist save roadtrip");
        assert!(
            dir.join("playlists/roadtrip.m3u8").exists(),
            "{}",
            ui.status
        );

        ui.playlist.clear();
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open roadtrip");
        assert_eq!(ui.playlist.entries(), [ids[2], ids[0]]);
        assert!(
            !ui.open.as_ref().unwrap().is_rule(),
            "a saved playlist is editable"
        );
        assert_eq!(ui.view, View::Playlist);
    }

    /// Saving is also how a rule becomes editable, so it must detach — leaving
    /// the title claiming a rule still owns the list would be a lie.
    #[test]
    fn saving_a_rule_owned_playlist_detaches_it() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "save-detach");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open electronic");
        let before = ui.playlist.len();

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist save keepers");
        // The copy is now what is open, and it is editable.
        assert_eq!(ui.open.as_ref().map(Open::name), Some("keepers"));
        assert!(!ui.open.as_ref().unwrap().is_rule());
        assert_eq!(ui.playlist.len(), before);

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        execute(&mut ui, &ctx, Command::Remove);
        assert_eq!(ui.playlist.len(), before - 1, "still not editable");
    }

    /// A playlist naming music the library does not have is short, and says so
    /// — quietly dropping tracks is how you stop trusting it.
    #[test]
    fn tracks_a_playlist_names_but_the_library_lacks_are_reported() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "missing");
        let known = store.iter().next().unwrap().1.path.display().to_string();
        std::fs::create_dir_all(dir.join("playlists")).unwrap();
        std::fs::write(
            dir.join("playlists/partial.m3u8"),
            format!(
                "#EXTM3U
{known}
/m/gone.mp3
"
            ),
        )
        .unwrap();

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open partial");
        assert_eq!(ui.playlist.len(), 1);
        assert!(ui.status.contains("1 not in the library"), "{}", ui.status);
    }

    /// One vocabulary: `open` takes a rule or a file without caring which.
    #[test]
    fn listing_shows_rules_and_saved_files_together() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "listing");
        std::fs::create_dir_all(dir.join("playlists")).unwrap();
        std::fs::write(
            dir.join("playlists/roadtrip.m3u8"),
            "#EXTM3U
",
        )
        .unwrap();

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist list");
        for expected in ["electronic", "good", "roadtrip"] {
            assert!(
                ui.status.contains(expected),
                "{expected} missing: {}",
                ui.status
            );
        }
    }

    #[test]
    fn saving_an_empty_playlist_says_so_rather_than_writing_a_stub() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "empty-save");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist save nothing");
        assert!(ui.status.contains("empty"), "{}", ui.status);
        assert!(!dir.join("playlists/nothing.m3u8").exists());
    }

    /// #45: this used to be a hardcoded 10, whatever the pane's height.
    ///
    /// Two different heights, because a test against a pane that happens to be
    /// ten rows tall cannot tell the old behaviour from the new one — the
    /// coincident-answers failure this project has now paid for three times.
    #[test]
    fn a_page_is_the_pane_it_is_in() {
        let store = big_fixture(200);
        let ctx = ctx_of(&store);

        // A terminal loses the tab strip and the status line, then the pane
        // loses two border rows, then vim's page keeps two rows of overlap.
        let page_of = |rows: u16| rows as usize - 2 - 2 - 2;

        for rows in [24u16, 44] {
            let expect = page_of(rows);
            let mut ui = ui_of(&store, true);
            ui.view = View::Sorted;
            // A frame, so the body height is recorded.
            let _ = render(&ui, &ctx, 100, rows);
            press(&mut ui, &ctx, "^f");
            assert_eq!(
                ui.sorted, expect,
                "a page in a {rows}-row terminal should be {expect}"
            );
        }
    }

    /// Half of it, and vim's two-row page overlap does not apply — half a page
    /// already leaves half the screen in common.
    #[test]
    fn a_half_page_is_half_the_pane() {
        let store = big_fixture(200);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        let _ = render(&ui, &ctx, 100, 24);

        press(&mut ui, &ctx, "^d");
        assert_eq!(ui.sorted, 10, "half of a 20-row pane");
        press(&mut ui, &ctx, "^u");
        assert_eq!(ui.sorted, 0, "and back");
    }

    /// The recentre is owed to the *next* frame, not applied at once.
    ///
    /// `Scroll::row` is the cursor's **screen row** as of the last frame, and
    /// a track list's rows and its track indices differ by however many album
    /// dividers sit above the cursor. Centring immediately would centre where
    /// the cursor was, wrongly and by a growing amount — so this asserts the
    /// cursor ends up near the middle of the pane after the frame, which is
    /// the property, rather than asserting on an offset.
    #[test]
    fn a_half_page_leaves_the_cursor_centred() {
        let store = big_fixture(200);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        let _ = render(&ui, &ctx, 100, 24);

        for _ in 0..6 {
            press(&mut ui, &ctx, "^d");
            let _ = render(&ui, &ctx, 100, 24);
        }

        let scroll = focused_scroll(&ui).expect("view 2 has one");
        let screen_row = scroll.row.get() - scroll.top();
        let height = scroll.height.get();
        assert!(
            screen_row.abs_diff(height / 2) <= 1,
            "cursor sat {screen_row} rows into a {height}-row pane"
        );
    }

    /// The pane the report came from. It has no `Scroll` at all — its cursor
    /// *is* the scroll offset — so a half-page motion has to work through
    /// `body_height` rather than through `focused_scroll`, and there is
    /// nothing to recentre.
    #[test]
    fn half_page_scrolls_the_guide_body() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Guide;
        ui.guide_pane = Pane::Tracks;
        let _ = render(&ui, &ctx, 100, 24);

        assert_eq!(ui.guide_scroll, 0);
        press(&mut ui, &ctx, "^d");
        assert_eq!(ui.guide_scroll, 10, "the guide body did not scroll");
        press(&mut ui, &ctx, "^u");
        assert_eq!(ui.guide_scroll, 0);
    }

    /// The tree numbers every row, because an artist and an album are both
    /// jumpable — unlike a track list, which skips its album dividers.
    #[test]
    fn relative_numbers_reach_the_artist_pane() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        // The *tree* pane's own cell, not the track pane beside it — which
        // has a gutter of its own and would satisfy a looser assertion.
        let tree_cell = |ui: &Ui, want: &str| -> String {
            render(ui, &ctx, 100, 12)
                .lines()
                .find(|l| l.contains(want))
                .and_then(|l| l.split('\u{2502}').nth(1).map(str::to_string))
                .unwrap_or_default()
        };

        assert!(
            !tree_cell(&ui, "Anderson .Paak").starts_with(|c: char| c.is_ascii_digit()),
            "a gutter appeared with the setting off"
        );

        ui.relative_numbers = true;
        // The cursor is on row 0, so it shows its *absolute* number — vim's
        // hybrid, and the number a count for `G` takes.
        let cursor = tree_cell(&ui, "Anderson .Paak");
        assert!(cursor.starts_with("1   "), "{cursor:?}");

        // The next artist is one row away, so it shows the distance instead,
        // right-aligned against the absolute.
        let next = tree_cell(&ui, "Unknown Artist");
        assert!(next.starts_with("  1 "), "{next:?}");

        // Dimmed, like the track pane's, so the numbers sit behind the names.
        assert_eq!(
            gutter_colour(&ui, &ctx, "Unknown Artist"),
            ui.theme.get(theme::Role::Gutter).fg,
            "the tree gutter is not drawn in the gutter role"
        );
    }

    /// The colour of the gutter on the tree row naming `want`.
    ///
    /// Read off the rendered buffer, because a role the code passes but the
    /// screen does not show is not a colour. Deliberately not the *cursor*
    /// row: ratatui's row highlight covers the whole line there, including the
    /// gutter, exactly as it does in the track pane.
    fn gutter_colour(ui: &Ui, ctx: &Context<'_>, want: &str) -> Option<ratatui::style::Color> {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        terminal.draw(|f| draw(f, ui, ctx)).unwrap();
        let buf = terminal.backend().buffer().clone();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            // The gutter is the first cells inside the pane's left border.
            if line.contains(want) {
                return Some(buf[(4, y)].fg);
            }
        }
        None
    }

    #[test]
    fn the_left_pane_lists_rules_and_saved_files() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "view8");
        std::fs::create_dir_all(dir.join("playlists")).unwrap();
        std::fs::write(
            dir.join("playlists/roadtrip.m3u8"),
            "#EXTM3U\n/m/a.mp3\n/m/b.mp3\n",
        )
        .unwrap();

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 3");
        assert_eq!(ui.view, View::Playlist);

        let out = render(&ui, &ctx, 110, 12);
        assert!(out.contains("3:playlist"), "{out}");
        // Rules are marked rather than hidden: opening one gives a list you
        // cannot edit, which is worth knowing before pressing enter.
        assert!(out.contains("electronic"), "{out}");
        assert!(out.contains("rule"), "{out}");
        assert!(out.contains("roadtrip"), "{out}");
    }

    /// `ALL` drives `gt`/`gT` and the tab strip, so a view missing from it is
    /// unreachable and invisible while still being a legal `:view <n>` target.
    /// Its order must match the ids, which is what lets the strip be numbered
    /// at all.
    #[test]
    fn every_view_is_in_all_exactly_once_and_in_id_order() {
        let ids: Vec<u8> = View::ALL.iter().map(|v| *v as u8).collect();
        assert_eq!(
            ids,
            (1..=View::ALL.len() as u8).collect::<Vec<_>>(),
            "ALL is missing a view, has one twice, or is out of id order"
        );
        for n in 1..=View::ALL.len() as u8 {
            let id = crate::cmd::ViewId::from_number(n).expect("a legal :view target");
            assert!(
                View::ALL.contains(&View::from_id(id)),
                "view {n} unreachable"
            );
        }
    }

    /// The tab strip numbers from each view's own id, which is what `3gt` and
    /// `:view 3` mean. Equivalent to numbering by position only while `ALL`
    /// stays in id order — which the test above is what actually guarantees.
    #[test]
    fn the_tab_strip_numbers_match_the_view_ids() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let ui = ui_of(&store, true);
        let out = render(&ui, &ctx, 110, 6);
        for view in View::ALL {
            let expected = format!("{}:{}", *view as u8, view.title());
            assert!(out.contains(&expected), "{expected} missing from: {out}");
        }
    }

    #[test]
    fn enter_in_the_left_pane_opens_that_playlist() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "view8-enter");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 3");
        // Rules sort first, so the cursor starts on `electronic`.
        execute(&mut ui, &ctx, Command::Activate);

        assert_eq!(ui.view, View::Playlist, "the browser is a place to leave");
        assert_eq!(ui.open.as_ref().map(Open::name), Some("electronic"));
        assert_eq!(ui.playlist.len(), 2);
    }

    /// Saving is the one thing that changes the set from inside the app, so
    /// the browser has to notice without being left and re-entered.
    #[test]
    fn saving_a_playlist_shows_up_in_the_left_pane() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "view8-refresh");
        ui.playlist.push(store.iter().next().unwrap().0);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 3");
        assert!(!ui.playlist_rows.iter().any(|r| r.name == "keepers"));

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist save keepers");
        assert!(
            ui.playlist_rows.iter().any(|r| r.name == "keepers"),
            "the browser did not notice the save"
        );
    }

    #[test]
    fn an_empty_left_pane_says_where_playlists_would_go() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let dir = scratch("view8-empty");
        ui.saved = Some(crate::playlist::Playlists::new(&dir));

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 3");
        let out = render(&ui, &ctx, 100, 8);
        assert!(out.contains("no playlists yet"), "{out}");
        assert!(out.contains("playlists"), "{out}");
    }

    /// A file that cannot be read still gets a row — it exists, and hiding it
    /// would be stranger than saying so.
    #[test]
    fn an_unreadable_playlist_is_listed_rather_than_hidden() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "view8-unreadable");
        std::fs::create_dir_all(dir.join("playlists/broken.m3u8")).unwrap();

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 3");
        let row = ui
            .playlist_rows
            .iter()
            .find(|r| r.name == "broken")
            .expect("not listed");
        assert_eq!(row.detail, "unreadable");
    }

    /// The two panes are view 1's shape, so `tab` and `pane-left`/`right`
    /// work here without any new keys.
    /// vim's `dd`, including its count. `D` was cmus's and meant this, which
    /// sat oddly in a keymap whose navigation is otherwise nvim's.
    #[test]
    fn dd_removes_one_and_takes_a_count() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "dd");
        new_playlist(&mut ui, &ctx, "mix");
        for (id, _) in store.iter() {
            ui.playlist.push(id);
        }
        let all = ui.playlist.len();
        assert!(all >= 3, "the fixture needs three tracks");

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "d d");
        assert_eq!(ui.playlist.len(), all - 1);

        press(&mut ui, &ctx, "2 d d");
        assert_eq!(ui.playlist.len(), all - 3, "the count was ignored");
    }

    /// `D` is delete-to-end. There is no "inside a line" here, so the list is
    /// what has an end — which makes the analogy hold rather than rhyme.
    #[test]
    fn capital_d_removes_to_the_end_of_the_list() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "capital-d");
        new_playlist(&mut ui, &ctx, "mix");
        for (id, _) in store.iter() {
            ui.playlist.push(id);
        }
        assert!(ui.playlist.len() >= 3, "the fixture needs three tracks");

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        ui.playlist_sel = 1;
        press(&mut ui, &ctx, "D");

        assert_eq!(ui.playlist.len(), 1, "everything from the cursor should go");
        // Written through, like any other edit.
        let at = dir.join("playlists/mix.m3u8");
        assert_eq!(crate::m3u::read(&at).unwrap().len(), 1);
    }

    /// A count that runs off the end takes what is there and stops, rather
    /// than wrapping or panicking.
    #[test]
    fn removing_past_the_end_stops_there() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "dd-overrun");
        new_playlist(&mut ui, &ctx, "mix");
        ui.playlist.push(store.iter().next().unwrap().0);

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "9 d d");
        assert!(ui.playlist.is_empty());
    }

    /// The leader's whole purpose: `?` stays reverse search, as in vim, and
    /// help lives behind a prefix instead of taking a key it should not.
    #[test]
    fn leader_question_mark_shows_the_keys_that_work_here() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "space");
        assert!(ui.help.is_none(), "the leader alone must do nothing yet");
        press(&mut ui, &ctx, "?");
        let rows = ui.help.as_ref().expect("no help popup");

        // The view's own bindings and common's, which is what is reachable.
        assert!(rows.iter().any(|r| r.contains("quit")), "{rows:?}");
        assert!(rows.iter().any(|r| r.starts_with("j ")), "{rows:?}");

        // It is drawn, not merely stored.
        let out = render(&ui, &ctx, 90, 20);
        assert!(out.contains("keys that work here"), "{out}");
    }

    /// "What can I do *here*" — so another view's bindings must not appear.
    /// The Index guide's flat list of every binding answers the other question.
    #[test]
    fn help_leaves_out_another_views_bindings() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 4");

        press(&mut ui, &ctx, "space");
        press(&mut ui, &ctx, "?");
        let rows = ui.help.as_ref().expect("no help popup");

        assert!(rows.iter().any(|r| r.contains("quit")), "common is missing");
        assert!(
            rows.iter().any(|r| r.starts_with("l ")),
            "the common bindings are missing: {rows:?}"
        );
    }

    /// `?` on its own is still reverse search — the reason for a leader.
    #[test]
    fn question_mark_alone_is_still_reverse_search() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "?");
        assert!(ui.help.is_none());
        assert!(ui.prompt.is_some(), "the search line did not open");
    }

    /// Any key closes it, and is swallowed doing so — reading the popup and
    /// pressing something must not differ from reading it and pressing nothing.
    #[test]
    fn any_key_dismisses_the_help_without_acting() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let before = ui.view;

        press(&mut ui, &ctx, "space");
        press(&mut ui, &ctx, "?");
        assert!(ui.help.is_some());

        press(&mut ui, &ctx, "g");
        assert!(ui.help.is_none(), "the popup stayed up");
        press(&mut ui, &ctx, "t");
        assert_eq!(ui.view, before, "the dismissing key also did something");
    }

    /// Space is the leader, so `expand` moved to `o` — a chord cannot extend a
    /// key that is bound on its own.
    #[test]
    fn o_expands_and_space_no_longer_does() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let before = ui.tree.rows(&ui.library, &store).len();

        press(&mut ui, &ctx, "space");
        assert_eq!(
            ui.tree.rows(&ui.library, &store).len(),
            before,
            "space still expands, so it cannot be a leader"
        );
        press(&mut ui, &ctx, "escape");

        press(&mut ui, &ctx, "o");
        assert!(ui.tree.rows(&ui.library, &store).len() > before);
    }

    /// `<leader>` is substituted where the configuration lives, so a binding
    /// written with it lands on the configured key.
    #[test]
    fn a_binding_written_with_leader_resolves_to_the_configured_key() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.leader = keys::Key::new(KeyCode::Char(','));

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind common \"<leader> x\" quit");
        press(&mut ui, &ctx, ", x");
        assert!(ui.quit, "the comma leader did not fire");
    }

    /// The whole point of a register over a nudge key: vim's registers are
    /// global, so yanking here and putting there needs nothing special.
    #[test]
    fn yank_crosses_playlists() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "yank-cross");
        new_playlist(&mut ui, &ctx, "one");
        ui.view = View::Sorted;
        press(&mut ui, &ctx, "a");
        assert_eq!(ui.playlist.len(), 1);

        // Copy it out of the first playlist.
        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "y y");
        assert_eq!(ui.register.len(), 1);

        // Open another and put it there. The track stays in the first: `yy`
        // copies, `dd` moves.
        new_playlist(&mut ui, &ctx, "two");
        assert!(ui.playlist.is_empty());
        press(&mut ui, &ctx, "p");
        assert_eq!(ui.playlist.len(), 1, "{}", ui.status);

        open_playlist(&mut ui, &ctx, "one");
        assert_eq!(ui.playlist.len(), 1, "`yy` removed it from the source");
    }

    /// vim's swap idiom, which is why no separate nudge key is needed: `p`
    /// puts *after* the cursor, so `dd` then `p` exchanges two rows.
    #[test]
    fn dd_then_p_swaps_a_track_with_the_next() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "swap");
        new_playlist(&mut ui, &ctx, "mix");
        for (id, _) in store.iter() {
            ui.playlist.push(id);
        }
        let before = ui.playlist.entries().to_vec();
        assert!(before.len() >= 3);

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "d d");
        press(&mut ui, &ctx, "p");

        assert_eq!(
            ui.playlist.entries(),
            [before[1], before[0], before[2]],
            "the first two did not swap"
        );
    }

    /// `P` puts before the cursor, `p` after — vim's distinction.
    #[test]
    fn capital_p_puts_before_the_cursor() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "put-before");
        new_playlist(&mut ui, &ctx, "mix");
        let ids: Vec<_> = store.iter().map(|(id, _)| id).collect();
        ui.playlist.push(ids[0]);
        ui.playlist.push(ids[1]);

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        ui.playlist_sel = 1;
        ui.register = vec![ids[2]];

        press(&mut ui, &ctx, "P");
        assert_eq!(ui.playlist.entries(), [ids[0], ids[2], ids[1]]);
    }

    /// Counts compose on both halves, as in vim.
    #[test]
    fn counts_work_on_yank_and_put() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "counts");
        new_playlist(&mut ui, &ctx, "mix");
        for (id, _) in store.iter() {
            ui.playlist.push(id);
        }
        let all = ui.playlist.len();

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "2 y y");
        assert_eq!(ui.register.len(), 2, "the count was ignored");

        press(&mut ui, &ctx, "2 p");
        assert_eq!(ui.playlist.len(), all + 4, "`2p` should put it twice");
    }

    /// An edit is an edit: putting writes the file like `y` and `dd` do.
    #[test]
    fn putting_is_written_through() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "put-write");
        new_playlist(&mut ui, &ctx, "mix");
        ui.register = vec![store.iter().next().unwrap().0];

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "p");

        let at = dir.join("playlists/mix.m3u8");
        assert_eq!(crate::m3u::read(&at).unwrap().len(), 1, "{}", ui.status);
    }

    #[test]
    fn putting_an_empty_register_says_so() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "put-empty");
        new_playlist(&mut ui, &ctx, "mix");
        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;

        press(&mut ui, &ctx, "p");
        assert!(ui.status.contains("register is empty"), "{}", ui.status);
    }

    /// A rule has no tracks of its own to put into.
    #[test]
    fn putting_into_a_rule_is_refused() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "put-rule");
        open_playlist(&mut ui, &ctx, "electronic");
        ui.register = vec![store.iter().next().unwrap().0];

        press(&mut ui, &ctx, "p");
        assert!(ui.status.contains(":freeze"), "{}", ui.status);
    }

    /// The case that asked for this: an accidental `p`, taken back.
    #[test]
    fn u_takes_back_a_put_and_a_cut() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "undo-basic");
        new_playlist(&mut ui, &ctx, "mix");
        for (id, _) in store.iter() {
            ui.playlist.push(id);
        }
        let original = ui.playlist.entries().to_vec();
        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;

        press(&mut ui, &ctx, "y y");
        press(&mut ui, &ctx, "p");
        assert_eq!(ui.playlist.len(), original.len() + 1);

        press(&mut ui, &ctx, "u");
        assert_eq!(ui.playlist.entries(), original, "the put was not undone");

        press(&mut ui, &ctx, "d d");
        assert_eq!(ui.playlist.len(), original.len() - 1);
        press(&mut ui, &ctx, "u");
        assert_eq!(ui.playlist.entries(), original, "the cut was not undone");

        // An undo is an edit like any other: the file follows.
        let at = dir.join("playlists/mix.m3u8");
        assert_eq!(crate::m3u::read(&at).unwrap().len(), original.len());
    }

    #[test]
    fn ctrl_r_redoes_what_u_took_back() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "undo-redo");
        new_playlist(&mut ui, &ctx, "mix");
        ui.playlist.push(store.iter().next().unwrap().0);
        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;

        press(&mut ui, &ctx, "d d");
        assert!(ui.playlist.is_empty());
        press(&mut ui, &ctx, "u");
        assert_eq!(ui.playlist.len(), 1);
        press(&mut ui, &ctx, "^r");
        assert!(ui.playlist.is_empty(), "redo did not reapply the cut");
    }

    /// vim's undo is per buffer, and a playlist is a buffer: undoing in one
    /// must not reach into another you are no longer looking at.
    #[test]
    fn history_is_per_playlist() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "undo-per-list");
        let ids: Vec<_> = store.iter().map(|(id, _)| id).collect();

        new_playlist(&mut ui, &ctx, "one");
        ui.view = View::Sorted;
        press(&mut ui, &ctx, "a");
        assert_eq!(ui.playlist.len(), 1);

        // A second playlist, never edited: `u` there has nothing to do.
        new_playlist(&mut ui, &ctx, "two");
        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "u");
        assert!(ui.status.contains("oldest"), "{}", ui.status);
        assert!(ui.playlist.is_empty(), "it undid the other playlist");

        // Back in the first, the step is still there — as a vim buffer keeps
        // its history across being left and returned to.
        let _ = ids;
        open_playlist(&mut ui, &ctx, "one");
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "u");
        assert!(ui.playlist.is_empty(), "the add was not undone");
    }

    /// The queue is edited by hand too, so it gets the same protection.
    #[test]
    fn the_queue_has_its_own_undo() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "undo-queue");
        ui.view = View::Sorted;
        press(&mut ui, &ctx, "e");
        let before = ui.queue.len();
        assert!(before > 0);

        ui.view = View::Queue;
        press(&mut ui, &ctx, "d d");
        assert_eq!(ui.queue.len(), before - 1);
        press(&mut ui, &ctx, "u");
        assert_eq!(ui.queue.len(), before);
    }

    /// A rule cannot be edited, so it has nothing to undo — and `u` says that
    /// rather than appearing to do nothing.
    /// Saving over a playlist replaces its contents, so its history stops
    /// describing anything — `u` must not restore a list that name never held.
    #[test]
    fn saving_over_a_playlist_forgets_its_history() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "undo-overwrite");
        new_playlist(&mut ui, &ctx, "mix");
        ui.view = View::Sorted;
        press(&mut ui, &ctx, "a");
        assert!(!ui.playlist.is_empty());

        // Overwrite it with something else entirely.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist save mix");

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "u");
        assert!(
            ui.status.contains("oldest"),
            "stale history survived: {}",
            ui.status
        );
    }

    #[test]
    fn a_rule_has_no_undo_history() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "undo-rule");
        open_playlist(&mut ui, &ctx, "electronic");
        ui.playlist_pane = Pane::Tracks;

        press(&mut ui, &ctx, "u");
        assert!(ui.status.contains("undo history"), "{}", ui.status);
    }

    /// Enter is for structure; the transport row is for sound. Two ways to
    /// start music would be two mechanisms for one idea (§9).
    #[test]
    fn enter_never_plays() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "enter-structure");
        new_playlist(&mut ui, &ctx, "mix");
        ui.view = View::Sorted;
        press(&mut ui, &ctx, "a");

        for (view, pane) in [
            (View::Sorted, Pane::Tree),
            (View::Playlist, Pane::Tracks),
            (View::Queue, Pane::Tree),
        ] {
            ui.view = view;
            ui.playlist_pane = pane;
            handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
            assert!(
                ui.playback.current.is_none(),
                "Enter started playing in {view:?}"
            );
        }
    }

    /// `x` used to mean "resume if playing, else play the selection" — two
    /// behaviours, the first of which is what `c` already does.
    #[test]
    fn x_plays_the_selection_even_while_something_is_playing() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "tab");

        press(&mut ui, &ctx, "<leader> x");
        let first = ui.playback.current.expect("nothing started");
        press(&mut ui, &ctx, "j");
        press(&mut ui, &ctx, "<leader> x");

        assert_ne!(
            ui.playback.current,
            Some(first),
            "`x` resumed instead of playing what the cursor was on"
        );
    }

    /// The queue is consumed as it plays, so playing from it takes the entry.
    #[test]
    fn x_in_the_queue_consumes_the_entry() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        press(&mut ui, &ctx, "e");
        let queued = ui.queue.len();
        assert!(queued > 0);

        ui.view = View::Queue;
        press(&mut ui, &ctx, "<leader> x");
        assert_eq!(ui.queue.len(), queued - 1, "the entry was not consumed");
    }

    /// vim's `zR` and `zM`, which replaced snacks' `Z` and an alt-prefixed
    /// expand-all that nobody would have found.
    #[test]
    fn z_r_opens_every_fold_and_z_m_closes_them() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let closed = ui.tree.rows(&ui.library, &store).len();

        press(&mut ui, &ctx, "z R");
        assert!(ui.tree.rows(&ui.library, &store).len() > closed);
        press(&mut ui, &ctx, "z M");
        assert_eq!(ui.tree.rows(&ui.library, &store).len(), closed);
    }

    /// The graduated version, which is the reason the level exists at all.
    #[test]
    fn z_r_lowercase_opens_one_level_at_a_time() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let artists = ui.tree.rows(&ui.library, &store).len();

        press(&mut ui, &ctx, "z r");
        let albums = ui.tree.rows(&ui.library, &store).len();
        press(&mut ui, &ctx, "z r");
        let tracks = ui.tree.rows(&ui.library, &store).len();
        assert!(
            artists < albums && albums < tracks,
            "zr should add a level each time, got {artists} then {albums} then {tracks}"
        );

        press(&mut ui, &ctx, "z m");
        assert_eq!(ui.tree.rows(&ui.library, &store).len(), albums);
    }

    /// `<leader>?` says what a key does; the guides say how to do the thing.
    /// Different questions, and the second is the one you cannot get to by
    /// pressing keys.
    #[test]
    fn view_eight_shows_a_guide_beside_the_list() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 7");
        assert_eq!(ui.view, View::Guide);

        let out = render(&ui, &ctx, 96, 18);
        assert!(out.contains("7:guide"), "{out}");
        // Both panes: the titles, and the first guide's text.
        assert!(out.contains("Moving around"), "{out}");
        assert!(out.contains("The library tree"), "{out}");
        // The opening line, because that is what the pane shows by definition.
        // A word from further down is hostage to how long the sections above
        // it happen to be.
        assert!(
            out.contains("nvim tabs"),
            "the guide body is missing:\n{out}"
        );
    }

    #[test]
    fn choosing_a_guide_shows_it_and_starts_at_the_top() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Guide;

        press(&mut ui, &ctx, "j");
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
        assert_eq!(ui.guide_pane, Pane::Tracks, "Enter did not move into it");
        let out = render(&ui, &ctx, 96, 18);
        assert!(out.contains("The library tree"), "{out}");

        // Scroll, then pick a different guide: it starts at its own top
        // rather than wherever the last one had been left.
        press(&mut ui, &ctx, "3 j");
        assert_eq!(ui.guide_scroll, 3);
        let _ = render(&ui, &ctx, 96, 18);
        execute(&mut ui, &ctx, Command::PaneLeft);
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.guide_scroll, 0, "the new guide kept the old scroll");
    }

    /// The cursor's range is a deliberate over-estimate, so `G` must land on
    /// the last screenful rather than scrolling past the text into nothing.
    #[test]
    fn g_reaches_the_end_of_a_guide() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Guide;
        ui.guide_pane = Pane::Tracks;
        // The cursor's range comes from what was last drawn, so draw first —
        // as the event loop does before it ever sees a keypress.
        let _ = render(&ui, &ctx, 56, 10);

        press(&mut ui, &ctx, "G");
        // A narrow, short pane: prose wraps to far more lines than the source
        // has, and only a few fit. That is where an under-estimated range
        // leaves the end out of reach — a tall pane hides the bug, because
        // the clamp rescues it.
        let out = render(&ui, &ctx, 56, 10);
        // The guide's last word, which is where it wraps to whatever the pane
        // is — comparing a whole wrapped line would only test my arithmetic
        // about the width.
        let first = crate::guides::body(&crate::guides::GUIDES[0], &bind::defaults());
        let last_word = crate::guides::lines(&first)
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| l.split_whitespace().next_back())
            .expect("the guide is empty");
        assert!(out.contains(last_word), "the end is unreachable:\n{out}");
    }

    /// `G` then `k` has to move. It did not: the range was a guess wide of
    /// what was on screen, so the cursor sat tens of rows past the end and
    /// `k` walked back through phantom rows before anything happened.
    #[test]
    fn k_moves_back_up_immediately_after_g() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Guide;
        ui.guide_pane = Pane::Tracks;
        let _ = render(&ui, &ctx, 56, 10);

        press(&mut ui, &ctx, "G");
        let bottom = ui.guide_scroll;
        assert!(bottom > 0, "`G` did not move");

        press(&mut ui, &ctx, "k");
        assert_eq!(ui.guide_scroll, bottom - 1, "`k` did nothing after `G`");

        // And what is drawn actually changes, not just the number.
        let after = render(&ui, &ctx, 56, 10);
        press(&mut ui, &ctx, "G");
        assert_ne!(after, render(&ui, &ctx, 56, 10), "the pane did not scroll");
    }

    #[test]
    fn tab_swaps_the_two_panes_of_view_three() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "panes");
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 3");
        assert_eq!(ui.playlist_pane, Pane::Tree, "the list has focus first");

        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        assert_eq!(ui.playlist_pane, Pane::Tracks);
        execute(&mut ui, &ctx, Command::PaneLeft);
        assert_eq!(ui.playlist_pane, Pane::Tree);
    }

    /// `j` moves within whichever pane has focus, so the two cursors are
    /// independent — the same as view 1.
    #[test]
    fn each_pane_of_view_three_has_its_own_cursor() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "panes-cursor");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist open electronic");
        assert_eq!(ui.playlist_pane, Pane::Tracks, "opening puts you in it");

        press(&mut ui, &ctx, "j");
        assert_eq!((ui.playlist_sel, ui.playlists_sel), (1, 0));

        execute(&mut ui, &ctx, Command::PaneLeft);
        press(&mut ui, &ctx, "j");
        assert_eq!((ui.playlist_sel, ui.playlists_sel), (1, 1));
    }

    /// Adding means adding to the one you picked, so there has to *be* one —
    /// and the refusal names the way to start one rather than inventing a file.
    #[test]
    fn adding_with_nothing_open_names_the_remedy() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "add-nothing-open");
        ui.view = View::Sorted;

        execute(
            &mut ui,
            &ctx,
            Command::Add {
                target: crate::cmd::AddTarget::Playlist,
                path: None,
            },
        );
        assert!(ui.playlist.is_empty());
        assert!(ui.status.contains(":playlist new"), "{}", ui.status);
    }

    /// An edit is written through, the same rule ratings and plays follow:
    /// losing what you meant is worse than the cost of a write.
    #[test]
    fn an_edit_is_written_to_the_file_immediately() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, dir) = ui_with_saved(&store, "write-through");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist new roadtrip");
        let at = dir.join("playlists/roadtrip.m3u8");
        assert!(
            at.exists(),
            "a new playlist is a file at once: {}",
            ui.status
        );
        assert!(crate::m3u::read(&at).unwrap().is_empty());

        ui.view = View::Sorted;
        execute(
            &mut ui,
            &ctx,
            Command::Add {
                target: crate::cmd::AddTarget::Playlist,
                path: None,
            },
        );
        assert_eq!(crate::m3u::read(&at).unwrap().len(), 1, "{}", ui.status);

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        execute(&mut ui, &ctx, Command::Remove);
        assert!(
            crate::m3u::read(&at).unwrap().is_empty(),
            "the removal was not written"
        );
    }

    #[test]
    fn a_new_playlist_will_not_shadow_an_existing_name() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "new-clash");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist new electronic");
        assert!(ui.status.contains("already exists"), "{}", ui.status);
    }

    #[test]
    fn tab_completes_smart_playlist_names() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        // The name completes after the action.
        press(&mut ui, &ctx, ":");
        for c in "playlist open e".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "playlist open electronic ");

        // And the actions complete before it: what can be done, before what to
        // do it to.
        press(&mut ui, &ctx, "escape");
        press(&mut ui, &ctx, ":");
        for c in "playlist sa".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "playlist save ");
    }

    /// §9's M6 criterion: every default binding must actually fire. A binding
    /// that parses but resolves to nothing is exactly the failure this catches.
    #[test]
    fn every_default_binding_resolves_and_runs() {
        let store = fixture();
        let ctx = ctx_of(&store);

        for line in crate::cmd::bind::DEFAULTS {
            let Command::Bind {
                ctx: bctx,
                key,
                cmd,
            } = crate::cmd::parse::command(line).unwrap()
            else {
                panic!("{line:?} is not a bind")
            };

            let mut ui = ui_of(&store, true);
            // The binding must be reachable through lookup from somewhere the
            // user can actually be — a view, or a mode.
            ui.view = match bctx {
                crate::cmd::Context::Common | crate::cmd::Context::Transport => View::Tree,
                other => View::from_id(match other {
                    crate::cmd::Context::Library => ViewId::Library,
                    crate::cmd::Context::SortedLibrary => ViewId::Sorted,
                    crate::cmd::Context::Playlist => ViewId::Playlist,
                    crate::cmd::Context::Queue => ViewId::Queue,
                    crate::cmd::Context::Browser => ViewId::Browser,
                    crate::cmd::Context::Filters => ViewId::Filters,
                    crate::cmd::Context::Guide => ViewId::Guide,
                    crate::cmd::Context::Common | crate::cmd::Context::Transport => {
                        unreachable!()
                    }
                }),
            };
            if bctx == crate::cmd::Context::Transport {
                ui.mode = crate::cmd::Mode::Transport;
            }
            // The table holds resolved keys; the default string says
            // `<leader>`. Resolving here is what makes them comparable.
            let key = crate::cmd::keys::resolve_leader(&key, ui.leader);
            let found = crate::cmd::bind::lookup(&ui.bindings, ui.context(), &key);
            assert_eq!(found, Some(&*cmd), "{line:?} did not resolve");

            // And running it must not panic, whatever the view state.
            execute(&mut ui, &ctx, *cmd);
        }
    }

    #[test]
    fn gt_steps_through_the_views_and_wraps() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "g t");
        assert_eq!(ui.view, View::Sorted);
        press(&mut ui, &ctx, "g t");
        assert_eq!(ui.view, View::Playlist);
        press(&mut ui, &ctx, "g T");
        assert_eq!(ui.view, View::Sorted);

        // Wrapping, so you can always get anywhere with one key. Counted
        // from `ALL` rather than a literal, so adding a view does not silently
        // turn this into a test of something else.
        for _ in 0..View::ALL.len() - 1 {
            press(&mut ui, &ctx, "g t");
        }
        assert_eq!(ui.view, View::Tree);
    }

    /// `1gt` is view 1 while `gt` is "next" — the count changes the meaning,
    /// which is why a count is `Option` rather than defaulting to 1.
    #[test]
    fn a_count_before_gt_selects_a_view_directly() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "4");
        press(&mut ui, &ctx, "g t");
        assert_eq!(ui.view, View::Queue);

        press(&mut ui, &ctx, "1");
        press(&mut ui, &ctx, "g t");
        assert_eq!(ui.view, View::Tree, "1gt must mean view 1, not next");
    }

    #[test]
    fn g_tab_returns_to_the_previous_view() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "4");
        press(&mut ui, &ctx, "g t");
        assert_eq!(ui.view, View::Queue);
        press(&mut ui, &ctx, "g tab");
        assert_eq!(ui.view, View::Tree);
        // And back again — it is an alt-tab, not a one-way trip.
        press(&mut ui, &ctx, "g tab");
        assert_eq!(ui.view, View::Queue);
    }

    #[test]
    fn the_command_line_runs_a_command() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        assert!(ui.prompt.is_some(), "colon should open the prompt");
        type_line(&mut ui, &ctx, "view 3");
        assert!(ui.prompt.is_none(), "prompt should close on enter");
        assert_eq!(ui.view, View::Playlist);
    }

    #[test]
    fn a_bad_command_line_reports_rather_than_dying() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "frobnicate");
        assert!(ui.status.contains("frobnicate"), "{}", ui.status);
    }

    #[test]
    fn escape_abandons_the_prompt() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, ":");
        handle_key(KeyEvent::from(KeyCode::Char('q')), &mut ui, &ctx);
        handle_key(KeyEvent::from(KeyCode::Esc), &mut ui, &ctx);
        assert!(ui.prompt.is_none());
        assert!(!ui.quit, "the q went into the prompt, not to the binding");
    }

    /// A rebinding through `:bind` must take effect immediately.
    #[test]
    fn rebinding_at_runtime_takes_effect() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        // `j` moves down by default.
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.tree.selected, 1);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind common j top");
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.tree.selected, 0, "rebinding did not take");

        // And unbinding leaves the key inert.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "unbind common j");
        press(&mut ui, &ctx, "k");
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.tree.selected, 0);
    }

    #[test]
    fn a_context_binding_only_applies_to_its_view() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind queue j bottom");

        // In the tree, j still steps down.
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.tree.selected, 1);
    }

    #[test]
    fn search_moves_the_cursor_and_n_repeats() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "/");
        type_line(&mut ui, &ctx, "years");
        let first = ui.sorted;
        let track = ui.library.sorted()[first];
        assert_eq!(store.get(track).unwrap().display_title(), "Years");

        // `n` with a single match wraps back to it rather than losing it.
        press(&mut ui, &ctx, "n");
        assert_eq!(ui.sorted, first);
    }

    #[test]
    fn a_search_with_no_match_says_so() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        press(&mut ui, &ctx, "/");
        type_line(&mut ui, &ctx, "zzzznotthere");
        assert!(ui.status.contains("no match"), "{}", ui.status);
    }

    #[test]
    fn add_from_the_tree_takes_the_whole_selection() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        // Artist row selected: `y` adds everything under it — to the open
        // playlist, since adding means adding to the one you picked.
        ui.saved = Some(crate::playlist::Playlists::new(&scratch("add-tree")));
        new_playlist(&mut ui, &ctx, "mix");
        ui.view = View::Tree;
        let expected = ui.tree.tracks_for(&ui.library, &store).len();
        press(&mut ui, &ctx, "a");
        assert_eq!(ui.playlist.len(), expected);
        assert!(expected > 1, "the fixture should have a multi-track artist");

        press(&mut ui, &ctx, "e");
        assert_eq!(ui.queue.len(), expected);
    }

    #[test]
    fn remove_deletes_from_the_playlist_only() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.saved = Some(crate::playlist::Playlists::new(&scratch("remove-only")));
        new_playlist(&mut ui, &ctx, "mix");
        ui.view = View::Tree;
        press(&mut ui, &ctx, "a");
        let before = ui.playlist.len();

        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "d d");
        assert_eq!(ui.playlist.len(), before - 1);

        // In the tree there is nothing to remove, and it must say so rather
        // than deleting something else.
        ui.view = View::Tree;
        press(&mut ui, &ctx, "d d");
        assert!(ui.status.contains("nothing to remove"), "{}", ui.status);
    }

    #[test]
    fn toggles_go_through_the_settings_layer() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "s");
        assert!(ui.playback.shuffle);
        press(&mut ui, &ctx, "r");
        assert!(ui.playback.repeat);
        press(&mut ui, &ctx, "m");
        assert_eq!(ui.playback.aaa_mode, crate::next::AaaMode::Artist);

        // And `:set` reaches the same state.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "set shuffle=false");
        assert!(!ui.playback.shuffle);
    }

    /// The keymap listing survived view 7's deletion (#47) — as a guide, and
    /// generated, so a `:bind` shows up in it without anything being kept in
    /// step by hand.
    #[test]
    fn the_index_guide_lists_the_live_keymap() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Guide;
        ui.guide_sel = crate::guides::GUIDES
            .iter()
            .position(|g| g.title == "Index")
            .expect("an index guide");
        ui.guide_pane = Pane::Tracks;

        let out = render(&ui, &ctx, 100, 24);
        assert!(out.contains("common"), "no context heading:\n{out}");
        assert!(
            out.contains("7:guide"),
            "the tab strip should end at 7:\n{out}"
        );

        // A binding made now must appear, which a checked-in file could not
        // do. Asserted on the guide's body rather than on the frame: the
        // listing is far longer than a pane, and where a given key lands in it
        // is not this test's business.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind common \"g z q\" quit");
        let index = &crate::guides::GUIDES[ui.guide_sel];
        let body = crate::guides::body(index, &ui.bindings);
        assert!(
            body.contains("g z q"),
            "a new binding is missing from the index"
        );
    }

    #[test]
    fn the_prompt_replaces_the_status_line_while_open() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, ":");
        for c in "vol 50%".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        let out = render(&ui, &ctx, 100, 10);
        assert!(out.contains(":vol 50%"), "{out}");
        assert!(!out.contains("7 tracks"), "status should be hidden:\n{out}");
    }
    // ---- catalogs --------------------------------------------------------

    use crate::catalog::{Catalogs, Spec};

    /// A fixture split into two catalogs by directory.
    fn catalog_ui(store: &Store) -> Ui {
        let mut catalogs = Catalogs::default();
        catalogs.define("paak", vec![PathBuf::from("/m/paak")], None);
        catalogs.define("va", vec![PathBuf::from("/m/va")], None);
        let mut ui = Ui::new(
            PathBuf::from("/m"),
            Library::from_members(
                store,
                catalogs.members(store, crate::filter::Env::default()),
                true,
            ),
        );
        ui.catalogs = catalogs;
        ui
    }

    #[test]
    fn switching_catalogs_narrows_the_library() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);
        let everything = ui.library.len();

        execute(
            &mut ui,
            &ctx,
            Command::Catalog(Spec::Only(vec!["paak".into()])),
        );
        assert!(ui.library.len() < everything, "library did not narrow");
        for (_, track) in ui
            .library
            .sorted()
            .iter()
            .filter_map(|id| store.get(*id).map(|t| (id, t)))
        {
            assert!(
                track.path.starts_with("/m/paak"),
                "{:?} leaked in",
                track.path
            );
        }
    }

    /// The whole point: shuffle must not cross a catalog boundary. §5 builds
    /// its pool from the library, so this is really a test that nothing has
    /// started building pools from the Store instead.
    #[test]
    fn playback_stays_inside_the_active_catalog() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);
        execute(
            &mut ui,
            &ctx,
            Command::Catalog(Spec::Only(vec!["paak".into()])),
        );

        ui.playback.shuffle = true;
        ui.playback.repeat = true;
        ui.playback.current = ui.library.sorted().first().copied();

        for _ in 0..40 {
            advance(&mut ui, &ctx, true);
            let Some(id) = ui.playback.current else { break };
            let path = &store.get(id).unwrap().path;
            assert!(
                path.starts_with("/m/paak"),
                "shuffle escaped the catalog to {path:?}"
            );
        }
    }

    /// Playlists are explicit choices and must cross catalogs.
    #[test]
    fn a_playlist_still_plays_tracks_outside_the_active_catalog() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);

        // Queue something from the "va" catalog while "paak" is active.
        let va = store
            .iter()
            .find(|(_, t)| t.path.starts_with("/m/va"))
            .map(|(id, _)| id)
            .unwrap();
        ui.playlist.push(va);
        execute(
            &mut ui,
            &ctx,
            Command::Catalog(Spec::Only(vec!["paak".into()])),
        );

        assert_eq!(ui.playlist.len(), 1, "the playlist was filtered");
        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "<leader> x");
        assert_eq!(
            ui.playback.current,
            Some(va),
            "a playlist entry outside the catalog must still play"
        );
    }

    #[test]
    fn the_queue_also_crosses_catalogs() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);
        let va = store
            .iter()
            .find(|(_, t)| t.path.starts_with("/m/va"))
            .map(|(id, _)| id)
            .unwrap();
        ui.queue.push_back(va);
        execute(
            &mut ui,
            &ctx,
            Command::Catalog(Spec::Only(vec!["paak".into()])),
        );
        assert_eq!(ui.queue.len(), 1);

        ui.playback.current = ui.library.sorted().first().copied();
        advance(&mut ui, &ctx, true);
        assert_eq!(ui.playback.current, Some(va), "the queue was skipped");
    }

    #[test]
    fn an_unknown_catalog_name_is_reported() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);
        let before = ui.library.len();
        execute(
            &mut ui,
            &ctx,
            Command::Catalog(Spec::Only(vec!["nosuch".into()])),
        );
        assert!(ui.status.contains("nosuch"), "{}", ui.status);
        assert_eq!(ui.library.len(), before, "a bad name changed the library");
    }

    #[test]
    fn the_status_line_says_which_catalogs_are_active() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);
        execute(
            &mut ui,
            &ctx,
            Command::Catalog(Spec::Only(vec!["paak".into()])),
        );
        ui.status.clear();
        let out = render(&ui, &ctx, 110, 10);
        assert!(out.contains("[paak]"), "{out}");
    }

    #[test]
    fn switching_catalogs_resets_the_cursors() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);
        ui.view = View::Sorted;
        ui.sorted = ui.library.sorted().len() - 1;

        execute(
            &mut ui,
            &ctx,
            Command::Catalog(Spec::Only(vec!["paak".into()])),
        );
        // The old row may not exist any more; leaving the cursor there would
        // index past the end.
        assert_eq!(ui.sorted, 0);
        assert!(ui.sorted < ui.library.sorted().len());
    }

    /// View 7 used to list catalogs too, which was always a duplicate of view
    /// 6 — the scope view lists them as its whole purpose. Deleting view 7
    /// (#47) lost nothing here, and this checks the survivor rather than
    /// assuming it.
    #[test]
    fn catalogs_show_in_the_scope_view() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);
        ui.view = View::Filters;
        let out = render(&ui, &ctx, 110, 24);
        assert!(out.contains("paak"), "{out}");
        assert!(out.contains("va"), "{out}");
    }
    // ---- album headers (#7) ----------------------------------------------

    /// Divider rows only. The pane border is also drawn with `─`, so a naive
    /// substring search matches the frame itself.
    fn dividers(out: &str) -> Vec<String> {
        out.lines()
            .filter(|l| {
                let body = l.trim_start_matches(['│', ' ']);
                body.starts_with("──") && !l.starts_with(['┌', '└'])
            })
            .map(|l| l.trim().to_string())
            .collect()
    }

    /// Dividers are drawn, but the cursor still counts tracks — the whole
    /// point of keeping the two coordinate systems apart.
    #[test]
    fn dividers_do_not_shift_the_cursor() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        // Moving down N times must land on the (N+1)th *track*, however many
        // dividers were drawn above it.
        for _ in 0..3 {
            press(&mut ui, &ctx, "j");
        }
        assert_eq!(ui.sorted, 3);
        let expected = ui.library.sorted()[3];
        assert_eq!(ui.selected_track(&ctx), Some(expected));
    }

    #[test]
    fn the_sorted_view_is_divided_by_album() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        let out = render(&ui, &ctx, 100, 24);
        let found = dividers(&out);
        assert!(!found.is_empty(), "no divider drawn:\n{out}");
        // The fixture spans several artists, so headers name them.
        assert!(
            found.iter().any(|d| d.contains("Malibu")),
            "{found:?}\n{out}"
        );
    }

    #[test]
    fn dividers_can_be_turned_off() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "set album_headers=false");
        let out = render(&ui, &ctx, 100, 24);
        assert!(
            dividers(&out).is_empty(),
            "dividers survived being turned off: {:?}\n{out}",
            dividers(&out)
        );
    }

    /// Every track must still be reachable with the dividers in place.
    #[test]
    fn every_track_is_still_selectable() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        let total = ui.library.sorted().len();
        let mut seen = Vec::new();
        for _ in 0..total {
            seen.push(ui.selected_track(&ctx).expect("a divider was selectable"));
            press(&mut ui, &ctx, "j");
        }
        assert_eq!(seen.len(), total);
        assert_eq!(seen, ui.library.sorted(), "order or reachability changed");
    }

    #[test]
    fn the_playlist_is_not_divided() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        // Add tracks from several albums.
        for id in ui.library.sorted().to_vec() {
            ui.playlist.push(id);
        }
        ui.view = View::Playlist;
        let out = render(&ui, &ctx, 100, 24);
        // A playlist is a deliberate ordering; a header would imply a grouping
        // that is not there.
        assert!(
            dividers(&out).is_empty(),
            "the playlist was divided:\n{out}"
        );
    }
    // ---- counts and chords (#6) ------------------------------------------

    #[test]
    fn a_count_multiplies_a_motion() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "3");
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.sorted, 3);

        press(&mut ui, &ctx, "2");
        press(&mut ui, &ctx, "k");
        assert_eq!(ui.sorted, 1);
    }

    #[test]
    fn a_multi_digit_count_accumulates() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        let len = ui.library.sorted().len();

        for c in "12".chars() {
            press(&mut ui, &ctx, &c.to_string());
        }
        press(&mut ui, &ctx, "j");
        // Clamped at the end rather than running off it.
        assert_eq!(ui.sorted, (12).min(len - 1));
    }

    /// `0` continues a count but may never start one — in vim it is a motion
    /// of its own, and here it would otherwise swallow the keypress.
    #[test]
    fn zero_cannot_start_a_count() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "0");
        assert_eq!(ui.count, None, "0 started a count");

        // But it extends one.
        press(&mut ui, &ctx, "1");
        press(&mut ui, &ctx, "0");
        assert_eq!(ui.count, Some(10));
    }

    #[test]
    fn count_g_goes_to_that_row_and_bare_g_goes_to_the_end() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        let len = ui.library.sorted().len();

        press(&mut ui, &ctx, "3");
        press(&mut ui, &ctx, "G");
        assert_eq!(ui.sorted, 2, "counts are 1-based, as in vim");

        press(&mut ui, &ctx, "G");
        assert_eq!(ui.sorted, len - 1);

        press(&mut ui, &ctx, "g g");
        assert_eq!(ui.sorted, 0);
    }

    #[test]
    fn a_count_is_consumed_by_the_command_it_precedes() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "3");
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.count, None, "the count outlived its command");

        // So the next motion is a single step, not another three.
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.sorted, 4);
    }

    /// A half-typed chord must show itself, or it feels like a dropped key.
    #[test]
    fn a_pending_chord_is_visible_and_abandonable() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        handle_key(KeyEvent::from(KeyCode::Char('g')), &mut ui, &ctx);
        assert_eq!(ui.pending.len(), 1);
        assert_eq!(ui.pending_label().as_deref(), Some("g"));
        assert!(render(&ui, &ctx, 100, 10).contains(" g"), "not shown");

        // An unmatched follow-up clears rather than firing something random.
        handle_key(KeyEvent::from(KeyCode::Char('q')), &mut ui, &ctx);
        assert!(ui.pending.is_empty());
        assert!(!ui.quit, "the q leaked to its own binding");
        assert_eq!(ui.pending_label(), None);
    }

    /// The bug this replaced: a completed chord left its indicator behind for
    /// ever, hiding the now-playing line.
    #[test]
    fn a_completed_chord_leaves_no_indicator() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "g g");
        assert_eq!(ui.pending_label(), None, "the chord left a trace");

        let out = render(&ui, &ctx, 100, 10);
        assert!(!out.contains("  g "), "a stale `g` is still drawn:\n{out}");
        // And the normal indicators are back.
        assert!(
            out.contains("7 tracks"),
            "the summary did not return:\n{out}"
        );
        assert!(
            out.contains("[all]"),
            "the mode flags did not return:\n{out}"
        );
    }

    /// A bare count had no indicator at all before, because the digit branch
    /// returned early without touching anything.
    #[test]
    fn a_bare_count_is_shown() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "1");
        press(&mut ui, &ctx, "2");
        assert_eq!(ui.pending_label().as_deref(), Some("12"));
        assert!(render(&ui, &ctx, 100, 10).contains("12"));

        // Consumed by the motion it precedes.
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.pending_label(), None);
    }

    #[test]
    fn a_count_shows_beside_a_pending_chord() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "4");
        handle_key(KeyEvent::from(KeyCode::Char('g')), &mut ui, &ctx);
        assert_eq!(ui.pending_label().as_deref(), Some("4g"));
    }

    /// An exact match must win over being a prefix of something longer, or `G`
    /// would wait to see whether a `G x` binding was meant.
    #[test]
    fn an_exact_match_fires_immediately() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind common \"G x\" top");

        press(&mut ui, &ctx, "G");
        assert!(ui.pending.is_empty(), "G waited instead of firing");
        assert_eq!(ui.sorted, ui.library.sorted().len() - 1);
    }

    #[test]
    fn chords_can_be_bound_at_runtime() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, ":");
        // `w` is unbound, so it can start a chord.
        type_line(&mut ui, &ctx, "bind common \"w w\" bottom");
        press(&mut ui, &ctx, "w w");
        assert_eq!(ui.sorted, ui.library.sorted().len() - 1);
    }

    /// Binding a chord onto an already-bound key makes the chord unreachable:
    /// the shorter binding is an exact match and fires first. Worth pinning,
    /// because it looks like the chord simply does not work.
    #[test]
    fn a_chord_cannot_extend_an_already_bound_key() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        // `u` is undo by default.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind common \"u u\" bottom");
        press(&mut ui, &ctx, "u u");
        assert_eq!(ui.sorted, 0, "the chord fired despite `u` being bound");

        // Freeing the key makes the chord reachable.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "unbind common u");
        press(&mut ui, &ctx, "u u");
        assert_eq!(ui.sorted, ui.library.sorted().len() - 1);
    }

    #[test]
    fn the_relative_gutter_counts_tracks_not_rows() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.relative_numbers = true;
        press(&mut ui, &ctx, "j");

        let out = render(&ui, &ctx, 100, 24);
        // The cursor row shows its absolute position, others their distance —
        // and dividers must not be counted, or `3j` would not be three away.
        assert!(
            out.contains(" 2  "),
            "no absolute number on the cursor row:\n{out}"
        );
        assert!(out.contains("  1 "), "no relative distance:\n{out}");
    }

    #[test]
    fn the_gutter_is_off_by_default() {
        let store = fixture();
        let ui = ui_of(&store, true);
        assert!(!ui.relative_numbers, "the gutter must be opt-in");
    }
    // ---- pane navigation (#5) --------------------------------------------

    /// `l` is `activate`: it opens a node and, at a track, plays it — the
    /// same key snacks binds to `confirm`. `h` closes, walking up when the row
    /// is already shut, which is how you get out without a notion of "level".
    #[test]
    fn l_and_h_fold_at_every_level() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let closed = ui.tree.rows(&ui.library, &store).len();

        press(&mut ui, &ctx, "l");
        let with_albums = ui.tree.rows(&ui.library, &store).len();
        assert!(with_albums > closed, "`l` did not open the artist");

        // Down onto an album and open that too: tracks appear inline, which
        // is the whole point of one pane.
        press(&mut ui, &ctx, "j");
        press(&mut ui, &ctx, "l");
        assert!(
            ui.tree.rows(&ui.library, &store).len() > with_albums,
            "`l` did not open the album"
        );

        // `h` closes the album we are on.
        press(&mut ui, &ctx, "h");
        assert_eq!(ui.tree.rows(&ui.library, &store).len(), with_albums);

        // Already closed, so `h` closes the parent and moves to it.
        press(&mut ui, &ctx, "h");
        assert_eq!(ui.tree.rows(&ui.library, &store).len(), closed);
        assert_eq!(ui.tree.selected, 0, "the cursor did not follow up");
    }

    /// At a track there is nothing left to open, so `l` plays it — snacks
    /// binds `l` to `confirm`, and confirming a leaf is doing the thing.
    #[test]
    fn l_on_a_track_plays_it() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        expand_to_tracks(&mut ui, &store);
        press(&mut ui, &ctx, "j");
        press(&mut ui, &ctx, "j");

        assert!(ui.playback.current.is_none());
        press(&mut ui, &ctx, "l");
        assert!(ui.playback.current.is_some(), "`l` did not play the track");
    }

    /// A count survives the `in <pane>` wrapper, so `2 in tracks down` moves
    /// two rows in the pane you are not standing in.
    #[test]
    fn a_count_passes_through_the_pane_wrapper() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "in-count");
        new_playlist(&mut ui, &ctx, "mix");
        for (id, _) in store.iter() {
            ui.playlist.push(id);
        }
        ui.playlist_pane = Pane::Tree;

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "in tracks down");
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "in tracks down");
        assert_eq!(ui.playlist_sel, 2, "the count was dropped by `in`");
        assert_eq!(ui.playlist_pane, Pane::Tree);
    }

    /// `in <pane>` aims a command at view 3's other pane without moving focus.
    /// The library tree has one pane now, so it is not a target.
    #[test]
    fn the_in_wrapper_is_expressible_on_the_command_line() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "in-wrapper");
        new_playlist(&mut ui, &ctx, "mix");
        for (id, _) in store.iter() {
            ui.playlist.push(id);
        }
        ui.playlist_pane = Pane::Tree;

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "in tracks bottom");
        assert_eq!(ui.playlist_sel, ui.playlist.len() - 1, "it did not reach");
        assert_eq!(ui.playlist_pane, Pane::Tree, "focus moved");
    }

    #[test]
    fn pane_commands_do_nothing_in_a_single_pane_view() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 1;

        // No panes here, so these must not disturb anything.
        press(&mut ui, &ctx, "h");
        press(&mut ui, &ctx, "l");
        press(&mut ui, &ctx, "^j");
        assert_eq!(ui.sorted, 1);
    }
    // ---- completion (#4) --------------------------------------------------

    fn typed(ui: &Ui) -> String {
        ui.prompt
            .as_ref()
            .map(|p| p.buffer.clone())
            .unwrap_or_default()
    }

    fn tab(ui: &mut Ui, ctx: &Context<'_>) {
        handle_key(KeyEvent::from(KeyCode::Tab), ui, ctx);
    }

    /// What the completion menu is highlighting, read back off the rendered
    /// buffer rather than out of `prompt.selected`.
    ///
    /// A selection that state agrees on but the screen does not show is not a
    /// selection, and it is the failure that matters here: the menu scrolls,
    /// so cycling can move the choice out of view.
    fn rendered_selection(ui: &Ui, ctx: &Context<'_>, w: u16, h: u16) -> Option<String> {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, ui, ctx)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let want = ui.theme.get(theme::Role::CompletionSelected);
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if Some(cell.fg) == want.fg
                    && Some(cell.bg) == want.bg
                    && cell.modifier.contains(ratatui::style::Modifier::BOLD)
                {
                    out.push_str(cell.symbol());
                }
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// `C-n` and `C-p` walk the menu, and the walk is visible.
    #[test]
    fn ctrl_n_and_ctrl_p_move_the_highlighted_candidate() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        let candidates = ui.prompt.as_ref().unwrap().candidates().to_vec();
        assert!(candidates.len() > 3, "need a menu to walk");

        // The menu opens on the first candidate, because `C-y` would take it.
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates[0].as_str()),
            "an opened menu must show what C-y would accept"
        );

        press(&mut ui, &ctx, "^n");
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates[1].as_str())
        );
        press(&mut ui, &ctx, "^p");
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates[0].as_str())
        );
        // Wrapping backwards off the front lands on the last.
        press(&mut ui, &ctx, "^p");
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates.last().unwrap().as_str())
        );
    }

    fn tagged(gain: Option<f32>, peak: Option<f32>, album: Option<f32>) -> crate::store::Track {
        crate::store::Track {
            path: PathBuf::from("/x.flac"),
            replaygain: Some(crate::store::ReplayGain {
                track_gain: gain,
                track_peak: peak,
                album_gain: album,
                album_peak: None,
            }),
            ..Default::default()
        }
    }

    /// A ReplayGain figure is decibels; the player multiplies by a linear
    /// gain. -6 dB is half the amplitude.
    #[test]
    fn replaygain_decibels_become_a_linear_gain() {
        let store = fixture();
        let mut ui = ui_of(&store, true);
        ui.replaygain = crate::cmd::ReplayGainMode::Track;

        let track = tagged(Some(-6.0206), None, None);
        assert!((replay_gain(&ui, &track) - 0.5).abs() < 1e-3);

        let quiet = tagged(Some(6.0206), None, None);
        assert!((replay_gain(&ui, &quiet) - 2.0).abs() < 1e-3);
    }

    /// A positive gain on a quiet master is exactly where clipping happens,
    /// and the peak tag is there to prevent it. Being a decibel under the
    /// target is better than being over full scale.
    #[test]
    fn a_known_peak_caps_the_gain() {
        let store = fixture();
        let mut ui = ui_of(&store, true);
        ui.replaygain = crate::cmd::ReplayGainMode::Track;

        // +6 dB wanted, but the file already peaks at 0.8: 1/0.8 = 1.25 is
        // all that fits.
        let track = tagged(Some(6.0206), Some(0.8), None);
        assert!((replay_gain(&ui, &track) - 1.25).abs() < 1e-3);

        // A cut is never capped — it cannot clip.
        let cut = tagged(Some(-6.0206), Some(0.99), None);
        assert!((replay_gain(&ui, &cut) - 0.5).abs() < 1e-3);
    }

    /// `auto` follows the play order: shuffling jumps between masters so each
    /// track's own level is what matters, while playing an album in order
    /// should keep the album's internal balance.
    #[test]
    fn auto_follows_the_play_order() {
        let store = fixture();
        let mut ui = ui_of(&store, true);
        ui.replaygain = crate::cmd::ReplayGainMode::Auto;
        let track = tagged(Some(-6.0206), None, Some(-12.041));

        ui.playback.shuffle = false;
        assert!(
            (replay_gain(&ui, &track) - 0.25).abs() < 1e-3,
            "in order, the album gain should win"
        );

        ui.playback.shuffle = true;
        assert!(
            (replay_gain(&ui, &track) - 0.5).abs() < 1e-3,
            "shuffled, the track gain should win"
        );
    }

    /// Off means off, and an untagged file is left alone — so turning this on
    /// changes nothing until something has actually been tagged.
    #[test]
    fn untagged_and_disabled_both_play_at_unity() {
        let store = fixture();
        let mut ui = ui_of(&store, true);
        let track = tagged(Some(-6.0206), None, None);

        ui.replaygain = crate::cmd::ReplayGainMode::Off;
        assert_eq!(replay_gain(&ui, &track), 1.0);

        ui.replaygain = crate::cmd::ReplayGainMode::Track;
        let untagged = crate::store::Track::default();
        assert_eq!(replay_gain(&ui, &untagged), 1.0);
    }

    /// Album mode falls back to the track figure rather than giving up: a file
    /// tagged by a track-only scan should still be normalised.
    #[test]
    fn album_mode_falls_back_to_the_track_figure() {
        let store = fixture();
        let mut ui = ui_of(&store, true);
        ui.replaygain = crate::cmd::ReplayGainMode::Album;
        let track = tagged(Some(-6.0206), None, None);
        assert!((replay_gain(&ui, &track) - 0.5).abs() < 1e-3);
    }

    /// Silence has no decibel value, and a log that printed `-inf dB` or
    /// `NaN` for a muted track would be worse than one that said so.
    #[test]
    fn a_gain_renders_as_decibels_or_says_silent() {
        assert_eq!(db_label(0.0), "silent");
        assert_eq!(db_label(-1.0), "silent");
        assert_eq!(db_label(1.0), "+0.0 dB");
        assert_eq!(db_label(0.5), "-6.0 dB");
        assert_eq!(db_label(2.0), "+6.0 dB");
    }

    fn saved_with(shuffle: bool, aaa: &str, rng: u64) -> crate::session::Session {
        crate::session::Session {
            version: 1,
            queue: Vec::new(),
            open_playlist: None,
            view: None,
            current: None,
            position_secs: 0.0,
            modes: crate::session::Modes {
                shuffle,
                repeat: true,
                repeat_current: false,
                continue_playing: true,
                aaa_mode: aaa.into(),
                replaygain: "album".into(),
                active: "library".into(),
                rng_state: rng,
            },
        }
    }

    /// The precedence rule: if you wrote it in `config.toml` you meant it, and
    /// anything the config does not mention remembers what you last did.
    ///
    /// The failure this prevents is the confusing one — editing the config and
    /// seeing no effect, because a stale session file silently outranks it.
    #[test]
    fn the_config_wins_where_it_speaks_and_the_session_fills_the_rest() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        // The config spoke about shuffle, and set it on.
        ui.playback.shuffle = true;
        let fixed = std::collections::HashSet::from([SettingKey::Shuffle]);

        // The session remembers shuffle off and aaa_mode album.
        restore_session(&mut ui, &ctx, &saved_with(false, "album", 7), &fixed);

        assert!(ui.playback.shuffle, "the config named shuffle and lost");
        assert_eq!(
            ui.playback.aaa_mode,
            crate::next::AaaMode::Album,
            "the config was silent on aaa_mode, so the session should win"
        );
        assert!(ui.playback.repeat, "likewise repeat");
    }

    /// Shuffle is a seeded permutation, so the seed *is* the order. Restoring
    /// the mode without it resumes a different sequence while looking correct.
    #[test]
    fn the_shuffle_order_is_restored_not_merely_the_mode() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        restore_session(
            &mut ui,
            &ctx,
            &saved_with(true, "all", 0xdead_beef),
            &Default::default(),
        );
        assert!(ui.playback.shuffle);
        assert_eq!(
            ui.playback.rng_state, 0xdead_beef,
            "the seed was lost, so the shuffle order silently changed"
        );
    }

    /// A round trip through capture must not change anything, or resuming
    /// twice would drift.
    #[test]
    fn capture_then_restore_is_a_fixed_point() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        let ids: Vec<TrackId> = store.iter().map(|(id, _)| id).take(3).collect();
        for id in &ids {
            ui.queue.push_back(*id);
        }
        ui.playback.current = Some(ids[0]);
        ui.playback.shuffle = true;
        ui.playback.rng_state = 99;
        ui.replaygain = crate::cmd::ReplayGainMode::Track;

        let first = capture_session(&ui, &ctx);
        let mut fresh = ui_of(&store, true);
        restore_session(&mut fresh, &ctx, &first, &Default::default());
        let second = capture_session(&fresh, &ctx);

        assert_eq!(first.queue, second.queue);
        assert_eq!(first.current, second.current);
        assert_eq!(first.modes, second.modes);
    }

    /// Restoring loads the track, which emits `Loaded` — a save point — while
    /// the position is still 0, because the seek that follows has not been
    /// processed yet. Saving there would overwrite the very position being
    /// resumed, and it would look like resumption simply did not work.
    #[test]
    fn resuming_does_not_immediately_save_over_the_position_it_restored() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        let dir = std::env::temp_dir().join("gmuse-session-resume-guard");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        ui.session_path = Some(path.clone());

        let id = store.iter().next().unwrap().0;
        let mut saved = saved_with(false, "all", 0);
        saved.current = Some(store.get(id).unwrap().path.clone());
        saved.position_secs = 42.5;

        restore_session(&mut ui, &ctx, &saved, &Default::default());
        assert!(ui.resuming, "the guard was not raised");

        save_session(&mut ui, &ctx);
        assert!(
            !path.exists(),
            "the restored position was overwritten before its seek landed"
        );

        // Once the resumed track's own `Loaded` has gone by, saving resumes.
        ui.resuming = false;
        save_session(&mut ui, &ctx);
        assert!(path.exists(), "saving never restarted");
    }

    /// A track that has moved or gone is skipped, not an error — a library
    /// reorganised between sessions must not stop the player starting.
    #[test]
    fn tracks_that_have_gone_are_skipped() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        let mut saved = saved_with(false, "all", 0);
        saved.queue = vec![PathBuf::from("/gone/a.mp3"), PathBuf::from("/gone/b.mp3")];
        saved.current = Some(PathBuf::from("/gone/c.mp3"));
        restore_session(&mut ui, &ctx, &saved, &Default::default());

        assert!(ui.queue.is_empty(), "phantom entries were restored");
        assert_eq!(ui.playback.current, None);
        assert!(ui.status.contains("gone"), "no notice: {:?}", ui.status);
    }

    /// The template reaches the status line from the config, and a bad one is
    /// a reported problem rather than something the renderer discovers.
    #[test]
    fn the_now_playing_template_comes_from_the_config() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        let loaded = crate::config::parse_text(
            r#"
            [format]
            now_playing = "{title} :: {file_under}"
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        ui.now_playing = loaded.now_playing;

        let id = store.iter().next().unwrap().0;
        ui.playback.current = Some(id);
        let track = store.get(id).unwrap();

        let out = render(&ui, &ctx, 120, 20);
        assert!(
            out.contains(&format!(
                "{} :: {}",
                track.display_title(),
                track.file_under()
            )),
            "the configured template was not used:\n{out}"
        );
    }

    #[test]
    fn a_bad_template_is_a_config_error_and_leaves_the_default() {
        let loaded = crate::config::parse_text(
            r#"
            [format]
            now_playing = "{nosuchfield}"
            "#,
        );
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert_eq!(loaded.errors[0].where_, "format.now_playing");
        assert!(loaded.errors[0].message.contains("nosuchfield"));
        assert_eq!(
            loaded.now_playing,
            crate::format::Template::default(),
            "a rejected template should leave the default in place"
        );
    }

    /// `:filter` narrows the library, and clearing it restores everything.
    #[test]
    fn a_filter_narrows_the_library_and_clears() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let all = ui.library.len();
        assert!(all > 2);

        let first = store.iter().next().unwrap().1.artist.clone().unwrap();
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, &format!("filter artist=\"{first}\""));
        let narrowed = ui.library.len();
        assert!(narrowed > 0 && narrowed < all, "{narrowed} of {all}");

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "filter");
        assert_eq!(ui.library.len(), all, "clearing should restore everything");
    }

    /// §7's combining rule: active catalogs union, the filter intersects. The
    /// failure to avoid is a filter that quietly widens the library past the
    /// catalogs, or catalogs that stop narrowing once a filter is set.
    #[test]
    fn a_filter_intersects_the_active_catalogs() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        // A filter that matches everything must not add tracks the catalogs
        // excluded, and must not remove any they included.
        let before = ui.library.len();
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "filter duration>=0");
        assert_eq!(
            ui.library.len(),
            before,
            "an all-matching filter changed the set"
        );

        // And one that matches nothing empties it.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "filter year>9999");
        assert_eq!(ui.library.len(), 0);
    }

    /// Playlists and the queue hold `TrackId`s straight into the `Store` and
    /// must survive a filter — crossing boundaries is the point of them.
    #[test]
    fn a_filter_leaves_the_playlist_and_queue_alone() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        let ids: Vec<TrackId> = store.iter().map(|(id, _)| id).take(3).collect();
        for id in &ids {
            ui.queue.push_back(*id);
            ui.playlist.push(*id);
        }

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "filter year>9999");
        assert_eq!(
            ui.library.len(),
            0,
            "the filter should have emptied the library"
        );
        assert_eq!(ui.queue.len(), 3, "the queue was filtered");
        assert_eq!(ui.playlist.len(), 3, "the playlist was filtered");
    }

    /// A bad expression is a reported problem, not a panic or a silent no-op.
    #[test]
    fn a_bad_filter_expression_says_so() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let before = ui.library.len();

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "filter nosuchfield=1");
        assert!(ui.status.contains("nosuchfield"), "{:?}", ui.status);
        assert_eq!(ui.library.len(), before, "a bad filter changed the library");
    }

    /// A catalog can be defined by an expression rather than by roots, and
    /// membership is roots AND where when both are given.
    #[test]
    fn a_catalog_can_be_defined_by_a_filter() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let all = ui.library.len();

        let loaded = crate::config::parse_text(
            r#"
            [catalogs.recent]
            where = "duration>100"
            [catalogs.nothing]
            where = "year>9999"
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);

        ui.catalogs = loaded.catalogs;
        ui.catalogs
            .apply(crate::catalog::Spec::Only(vec!["nothing".into()]))
            .unwrap();
        rebuild_library(&mut ui, &ctx);
        assert_eq!(ui.library.len(), 0, "a where-only catalog should apply");

        ui.catalogs
            .apply(crate::catalog::Spec::Only(vec!["recent".into()]))
            .unwrap();
        rebuild_library(&mut ui, &ctx);
        assert!(!ui.library.is_empty() && ui.library.len() <= all);
    }

    /// Active catalogs union — the rule §7 settled, and the reason
    /// `:catalog folk pop` gives you both rather than nothing.
    #[test]
    fn active_catalogs_union() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        let loaded = crate::config::parse_text(
            r#"
            [catalogs.none]
            where = "year>9999"
            [catalogs.all_of_them]
            where = "duration>=0"
            "#,
        );
        ui.catalogs = loaded.catalogs;
        ui.catalogs
            .apply(crate::catalog::Spec::Only(vec![
                "none".into(),
                "all_of_them".into(),
            ]))
            .unwrap();
        rebuild_library(&mut ui, &ctx);
        assert!(
            !ui.library.is_empty(),
            "a union with an empty catalog should not empty the library"
        );
    }

    /// A bad expression names the catalog it came from, and the catalog is
    /// dropped rather than silently matching everything.
    #[test]
    fn a_bad_catalog_expression_names_the_catalog() {
        let loaded = crate::config::parse_text(
            r#"
            [catalogs.broken]
            where = "nosuchfield=1"
            "#,
        );
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert_eq!(loaded.errors[0].where_, "catalogs.broken.where");
        assert!(loaded.catalogs.defined().is_empty());
    }

    #[test]
    fn a_catalog_with_neither_roots_nor_where_is_rejected() {
        let loaded = crate::config::parse_text(
            "[catalogs.empty]
",
        );
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert!(loaded.errors[0].message.contains("neither"));
    }

    /// View 6 lists the catalogs, shows which are on, and toggles them.
    #[test]
    fn view_six_lists_and_toggles_catalogs() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.catalogs.define("alpha", vec![PathBuf::from("/m")], None);
        ui.catalogs
            .define("beta", vec![PathBuf::from("/other")], None);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "view 6");
        assert_eq!(ui.view, View::Filters);
        let out = render(&ui, &ctx, 100, 20);
        assert!(out.contains("alpha") && out.contains("beta"), "{out}");
        assert!(out.contains("[x]"), "nothing shown as active:\n{out}");

        // Space turns the one under the cursor off. Everything was active, so
        // this has to name the others rather than collapsing to nothing.
        press(&mut ui, &ctx, "o");
        assert!(!ui.catalogs.is_active("alpha"), "space did not toggle");
        assert!(ui.catalogs.is_active("beta"), "it turned off the wrong one");

        // Enter makes the one under the cursor the only one.
        press(&mut ui, &ctx, "j");
        press(&mut ui, &ctx, "enter");
        assert!(ui.catalogs.is_active("beta"));
        assert!(!ui.catalogs.is_active("alpha"));
    }

    /// A play is counted once, however long the track is left running, and
    /// only after enough of it has been heard.
    #[test]
    fn a_play_is_counted_once_and_only_when_earned() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        ui.events_path = Some(crate::events::log_path(&scratch("history-ui"), "test"));

        let (id, track) = store.iter().next().unwrap();
        let (path, half) = (track.path.clone(), track.duration / 2);
        ui.playback.current = Some(id);

        // Too early: a skip is not a play.
        record_play_if_earned(&mut ui, &ctx, Duration::from_secs(1));
        assert!(ui.history.get(&path).is_none(), "counted a skip");

        record_play_if_earned(&mut ui, &ctx, half);
        assert_eq!(ui.history.get(&path).unwrap().plays, 1);

        // Still playing, later in the same track: it must not count again.
        record_play_if_earned(&mut ui, &ctx, track.duration);
        assert_eq!(
            ui.history.get(&path).unwrap().plays,
            1,
            "the same track was counted twice"
        );

        // A different track becoming audible clears the guard, so the next one
        // can earn its own play.
        ui.counted_current = false;
        record_play_if_earned(&mut ui, &ctx, track.duration);
        assert_eq!(ui.history.get(&path).unwrap().plays, 2);
    }

    /// A change is durable because an event was appended, not because a file
    /// was rewritten — and folding that log back must reproduce the state.
    #[test]
    fn a_change_is_appended_and_folds_back() {
        let store = fixture();
        let mut ui = ui_of(&store, true);
        let dir = scratch("history-append");
        ui.events_path = Some(crate::events::log_path(&dir, "test"));

        ui.history.note_played(std::path::Path::new("/m/x.mp3"), 5);
        assert!(ui.history.is_dirty());
        record(&mut ui, crate::events::Kind::Played("/m/x.mp3".into()));
        assert!(!ui.history.is_dirty(), "recording should clear the flag");

        // The base is untouched — that is what lets two machines share a
        // directory without one whole file losing to the other.
        assert!(
            !dir.join("history.toml").exists(),
            "recording must not rewrite the base"
        );

        let (folded, applied) = crate::events::folded(&dir.join("history.toml"), &dir);
        assert_eq!(applied, 1);
        assert_eq!(
            folded.get(std::path::Path::new("/m/x.mp3")).unwrap().plays,
            1
        );
    }

    /// With nowhere to record — the default until a path is set — nothing is
    /// counted and nothing is written.
    #[test]
    fn without_a_history_path_nothing_is_recorded() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let (id, _) = store.iter().next().unwrap();
        ui.playback.current = Some(id);
        record_play_if_earned(&mut ui, &ctx, Duration::from_secs(9999));
        assert!(ui.history.is_empty());
    }

    /// A clean directory of its own, so tests never see each other's files.
    ///
    /// Uniquely suffixed: tests run in parallel, and two sharing a name raced
    /// between one's `remove_dir_all` and the other's `create_dir_all`.
    fn scratch(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("gmuse-ui-{name}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ui_with_history(store: &Store, name: &str) -> Ui {
        let mut ui = ui_of(store, true);
        ui.events_path = Some(crate::events::log_path(&scratch(name), "test"));
        ui
    }

    /// `:rate` acts on what is playing — you rate a song you are hearing.
    #[test]
    fn rating_applies_to_the_playing_track() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_with_history(&store, "playing");

        let ids: Vec<TrackId> = store.iter().map(|(id, _)| id).collect();
        let (playing, selected) = (ids[0], ids[1]);
        ui.playback.current = Some(playing);
        ui.view = View::Sorted;
        ui.sorted = ui
            .library
            .sorted()
            .iter()
            .position(|id| *id == selected)
            .unwrap();

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate 5");

        let played_path = &store.get(playing).unwrap().path;
        assert_eq!(ui.history.get(played_path).unwrap().rating, Some(5));
        assert!(
            ui.history
                .get(&store.get(selected).unwrap().path)
                .and_then(|e| e.rating)
                .is_none(),
            "it rated the selection instead of what was playing"
        );
    }

    /// With nothing playing it falls back to the cursor, so browsing and
    /// rating works too.
    #[test]
    fn rating_falls_back_to_the_selection() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_with_history(&store, "selection");
        ui.playback.current = None;
        ui.view = View::Sorted;
        ui.sorted = 0;
        let under_cursor = ui.library.sorted()[0];

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate 3");
        assert_eq!(
            ui.history
                .get(&store.get(under_cursor).unwrap().path)
                .unwrap()
                .rating,
            Some(3)
        );
    }

    /// A rating is written straight away — it is a judgement, and losing it to
    /// a crash would be worse than losing a play count.
    #[test]
    fn a_rating_is_saved_immediately() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_with_history(&store, "rating-saved");
        let dir = ui
            .events_path
            .clone()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let id = store.iter().next().unwrap().0;
        ui.playback.current = Some(id);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate 4");

        let (back, _) = crate::events::folded(&dir.join("history.toml"), &dir);
        assert_eq!(
            back.get(&store.get(id).unwrap().path).unwrap().rating,
            Some(4)
        );
    }

    /// Clearing is an event too, and `none` must fold back to unrated rather
    /// than to nought stars — §13 keeps those different questions.
    #[test]
    fn clearing_a_rating_folds_back_to_unrated() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_with_history(&store, "rating-cleared");
        let dir = ui
            .events_path
            .clone()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let id = store.iter().next().unwrap().0;
        ui.playback.current = Some(id);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate 4");
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate none");

        let (back, _) = crate::events::folded(&dir.join("history.toml"), &dir);
        assert_eq!(
            back.get(&store.get(id).unwrap().path).unwrap().rating,
            None,
            "the later `rate none` did not win"
        );
    }

    /// `:rate none` clears, and `:rate 0` is refused rather than doing it
    /// silently — one rule for zero across both languages.
    #[test]
    fn rate_none_clears_and_rate_zero_is_refused() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_with_history(&store, "clear");
        let id = store.iter().next().unwrap().0;
        let path = store.get(id).unwrap().path.clone();
        ui.playback.current = Some(id);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate 4");
        assert_eq!(ui.history.get(&path).unwrap().rating, Some(4));

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate 0");
        assert_eq!(
            ui.history.get(&path).unwrap().rating,
            Some(4),
            "`rate 0` should have been refused, not silently cleared"
        );
        assert!(ui.status.contains("rate none"), "{:?}", ui.status);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate none");
        assert_eq!(ui.history.get(&path).unwrap().rating, None);
    }

    /// A bare `:rate` reports rather than changing anything.
    #[test]
    fn a_bare_rate_reports_the_current_rating() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_with_history(&store, "report");
        let id = store.iter().next().unwrap().0;
        ui.playback.current = Some(id);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate");
        assert!(ui.status.contains("unrated"), "{:?}", ui.status);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate 2");
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate");
        assert!(ui.status.contains("2/5"), "{:?}", ui.status);
        assert_eq!(
            ui.history.get(&store.get(id).unwrap().path).unwrap().rating,
            Some(2),
            "reporting changed the rating"
        );
    }

    /// The arrows drive the menu too. Not the primary binding, but a menu
    /// that ignores them reads as broken — and since candidates run down the
    /// columns, `Down` is genuinely the next candidate.
    #[test]
    fn the_arrows_move_the_menu_like_ctrl_n_and_ctrl_p() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        let candidates = ui.prompt.as_ref().unwrap().candidates().to_vec();

        handle_key(KeyEvent::from(KeyCode::Down), &mut ui, &ctx);
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates[1].as_str())
        );
        handle_key(KeyEvent::from(KeyCode::Up), &mut ui, &ctx);
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates[0].as_str())
        );
        // Wrapping, the same as C-p.
        handle_key(KeyEvent::from(KeyCode::Up), &mut ui, &ctx);
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates.last().unwrap().as_str())
        );
        // And the line was not typed into.
        assert_eq!(ui.prompt.as_ref().unwrap().buffer, "");
    }

    /// A chord is not a keystroke. With no menu open, the keys that drive one
    /// must do nothing at all — before this they typed their own letters, so
    /// `C-n` `C-p` `C-y` on an empty `:` line left "npy" in the buffer.
    #[test]
    fn the_menu_chords_are_not_typed_into_the_line() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for k in ["^n", "^p", "^y", "^a"] {
            press(&mut ui, &ctx, k);
        }
        handle_key(KeyEvent::from(KeyCode::Down), &mut ui, &ctx);
        handle_key(KeyEvent::from(KeyCode::Up), &mut ui, &ctx);

        let prompt = ui.prompt.as_ref().expect("the line should still be open");
        assert_eq!(prompt.buffer, "", "a chord was typed into the line");
    }

    /// Cycling past the bottom of a capped menu must scroll it, or the choice
    /// moves somewhere the user cannot see.
    #[test]
    fn the_menu_scrolls_to_keep_the_selection_visible() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        let candidates = ui.prompt.as_ref().unwrap().candidates().to_vec();
        // Narrow, so the 63 verbs need far more rows than the menu can show.
        let (w, h) = (24u16, 24u16);

        for (n, expected) in candidates.iter().enumerate() {
            if n > 0 {
                press(&mut ui, &ctx, "^n");
            }
            assert_eq!(
                rendered_selection(&ui, &ctx, w, h).as_deref(),
                Some(expected.as_str()),
                "candidate {n} ({expected}) was selected but not on screen"
            );
        }
    }

    /// `C-y` replaces the word being typed and closes the menu.
    #[test]
    fn ctrl_y_accepts_the_highlighted_candidate() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        let second = ui.prompt.as_ref().unwrap().candidates()[1].clone();

        press(&mut ui, &ctx, "^n");
        press(&mut ui, &ctx, "^y");

        let prompt = ui.prompt.as_ref().expect("the line should still be open");
        assert_eq!(prompt.buffer, format!("{second} "));
        assert!(
            prompt.candidates().is_empty(),
            "the menu should be dismissed"
        );
        assert_eq!(rendered_selection(&ui, &ctx, 100, 20), None);
    }

    /// `C-y` straight after `Tab` takes the first candidate — and the menu
    /// says so, rather than leaving the key's effect to be discovered.
    #[test]
    fn ctrl_y_without_moving_takes_the_highlighted_first_candidate() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        let first = ui.prompt.as_ref().unwrap().candidates()[0].clone();
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(first.as_str()),
            "the candidate C-y would take must be visibly highlighted"
        );

        press(&mut ui, &ctx, "^y");
        assert_eq!(ui.prompt.as_ref().unwrap().buffer, format!("{first} "));
    }

    /// `C-y` on a `set key=value` completion replaces the value, not the whole
    /// word — the split is inside the word rather than at a space.
    #[test]
    fn ctrl_y_completes_the_value_half_of_a_setting() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "set aaa_mode=".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        let first = ui.prompt.as_ref().unwrap().candidates()[0].clone();

        press(&mut ui, &ctx, "^y");
        assert_eq!(
            ui.prompt.as_ref().unwrap().buffer,
            format!("set aaa_mode={first} ")
        );
    }

    /// Esc closes the menu first and the line second, so dismissing a list of
    /// possibilities does not throw away what has been typed.
    #[test]
    fn esc_dismisses_the_menu_before_the_line() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "sea".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        let typed = ui.prompt.as_ref().unwrap().buffer.clone();
        assert!(!ui.prompt.as_ref().unwrap().candidates().is_empty());

        handle_key(KeyEvent::from(KeyCode::Esc), &mut ui, &ctx);
        let prompt = ui.prompt.as_ref().expect("the line should survive");
        assert!(prompt.candidates().is_empty(), "the menu should be gone");
        assert_eq!(prompt.buffer, typed, "the line should be untouched");

        handle_key(KeyEvent::from(KeyCode::Esc), &mut ui, &ctx);
        assert!(ui.prompt.is_none(), "a second Esc should close the line");
    }

    /// With a menu open Tab cycles it, as vim's wildmenu does — re-completing
    /// would be a no-op against an unchanged candidate set.
    #[test]
    fn tab_cycles_once_the_menu_is_open() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        let candidates = ui.prompt.as_ref().unwrap().candidates().to_vec();

        tab(&mut ui, &ctx);
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates[1].as_str())
        );
        tab(&mut ui, &ctx);
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(candidates[2].as_str())
        );
    }

    /// Typing dismisses the menu, so the keys that drive it stop applying and
    /// go back to being text.
    #[test]
    fn narrowing_moves_the_selection_to_the_top() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        press(&mut ui, &ctx, "^n");
        assert!(rendered_selection(&ui, &ctx, 100, 20).is_some());

        // Typing narrows rather than dismissing, so the selection moves back
        // to the top of whatever survived rather than pointing at a candidate
        // that is no longer there.
        handle_key(KeyEvent::from(KeyCode::Char('s')), &mut ui, &ctx);
        let prompt = ui.prompt.as_ref().unwrap();
        assert_eq!(prompt.buffer, "s");
        assert_eq!(prompt.selected, 0, "a stale selection survived");
        let first = prompt.candidates()[0].clone();
        assert_eq!(
            rendered_selection(&ui, &ctx, 100, 20).as_deref(),
            Some(first.as_str())
        );
    }

    #[test]
    fn tab_completes_a_verb() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "qui".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "quit ");
        // And it did not run: Tab completes, Enter commits.
        assert!(!ui.quit);
    }

    #[test]
    fn tab_lists_the_options_when_ambiguous() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "pan".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "pane-", "extended past the divergence");

        let out = render(&ui, &ctx, 100, 20);
        assert!(out.contains("pane-next"), "{out}");
        assert!(out.contains("pane-left"), "{out}");
        assert!(out.contains("completions"), "no menu drawn:\n{out}");
    }

    /// `:` then Tab is the "what can I even type" case, and 63 verbs joined
    /// into a status line was useless.
    #[test]
    fn tab_on_an_empty_line_shows_a_menu() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);

        let count = ui.prompt.as_ref().unwrap().candidates().len();
        assert_eq!(count, crate::cmd::parse::VERBS.len());

        // Narrow, so the vocabulary cannot fit however small it gets — the
        // cap is what is under test, not how many verbs there happen to be.
        let out = render(&ui, &ctx, 60, 20);
        // Derived, not spelled out: a hardcoded count breaks whenever the
        // vocabulary changes, which says nothing about the menu.
        let title = format!("{} completions", crate::cmd::parse::VERBS.len());
        assert!(out.contains(&title), "{out}");
        // Laid out in columns, so several verbs share a line. Derived, like
        // the count: naming a verb made this fail when another was added and
        // pushed that one into the hidden remainder.
        let first = crate::cmd::parse::VERBS[0];
        assert!(out.contains(first), "{first} missing:\n{out}");
        assert!(
            out.lines().any(|l| l.matches("  ").count() > 1),
            "not in columns:\n{out}"
        );
        // Capped, with the remainder counted rather than silently dropped.
        assert!(out.contains("more"), "no overflow notice:\n{out}");
    }

    /// The two-step the report described: `c` extends, then Tab lists.
    #[test]
    fn extending_then_listing_is_two_presses() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "sea".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        // `search`, `search-next` and `search-prev` agree this far.
        assert_eq!(typed(&ui), "search");

        tab(&mut ui, &ctx);
        let candidates = &ui.prompt.as_ref().unwrap().candidates();
        assert!(candidates.contains(&"search".to_string()), "{candidates:?}");
        assert!(
            candidates.contains(&"search-next".to_string()),
            "{candidates:?}"
        );
    }

    #[test]
    fn typing_narrows_the_menu() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        let all = ui.prompt.as_ref().unwrap().candidates().len();
        assert!(all > 10);

        handle_key(KeyEvent::from(KeyCode::Char('s')), &mut ui, &ctx);
        let narrowed: Vec<String> = ui.prompt.as_ref().unwrap().candidates().to_vec();
        assert!(
            narrowed.len() < all && !narrowed.is_empty(),
            "typing should narrow, not dismiss: {narrowed:?}"
        );
        assert!(
            narrowed.iter().all(|c| c.starts_with('s')),
            "everything left should match what was typed: {narrowed:?}"
        );
        assert!(render(&ui, &ctx, 100, 20).contains("completions"));
    }

    /// The line is never rewritten by filtering. `Tab` is the only key that
    /// edits what you typed; narrowing the list must not silently complete it.
    #[test]
    fn narrowing_never_rewrites_the_line() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        // "sea" leaves search, search-next and search-prev — a common prefix
        // of "search", which `Tab` would apply and typing must not.
        for c in "sea".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        assert_eq!(ui.prompt.as_ref().unwrap().buffer, "sea");
        assert!(ui.prompt.as_ref().unwrap().candidates().len() >= 3);
    }

    /// Typing past the last match leaves the menu **open**, saying so. A list
    /// that vanished would read as a swallowed keystroke.
    #[test]
    fn no_matches_keeps_the_menu_open_and_says_so() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        for c in "zzz".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        let prompt = ui.prompt.as_ref().unwrap();
        assert!(
            prompt.menu.is_some(),
            "the menu closed instead of saying so"
        );
        assert!(prompt.candidates().is_empty());

        let out = render(&ui, &ctx, 100, 20);
        assert!(out.contains("no matches"), "{out}");

        // And the line is still executable — `:seek +30` has no candidates for
        // its argument either, and must still run.
        assert_eq!(prompt.buffer, "zzz");
    }

    /// `set key=value` is one word whose halves complete against different
    /// things, so filtering has to make the same split inside the word that
    /// completion does — otherwise typing a value narrows against verbs.
    #[test]
    fn narrowing_works_inside_a_setting_value() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "set aaa_mode=".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        let all: Vec<String> = ui.prompt.as_ref().unwrap().candidates().to_vec();
        // Tab extended to the common prefix of all/artist/album first.
        assert_eq!(ui.prompt.as_ref().unwrap().buffer, "set aaa_mode=a");
        assert_eq!(all, vec!["all", "artist", "album"]);

        handle_key(KeyEvent::from(KeyCode::Char('l')), &mut ui, &ctx);
        let narrowed: Vec<String> = ui.prompt.as_ref().unwrap().candidates().to_vec();
        assert_eq!(
            narrowed,
            vec!["all".to_string(), "album".to_string()],
            "narrowed against verbs instead of the value half?"
        );

        handle_key(KeyEvent::from(KeyCode::Char('b')), &mut ui, &ctx);
        assert_eq!(
            ui.prompt.as_ref().unwrap().candidates(),
            ["album".to_string()]
        );
        assert_eq!(ui.prompt.as_ref().unwrap().buffer, "set aaa_mode=alb");
    }

    /// Backspace widens the list again. This is the payoff of deriving the
    /// candidates from the buffer: there is nothing to undo.
    #[test]
    fn backspace_brings_the_candidates_back() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        let all = ui.prompt.as_ref().unwrap().candidates().len();

        for c in "zzz".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        assert!(ui.prompt.as_ref().unwrap().candidates().is_empty());

        for _ in 0..3 {
            handle_key(KeyEvent::from(KeyCode::Backspace), &mut ui, &ctx);
        }
        assert_eq!(
            ui.prompt.as_ref().unwrap().candidates().len(),
            all,
            "backspacing back to an empty line should offer everything again"
        );
    }

    #[test]
    fn a_unique_completion_shows_no_menu() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "qui".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "quit ");
        // A menu of one would be noise: it is already applied.
        assert!(ui.prompt.as_ref().unwrap().candidates().is_empty());
    }

    #[test]
    fn the_menu_is_capped_and_says_how_many_were_hidden() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        tab(&mut ui, &ctx);
        // Narrow, so every verb needs its own row and they cannot all fit.
        let out = render(&ui, &ctx, 30, 20);
        assert!(out.contains("more"), "no overflow notice:\n{out}");
    }

    /// Completion has to see the catalogs the application actually has.
    #[test]
    fn tab_completes_a_catalog_name() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);

        press(&mut ui, &ctx, ":");
        for c in "catalog pa".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "catalog paak ");
    }

    #[test]
    fn tab_completes_a_setting_value_after_the_equals() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "set aaa_mode=ar".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "set aaa_mode=artist");

        // And the completed line still parses and runs.
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
        assert_eq!(ui.playback.aaa_mode, crate::next::AaaMode::Artist);
    }

    #[test]
    fn tab_completes_a_nested_command_after_bind() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        // `pau`, not `pla`: `play` and `playlist` share a prefix, so `pla`
        // completes to the common stem and this would stop testing that a
        // *unique* match appends its space.
        for c in "bind common Z pau".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "bind common Z pause ");
    }

    #[test]
    fn tab_says_so_when_nothing_matches() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        for c in "frobni".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        // The typed text survives: completion must never eat what you wrote.
        assert_eq!(typed(&ui), "frobni");
        assert!(ui.status.contains("no completion"), "{}", ui.status);
    }

    #[test]
    fn tab_does_nothing_in_the_search_prompt() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "/");
        for c in "bur".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        // There is no closed vocabulary to complete a search against.
        assert_eq!(typed(&ui), "bur");
    }
    /// Rows as `(text, any cell bold)`.
    ///
    /// Cells are indexed by character, not byte — the `│` border alone is
    /// three bytes in UTF-8, so a `str::find` offset is not a column.
    fn rows_with_bold(ui: &Ui, ctx: &Context<'_>, w: u16, h: u16) -> Vec<(String, bool)> {
        use ratatui::style::Modifier;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, ui, ctx)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                let text: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect();
                let bold =
                    (0..buf.area.width).any(|x| buf[(x, y)].modifier.contains(Modifier::BOLD));
                (text.trim_end().to_string(), bold)
            })
            .collect()
    }

    /// Spotting what is playing without reading the status bar — the point of
    /// the whole thing during a long listen.
    #[test]
    fn the_playing_track_is_marked_in_the_list() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        let playing = ui.library.sorted()[2];
        ui.playback.current = Some(playing);
        // Cursor elsewhere, so the mark cannot be the selection highlight.
        ui.sorted = 0;

        let title = store.get(playing).unwrap().display_title().to_string();
        let rows = rows_with_bold(&ui, &ctx, 90, 20);
        let row = rows
            .iter()
            .find(|(text, _)| text.contains(&title))
            .unwrap_or_else(|| panic!("the playing track was not drawn:\n{rows:#?}"));
        assert!(row.1, "the playing track is not emphasised: {:?}", row.0);
    }

    /// Counted rather than checked row by row, because the *selected* row is
    /// bold too — the selection style says so. What the mark adds is one more
    /// emphasised track, and that is what this measures.
    #[test]
    fn nothing_playing_means_no_extra_emphasis() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 0;

        let bold_tracks = |ui: &Ui| {
            rows_with_bold(ui, &ctx, 90, 20)
                .into_iter()
                .filter(|(text, bold)| *bold && text.starts_with('│') && !text.contains("──"))
                .count()
        };

        ui.playback.current = None;
        assert_eq!(
            bold_tracks(&ui),
            1,
            "only the cursor row should be emphasised"
        );

        ui.playback.current = Some(ui.library.sorted()[2]);
        assert_eq!(
            bold_tracks(&ui),
            2,
            "the playing track should add exactly one more"
        );
    }

    /// A track can be in the library, a playlist and the queue at once, and
    /// should be marked wherever it appears.
    #[test]
    fn the_mark_follows_the_track_into_other_lists() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        let playing = ui.library.sorted()[1];
        ui.playlist.push(playing);
        ui.playback.current = Some(playing);
        ui.view = View::Playlist;

        let title = store.get(playing).unwrap().display_title().to_string();
        let rows = rows_with_bold(&ui, &ctx, 90, 12);
        let row = rows
            .iter()
            .find(|(text, _)| text.contains(&title))
            .expect("not drawn in the playlist");
        assert!(row.1, "not marked in the playlist: {:?}", row.0);
    }
    /// Colours of the row the cursor is on, as actually rendered.
    fn cursor_row_colours(
        ui: &Ui,
        ctx: &Context<'_>,
        needle: &str,
    ) -> (ratatui::style::Color, ratatui::style::Color) {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(90, 20)).unwrap();
        terminal.draw(|f| draw(f, ui, ctx)).unwrap();
        let buf = terminal.backend().buffer().clone();
        for y in 0..buf.area.height {
            let text: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if text.contains(needle) {
                // A cell inside the title, not the border.
                let x = (0..buf.area.width)
                    .find(|x| buf[(*x, y)].symbol() != " " && buf[(*x, y)].symbol() != "│")
                    .unwrap_or(1);
                let cell = &buf[(x, y)];
                return (cell.fg, cell.bg);
            }
        }
        panic!("row containing {needle:?} was not drawn");
    }

    /// The cursor landing on the playing track must not hide which track is
    /// playing — the selection colour would otherwise mask it.
    #[test]
    fn the_cursor_keeps_the_playing_colour() {
        use ratatui::style::Color;
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        let playing = ui.library.sorted()[1];
        let title = store.get(playing).unwrap().display_title().to_string();
        ui.playback.current = Some(playing);

        // Cursor elsewhere: ordinary selection colour on that other row.
        ui.sorted = 0;
        let other = store
            .get(ui.library.sorted()[0])
            .unwrap()
            .display_title()
            .to_string();
        let (fg, bg) = cursor_row_colours(&ui, &ctx, &other);
        assert_eq!(fg, Color::Magenta, "the normal selection changed");

        // Cursor on the playing track: green kept, surface kept.
        ui.sorted = 1;
        let (fg2, bg2) = cursor_row_colours(&ui, &ctx, &title);
        assert_eq!(fg2, Color::Green, "the cursor masked the playing mark");
        assert_eq!(bg2, bg, "the selection surface should be unchanged");
    }

    /// A pane that is not focused still shows where its cursor is, dimmed —
    /// otherwise coming back to it is a guess. View 3 is the two-pane one now.
    #[test]
    fn an_unfocused_pane_keeps_its_dim_selection() {
        use ratatui::style::Color;
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let (mut ui, _) = ui_with_saved(&store, "dim-pane");
        new_playlist(&mut ui, &ctx, "mix");
        for (id, _) in store.iter() {
            ui.playlist.push(id);
        }
        ui.view = View::Playlist;
        ui.playlist_pane = Pane::Tree;

        let title = store
            .get(ui.playlist.entries()[0])
            .unwrap()
            .display_title()
            .to_string();
        let (fg, _) = cursor_row_colours(&ui, &ctx, &title);
        assert_ne!(fg, Color::Green, "an unfocused pane looked focused");
    }

    /// A list longer than the pane, so the cursor has somewhere to be other
    /// than the edge.
    fn tall_fixture(n: usize) -> Store {
        let mut store = Store::new();
        for i in 0..n {
            store.upsert(Track {
                path: PathBuf::from(format!("/m/tall/{i:03}.mp3")),
                artist: Some("Tall".into()),
                album: Some("Album".into()),
                title: Some(format!("Track {i:03}")),
                tracknumber: Some(i as u32 + 1),
                duration: Duration::from_secs(100),
                ..Default::default()
            });
        }
        store
    }

    /// `G` then `k` used to drag the whole view up with the cursor pinned to
    /// the bottom line, because a fresh `ListState` starts at offset zero and
    /// ratatui then scrolls the least it can to reach the selection.
    #[test]
    fn k_after_g_walks_up_the_page_rather_than_dragging_the_view() {
        let store = tall_fixture(40);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.album_headers = false;

        press(&mut ui, &ctx, "G");
        let at_bottom = render(&ui, &ctx, 90, 12);
        assert!(at_bottom.contains("Track 039"), "G should reach the end");

        press(&mut ui, &ctx, "k");
        let after = render(&ui, &ctx, 90, 12);
        assert!(
            after.contains("Track 039"),
            "the last row scrolled off, so the cursor never left the bottom line:\n{after}"
        );
        assert!(after.contains("Track 038"), "and the cursor is on 038");
    }

    /// The other direction still works, which is what made the bug hard to
    /// see: from the top the view only starts moving once the cursor reaches
    /// the bottom, and that part was always right.
    #[test]
    fn j_from_the_top_fills_the_page_before_scrolling() {
        let store = tall_fixture(40);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.album_headers = false;

        press(&mut ui, &ctx, "g g");
        press(&mut ui, &ctx, "j");
        let after = render(&ui, &ctx, 90, 12);
        assert!(
            after.contains("Track 000"),
            "one j should not have scrolled anything:\n{after}"
        );
    }

    /// Each list keeps its own place, or stepping to another view and back
    /// would land you somewhere you never scrolled to.
    #[test]
    fn each_view_remembers_its_own_scroll() {
        let store = tall_fixture(40);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.album_headers = false;

        press(&mut ui, &ctx, "G");
        let _ = render(&ui, &ctx, 90, 12);

        ui.view = View::Queue;
        let queue = render(&ui, &ctx, 90, 12);
        assert!(!queue.contains("Track 039"), "the queue is empty:\n{queue}");

        ui.view = View::Sorted;
        let back = render(&ui, &ctx, 90, 12);
        assert!(
            back.contains("Track 039"),
            "view 2 should still be where it was left:\n{back}"
        );
    }

    /// Remembering the offset means it can outlive the rows it referred to —
    /// a fold closing, a filter narrowing, `dd` on the last entry. ratatui
    /// clamps, and this says so rather than trusting it.
    #[test]
    fn a_scroll_left_past_the_end_of_a_shrunken_list_still_draws() {
        let store = tall_fixture(40);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.album_headers = false;
        ui.sorted = 0;
        ui.sorted_scroll.scroll_to(9_999);

        let out = render(&ui, &ctx, 90, 12);
        assert!(
            out.contains("Track 000"),
            "a stale offset should clamp, not blank the pane:\n{out}"
        );
    }
    /// Its own store rather than the shared fixture: adding a row there moves
    /// every count and rendering assertion in the file, and this needs exactly
    /// two tracks to say what it means.
    fn marked_fixture() -> Store {
        let mut store = Store::new();
        store.upsert(Track {
            path: PathBuf::from("/m/loffler/01.mp3"),
            artist: Some("Christian Löffler".into()),
            album: Some("A Life ⧸ Young Alaska".into()),
            title: Some("Nothing Is Real".into()),
            tracknumber: Some(1),
            duration: Duration::from_secs(267),
            ..Default::default()
        });
        store.upsert(Track {
            path: PathBuf::from("/m/aaa/01.mp3"),
            artist: Some("Aaa Ordinary".into()),
            album: Some("Plain".into()),
            title: Some("First".into()),
            tracknumber: Some(1),
            duration: Duration::from_secs(100),
            ..Default::default()
        });
        store
    }

    /// The whole point, end to end: `/` finds him without the umlaut, and
    /// spelled out the way German spells it when the key is missing.
    #[test]
    fn search_finds_an_umlaut_however_it_is_typed() {
        let store = marked_fixture();
        let ctx = ctx_of(&store);

        for typed in ["loffler", "loeffler", "löffler", "Loffler"] {
            let mut ui = ui_of(&store, true);
            ui.view = View::Sorted;
            ui.sorted = 0;
            press(&mut ui, &ctx, "/");
            type_line(&mut ui, &ctx, typed);

            let landed = ui.library.sorted()[ui.sorted];
            let artist = store.get(landed).map(|t| t.file_under().to_string());
            assert_eq!(
                artist.as_deref(),
                Some("Christian Löffler"),
                "/{typed} landed somewhere else"
            );
        }
    }

    /// The tag holds `/`; the filename holds `⧸`, because Windows forbids the
    /// first. Searching has to put them back together.
    #[test]
    fn search_crosses_the_fullwidth_stand_ins() {
        let store = marked_fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 0;

        press(&mut ui, &ctx, "/");
        type_line(&mut ui, &ctx, "a life / young");

        let landed = ui.library.sorted()[ui.sorted];
        assert_eq!(
            store.get(landed).and_then(|t| t.album.clone()).as_deref(),
            Some("A Life ⧸ Young Alaska")
        );
    }

    /// Folding must not quietly become fuzzy matching — a real miss still
    /// misses, and says so.
    #[test]
    fn a_search_that_should_miss_still_misses() {
        let store = marked_fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 0;

        press(&mut ui, &ctx, "/");
        type_line(&mut ui, &ctx, "lieffler");
        assert_eq!(ui.sorted, 0, "nothing should have matched");
    }

    /// A library deep enough that a `zr` inserts many rows above the cursor.
    fn many_artists(n: usize) -> Store {
        let mut store = Store::new();
        for i in 0..n {
            for t in 1..=2u32 {
                store.upsert(Track {
                    path: PathBuf::from(format!("/m/{i:03}/{t}.mp3")),
                    artist: Some(format!("Artist {i:03}")),
                    album: Some(format!("Album {i:03}")),
                    title: Some(format!("Track {t}")),
                    tracknumber: Some(t),
                    duration: Duration::from_secs(100),
                    ..Default::default()
                });
            }
        }
        store
    }

    /// `zr` should open the tree *behind* the cursor: the row you are looking
    /// at stays where it is, and the new rows appear around it. Keeping the
    /// node without keeping the row makes the cursor jump down the screen by
    /// however many albums were inserted above it.
    ///
    /// Far enough down that the view is **already scrolled** — with the
    /// viewport at row zero the screen row and the list index are the same
    /// number, and a test cannot tell which one the code used.
    #[test]
    fn zr_holds_the_cursor_still_on_screen() {
        let store = many_artists(20);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "1 5 G");
        let before = render(&ui, &ctx, 90, 14);
        assert!(
            ui.tree_scroll.top() > 0,
            "the view should be scrolled:\n{before}"
        );
        let was = tree_cursor_row(&ui);

        press(&mut ui, &ctx, "z r");
        let after = render(&ui, &ctx, 90, 14);
        assert_eq!(
            tree_cursor_row(&ui),
            was,
            "the cursor moved down the screen as the tree opened:\nbefore:\n{before}\nafter:\n{after}"
        );
        assert!(
            after.contains("Artist 014"),
            "and it is still on the row it was on:\n{after}"
        );
    }

    /// Where the tree's cursor sits on screen, after the frame that decided
    /// it — ratatui clamps the offset at render, so reading it before would be
    /// reading a request rather than a result.
    fn tree_cursor_row(ui: &Ui) -> usize {
        ui.tree.selected.saturating_sub(ui.tree_scroll.top())
    }

    /// And the same coming back. Deep enough into the list that there *are*
    /// enough rows above the cursor to hold it where it was — near the top
    /// there are not, which is the next test.
    #[test]
    fn zm_holds_the_cursor_still_on_screen() {
        let store = many_artists(20);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z r");
        press(&mut ui, &ctx, "3 0 G");
        let before = render(&ui, &ctx, 90, 14);
        let was = tree_cursor_row(&ui);

        press(&mut ui, &ctx, "z m");
        let after = render(&ui, &ctx, 90, 14);
        assert_eq!(
            tree_cursor_row(&ui),
            was,
            "the cursor moved as the tree closed:\nbefore:\n{before}\nafter:\n{after}"
        );
    }

    /// Near the top of the tree the request is impossible: collapsing leaves
    /// fewer rows above the cursor than the screen row it wants, and there is
    /// no negative offset. It rides as high as it can rather than scrolling
    /// past the start.
    #[test]
    fn zm_near_the_top_settles_for_the_top() {
        let store = many_artists(20);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z r");
        press(&mut ui, &ctx, "1 0 G");
        let _ = render(&ui, &ctx, 90, 14);

        press(&mut ui, &ctx, "z m");
        let after = render(&ui, &ctx, 90, 14);
        assert_eq!(ui.tree_scroll.top(), 0, "should be showing the start");
        assert!(
            after.contains("Artist 000"),
            "and the first row with it:\n{after}"
        );
    }

    /// `zo` acts on the node under the cursor, so everything it changes is
    /// below — the row must not be dragged to compensate for nothing.
    #[test]
    fn zo_leaves_the_cursor_where_it_is() {
        let store = many_artists(20);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "1 5 G");
        let before = render(&ui, &ctx, 90, 14);
        let was = tree_cursor_row(&ui);

        press(&mut ui, &ctx, "z o");
        let after = render(&ui, &ctx, 90, 14);
        assert_eq!(
            tree_cursor_row(&ui),
            was,
            "nothing above the cursor changed, so nothing should have moved:\nbefore:\n{before}\nafter:\n{after}"
        );
    }

    /// The reason `levels()` exists rather than anchoring everything. `zc` on
    /// something already closed walks up to the parent, and the cursor should
    /// *move up the screen* to it — as it does in vim, and as `h` has always
    /// done here. Anchoring would drag the whole view down to keep the parent
    /// where the child had been.
    #[test]
    fn zc_walking_up_moves_the_cursor_rather_than_the_view() {
        let store = many_artists(20);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z r");
        press(&mut ui, &ctx, "3 0 G");
        let before = render(&ui, &ctx, 90, 14);
        let was = tree_cursor_row(&ui);
        let top = ui.tree_scroll.top();

        // On a closed album, so `zc` closes the artist above it and moves there.
        press(&mut ui, &ctx, "z c");
        let after = render(&ui, &ctx, 90, 14);
        assert_eq!(
            ui.tree_scroll.top(),
            top,
            "the view should not have scrolled:\nbefore:\n{before}\nafter:\n{after}"
        );
        assert!(
            tree_cursor_row(&ui) < was,
            "the cursor should have walked up the screen, not stayed put"
        );
    }

    /// The point of the `~` filler. Collapsing near the *end* of the tree
    /// leaves fewer rows below the cursor than the pane can show, and without
    /// somewhere to put the shortfall the list stops scrolling and the cursor
    /// drops to the last line — however carefully the fold code held it.
    #[test]
    fn zm_near_the_end_holds_the_cursor_and_shows_filler() {
        let store = many_artists(20);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        // `G` then back up the page, so the cursor sits *high* on a view that
        // is scrolled a long way down. Collapsing from there wants a viewport
        // that runs past the end of the tree.
        press(&mut ui, &ctx, "z r");
        press(&mut ui, &ctx, "G");
        // A frame between the two, because the offset is a property of the
        // last render — which is exactly what makes `k` walk up the page
        // rather than drag the view.
        let _ = render(&ui, &ctx, 90, 14);
        press(&mut ui, &ctx, "5 k");
        let before = render(&ui, &ctx, 90, 14);
        let was = tree_cursor_row(&ui);

        press(&mut ui, &ctx, "z m");
        let after = render(&ui, &ctx, 90, 14);
        assert_eq!(
            tree_cursor_row(&ui),
            was,
            "the cursor slid down the screen as the tree ran out:\nbefore:\n{before}\nafter:\n{after}"
        );
        assert!(
            after.contains('~'),
            "and the shortfall should be drawn as filler:\n{after}"
        );
    }

    /// Filler is what is *past* the list, never instead of it: scrolling can
    /// leave the last row at the top but no further.
    #[test]
    fn a_pane_never_shows_only_filler() {
        let store = many_artists(20);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;
        ui.tree.selected = 19;
        ui.tree_scroll.scroll_to(9_999);

        let out = render(&ui, &ctx, 90, 14);
        assert!(
            out.contains("Artist 019"),
            "the last row should still be on screen:\n{out}"
        );
        assert!(
            !out.contains("Artist 018"),
            "and it should be at the top, with filler under it:\n{out}"
        );
    }

    /// A list shorter than its pane is the ordinary case, and it should look
    /// like vim rather than like a list with a lot of blank rows after it.
    #[test]
    fn a_short_list_fills_the_rest_of_the_pane() {
        let store = many_artists(2);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        let out = render(&ui, &ctx, 90, 14);
        assert!(out.contains('~'), "expected filler rows:\n{out}");
    }

    /// The clamp in `state_within` is not about what you see — ratatui scrolls
    /// back to the selection regardless — it is about not asking `pad_to_view`
    /// for ten thousand filler rows.
    #[test]
    fn a_stale_offset_is_clamped_before_it_pads_a_pane() {
        let scroll = Scroll::default();
        scroll.scroll_to(9_999);
        let state = scroll.state_within(Some(0), 20, 10);
        assert_eq!(state.offset(), 19, "the last row is as far as it may go");
        assert_eq!(scroll.top(), 19, "and the clamp is written back");
    }

    /// Pressing a prefix should say what can follow it, without being asked.
    #[test]
    fn a_prefix_key_offers_what_comes_next() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z");
        waited(&mut ui);
        let out = render(&ui, &ctx, 100, 20);
        for expect in ["o  fold open", "r  fold more", "M  fold none"] {
            assert!(out.contains(expect), "expected {expect:?} in:\n{out}");
        }
    }

    /// And the leader, which is the other prefix worth having one for.
    #[test]
    fn the_leader_offers_what_comes_next() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "<leader>");
        waited(&mut ui);
        let out = render(&ui, &ctx, 100, 20);
        assert!(out.contains("c  pause"), "{out}");
        assert!(out.contains("?  help"), "{out}");
    }

    /// It is derived, not stored, so a `:bind` shows up in it at once.
    #[test]
    fn a_new_binding_appears_in_the_panel_immediately() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind common \"z 9\" quit");
        press(&mut ui, &ctx, "z");
        waited(&mut ui);
        let out = render(&ui, &ctx, 100, 20);
        assert!(out.contains("9  quit"), "{out}");
    }

    /// The delay itself, as a rule rather than as a rendering.
    #[test]
    fn the_popup_waits_and_then_is_due() {
        let start = Instant::now();
        let delay = Duration::from_millis(300);

        assert!(
            !which_key_due(Some(start), start, delay),
            "due the instant the prefix was pressed"
        );
        assert!(
            !which_key_due(Some(start), start + Duration::from_millis(299), delay),
            "due a millisecond early"
        );
        assert!(which_key_due(Some(start), start + delay, delay));
        assert!(
            !which_key_due(None, start + delay, delay),
            "due with no chord pending"
        );
        assert!(
            which_key_due(Some(start), start, Duration::ZERO),
            "a zero delay should restore the old behaviour exactly"
        );
    }

    /// The whole point: a chord typed at speed never draws the panel.
    #[test]
    fn a_chord_finished_before_the_delay_never_shows_the_panel() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z");
        assert!(
            !render(&ui, &ctx, 100, 20).contains("fold open"),
            "the panel appeared before its delay had passed"
        );
        press(&mut ui, &ctx, "a");
        assert!(ui.pending.is_empty(), "the chord did not complete");
    }

    /// which-key charges the delay once per chord, not once per key: going
    /// deeper redraws immediately. Held here by keeping the *first* key's
    /// instant, so this is really a test that the second key does not reset it.
    #[test]
    fn going_deeper_does_not_pay_the_delay_again() {
        let mut bindings = bind::defaults();
        let seq = crate::cmd::keys::parse_seq("g q x").expect("test keys");
        bindings.insert((crate::cmd::Context::Common, seq), Command::Quit);

        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.bindings = bindings;
        ui.view = View::Tree;

        press(&mut ui, &ctx, "g");
        waited(&mut ui);
        let before = ui.pending_since.expect("a chord is pending");
        press(&mut ui, &ctx, "q");

        assert_eq!(
            ui.pending_since,
            Some(before),
            "the second key restarted the clock"
        );
        assert!(
            render(&ui, &ctx, 100, 20).contains("x  quit"),
            "going deeper made the popup wait again"
        );
    }

    /// The loop must not spin once the popup is up, and must not oversleep
    /// while it is waiting.
    #[test]
    fn the_tick_shortens_for_a_pending_prefix_and_no_further() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        let full = Duration::from_millis(200);

        let now = Instant::now();
        assert_eq!(ui.tick(now), full, "nothing pending, so the ordinary tick");

        press(&mut ui, &ctx, "z");
        let started = ui.pending_since.expect("a chord is pending");
        assert_eq!(
            ui.tick(started + Duration::from_millis(150)),
            Duration::from_millis(150),
            "it should wake exactly when the delay expires"
        );
        assert_eq!(
            ui.tick(started + Duration::from_millis(1)),
            full,
            "a delay longer than the tick should not stretch it"
        );
        assert_eq!(
            ui.tick(started + Duration::from_secs(9)),
            full,
            "a due popup should not spin the loop on a zero timeout"
        );
    }

    /// The complaint in #42: the rows past the cap were unreachable. `<leader>?`
    /// lists every key that works in the view, which is far more than the
    /// popup's ten-row cap holds.
    #[test]
    fn the_help_popup_scrolls_to_what_it_could_not_show() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "<leader> ?");
        let first = render(&ui, &ctx, 80, 24);
        assert!(
            first.contains("^d ^u scroll"),
            "no scroll affordance on an overflowing popup:\n{first}"
        );
        assert_eq!(ui.help_scroll, 0, "it should open at the top");

        press(&mut ui, &ctx, "^d");
        assert!(
            ui.help.is_some(),
            "^d dismissed the popup instead of scrolling"
        );
        assert!(ui.help_scroll > 0, "^d did not scroll");
        let second = render(&ui, &ctx, 80, 24);
        assert_ne!(
            first, second,
            "the popup drew the same rows after scrolling"
        );

        press(&mut ui, &ctx, "^u");
        assert_eq!(ui.help_scroll, 0, "^u did not come back");
    }

    /// The exception is exactly as wide as the affordance: a popup that fits
    /// shows no hint and does not take the key either.
    ///
    /// What `^d` does *instead* here is what it has always done mid-chord —
    /// no binding continues a prefix with a control key, so `resolve` finds
    /// nothing and the chord is abandoned. That is unchanged, and the point of
    /// the test is that the popup does not quietly swallow the press on top of
    /// it.
    #[test]
    fn a_popup_that_fits_does_not_take_the_key() {
        let store = big_fixture(200);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        let _ = render(&ui, &ctx, 100, 24);

        // `d` has one continuation, so its popup cannot overflow.
        press(&mut ui, &ctx, "d");
        waited(&mut ui);
        let _ = render(&ui, &ctx, 100, 24);
        assert_eq!(ui.help_view.get().0, ui.help_view.get().1, "it overflowed");

        press(&mut ui, &ctx, "^d");
        assert_eq!(ui.help_scroll, 0, "a popup that fits scrolled");
        assert!(
            ui.pending.is_empty(),
            "the chord should have been abandoned"
        );
    }

    /// which-key's rule: the hint appears only when it is true (`view.lua:448`).
    /// A popup that fits advertises nothing, or the hint becomes furniture.
    #[test]
    fn a_popup_that_fits_offers_no_scroll_hint() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        // `d` has one continuation, so it cannot overflow at any width.
        press(&mut ui, &ctx, "d");
        waited(&mut ui);
        let out = render(&ui, &ctx, 100, 24);
        assert!(out.contains("d  remove"), "the panel is not up:\n{out}");
        assert!(
            !out.contains("scroll"),
            "a popup that fits should not mention scrolling:\n{out}"
        );
    }

    /// Scrolling stops at the end rather than running off it, and the footer
    /// says which end you are at.
    #[test]
    fn the_popup_stops_at_its_end() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "<leader> ?");
        let _ = render(&ui, &ctx, 80, 24);
        for _ in 0..40 {
            press(&mut ui, &ctx, "^d");
            let _ = render(&ui, &ctx, 80, 24);
        }
        let (visible, total) = ui.help_view.get();
        assert_eq!(
            ui.help_scroll,
            total - visible,
            "it scrolled past the last row"
        );
        assert!(
            render(&ui, &ctx, 80, 24).contains("… end"),
            "the footer should say there is nothing below"
        );
    }

    /// Going deeper into a chord shows a different list, so it opens at its
    /// top — otherwise the offset from the previous list lands somewhere
    /// arbitrary in this one.
    #[test]
    fn a_deeper_chord_opens_the_popup_at_its_top() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "<leader> ?");
        let _ = render(&ui, &ctx, 80, 24);
        press(&mut ui, &ctx, "^d");
        assert!(ui.help_scroll > 0);

        // Any key dismisses the help; then a prefix opens the other popup.
        press(&mut ui, &ctx, "escape");
        press(&mut ui, &ctx, "z");
        assert_eq!(ui.help_scroll, 0, "the offset survived into a new popup");
    }

    // ---- transport mode (#33) ---------------------------------------------

    // ---- incremental search (#55) -----------------------------------------

    /// vim's `incsearch`: the cursor moves as you type, before anything is
    /// committed.
    #[test]
    fn the_search_runs_as_you_type() {
        let store = big_fixture(12);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "/");
        assert_eq!(ui.sorted, 0);
        for c in "track 0007".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        assert_eq!(ui.sorted, 6, "the cursor did not follow what was typed");
        assert!(ui.prompt.is_some(), "nothing was committed yet");
    }

    /// Each keystroke refines **one** search from where the prompt opened,
    /// rather than starting a fresh one from wherever the last landed — so
    /// backspacing walks back exactly where typing went.
    #[test]
    fn deleting_a_character_walks_the_search_back() {
        let store = big_fixture(12);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "/");
        for c in "track".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        // Wherever `/track` alone lands. vim searches from *past* the cursor,
        // so that is the second row rather than the first — the property here
        // is the round trip, not the index.
        let with_prefix = ui.sorted;

        for c in " 0009".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        assert_eq!(ui.sorted, 8, "the longer pattern did not move it");

        for _ in 0..5 {
            handle_key(KeyEvent::from(KeyCode::Backspace), &mut ui, &ctx);
        }
        assert_eq!(
            ui.sorted, with_prefix,
            "backspacing did not walk back to where typing had gone"
        );
    }

    /// **The preview must be undoable.** vim moves the view while you type and
    /// puts you back if you cancel; only `Enter` commits. Without this an
    /// abandoned search is an unintentional jump.
    #[test]
    fn escape_puts_the_cursor_back_where_the_search_started() {
        let store = big_fixture(12);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 3;

        press(&mut ui, &ctx, "/");
        for c in "track 0009".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        assert_eq!(ui.sorted, 8, "the preview did not move");

        handle_key(KeyEvent::from(KeyCode::Esc), &mut ui, &ctx);
        assert_eq!(ui.sorted, 3, "escape did not restore the cursor");
        assert!(ui.highlight.is_none(), "escape left the highlight up");
    }

    /// And `Enter` keeps it, which is the whole difference between the two.
    #[test]
    fn enter_commits_where_the_preview_landed() {
        let store = big_fixture(12);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 3;

        press(&mut ui, &ctx, "/");
        for c in "track 0009".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
        assert_eq!(ui.sorted, 8, "enter did not keep the match");
        assert!(ui.highlight.is_some(), "the match stopped being painted");
    }

    /// Painted where it matched, and `<Esc>` stops painting without losing the
    /// search — vim's `:nohlsearch`, which the local nvim puts on `<Esc>`.
    #[test]
    fn the_match_is_painted_and_escape_stops_painting_it() {
        use ratatui::style::Color;
        let store = big_fixture(12);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "/");
        for c in "rack".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);

        let painted = |ui: &Ui| {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 12))
                .expect("a terminal");
            term.draw(|f| draw(f, ui, &ctx)).expect("a frame");
            let buf = term.backend().buffer().clone();
            (0..12u16)
                .flat_map(|y| (0..60u16).map(move |x| (x, y)))
                .filter(|(x, y)| buf[(*x, *y)].style().fg == Some(Color::Yellow))
                .count()
        };
        assert!(painted(&ui) >= 4, "the match was not painted");

        // `n` keeps it painted; `<Esc>` stops.
        press(&mut ui, &ctx, "n");
        assert!(painted(&ui) >= 4, "`n` should not clear the highlight");
        press(&mut ui, &ctx, "escape");
        assert_eq!(painted(&ui), 0, "`<Esc>` should have cleared it");
    }

    // ---- the up-next projection (#56) -------------------------------------

    /// View 4 shows what would play next, not just what was queued — the queue
    /// first, then the pool's own order, with a divider between them.
    #[test]
    fn view_four_projects_the_pool_after_the_queue() {
        let store = big_fixture(8);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Queue;
        ui.playback.current = ui.library.sorted().first().copied();
        ui.queue.push_back(ui.library.sorted()[5]);

        let out = render(&ui, &ctx, 62, 14);
        assert!(out.contains("up next"), "{out}");
        assert!(out.contains("── the pool, from here"), "no divider:\n{out}");
        // Seven rows follow the one queued: tracks 2..8 of eight.
        assert!(
            out.contains("Track 0002"),
            "the pool tail is missing:\n{out}"
        );
    }

    /// **A queued track does not move the pool cursor** (§5), so it genuinely
    /// plays twice — once from the queue and again when the pool reaches it.
    /// The projection shows that rather than hiding it, because hiding it
    /// would be showing a future that does not happen.
    #[test]
    fn a_queued_track_still_appears_in_the_tail() {
        let store = big_fixture(8);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Queue;
        ui.playback.current = ui.library.sorted().first().copied();
        let sixth = ui.library.sorted()[5];
        ui.queue.push_back(sixth);

        let rows = up_next(&ui, &ctx);
        let times = rows.iter().filter(|(id, _)| *id == sixth).count();
        assert_eq!(times, 2, "queued once and reached once: {rows:?}");
        assert_eq!(rows[0].1, crate::next::Upcoming::Queued);
    }

    /// The cursor ranges over the whole projection, but only the queued rows
    /// can be edited — the pool's order has no seed that would yield "this
    /// permutation with one track moved", so an edit there has nowhere to go.
    #[test]
    fn a_projected_row_refuses_to_be_removed() {
        let store = big_fixture(8);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Queue;
        ui.playback.current = ui.library.sorted().first().copied();
        ui.queue.push_back(ui.library.sorted()[5]);

        // Row 0 is queued and removable.
        ui.queue_sel = 0;
        press(&mut ui, &ctx, "d");
        press(&mut ui, &ctx, "d");
        assert_eq!(ui.queue.len(), 0, "a queued row should be removable");

        // Now everything is projection, and `dd` says so rather than eating a
        // row that was never the queue's.
        ui.queue_sel = 1;
        press(&mut ui, &ctx, "d");
        press(&mut ui, &ctx, "d");
        assert!(ui.status.contains("not a queued one"), "{}", ui.status);
    }

    /// `i` in, `escape` out, and the status line says which.    /// `i` in, `escape` out, and the status line says which.    /// `i` in, `escape` out, and the status line says which.
    #[test]
    fn i_enters_transport_mode_and_escape_leaves() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        assert_eq!(ui.mode, crate::cmd::Mode::Normal);
        assert!(!render(&ui, &ctx, 100, 24).contains("TRANSPORT"));

        press(&mut ui, &ctx, "i");
        assert_eq!(ui.mode, crate::cmd::Mode::Transport);
        assert!(
            render(&ui, &ctx, 100, 24).contains("-- TRANSPORT --"),
            "a mode you cannot see is a mode you press the wrong key in"
        );

        press(&mut ui, &ctx, "escape");
        assert_eq!(ui.mode, crate::cmd::Mode::Normal);
    }

    /// The panel rises with the mode and is not there without it — which is
    /// the whole of why it is not the "something to watch" §1 refuses.
    #[test]
    fn the_transport_panel_rises_with_the_mode() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;
        ui.playback.current = ui.library.sorted().first().copied();

        let browsing = render(&ui, &ctx, 84, 20);
        assert!(
            !browsing.contains("TRANSPORT"),
            "the panel is up while browsing:\n{browsing}"
        );

        press(&mut ui, &ctx, "i");
        let out = render(&ui, &ctx, 84, 20);
        assert!(
            out.contains("╔ -- TRANSPORT --"),
            "no panel in the mode:\n{out}"
        );
        assert!(out.contains("next  "), "it should say what is next:\n{out}");

        // Over the middle of the view, not along the bottom edge. A panel in
        // the bottom slot read as one more transient helper, and a mode you do
        // not notice is a mode you press navigation keys in.
        let row = out
            .lines()
            .position(|l| l.contains("╔ -- TRANSPORT --"))
            .expect("a top border");
        let rows = out.lines().count();
        assert!(
            row > 2 && row < rows - 4,
            "the panel is at row {row} of {rows}, which is not the middle"
        );

        press(&mut ui, &ctx, "escape");
        assert!(!render(&ui, &ctx, 84, 20).contains("╔ -- TRANSPORT --"));
    }

    /// Floating means it no longer competes with the bottom slot at all — and
    /// it **stays up while a prompt is open**, which the slot-based version
    /// could not do. Opening `:` does not leave the mode, so an indicator that
    /// vanished there would be missing at exactly the moment you are typing.
    #[test]
    fn the_panel_stays_up_while_a_prompt_is_open() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.playback.current = ui.library.sorted().first().copied();
        press(&mut ui, &ctx, "i");
        assert!(render(&ui, &ctx, 84, 20).contains("-- TRANSPORT -- "));

        press(&mut ui, &ctx, ":");
        let out = render(&ui, &ctx, 84, 20);
        assert!(
            out.contains("╔ -- TRANSPORT --"),
            "the panel went when the prompt opened:\n{out}"
        );
    }

    /// A panel that filled the pane would not read as floating *over* it, so
    /// below the margin it does not draw at all. The status line still carries
    /// the indicator, which is the part that may never be lost.
    #[test]
    fn the_panel_yields_when_there_is_no_room_to_float() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;
        press(&mut ui, &ctx, "i");

        for (w, h) in [(84u16, 8u16), (40, 20)] {
            let out = render(&ui, &ctx, w, h);
            assert!(!out.contains("╔"), "the panel drew at {w}x{h}:\n{out}");
            assert!(
                out.contains("-- TRANSPORT --"),
                "the indicator was lost at {w}x{h}:\n{out}"
            );
        }
    }

    /// The bar is a fraction of the pane, not of anything it invented.
    #[test]
    fn the_position_bar_fills_by_how_far_in_you_are() {
        let full = progress_bar(Duration::from_secs(50), Duration::from_secs(100), 10);
        assert_eq!(full.chars().filter(|c| *c == '━').count(), 5, "{full}");
        let start = progress_bar(Duration::ZERO, Duration::from_secs(100), 10);
        assert_eq!(start.chars().filter(|c| *c == '━').count(), 0, "{start}");
        let end = progress_bar(Duration::from_secs(100), Duration::from_secs(100), 10);
        assert_eq!(end.chars().filter(|c| *c == '━').count(), 10, "{end}");
        // A track with no duration must not divide by zero or overfill.
        let unknown = progress_bar(Duration::from_secs(9), Duration::ZERO, 10);
        assert_eq!(
            unknown.chars().filter(|c| *c == '━').count(),
            0,
            "{unknown}"
        );
        // Nor may a position past the end run off it.
        let over = progress_bar(Duration::from_secs(500), Duration::from_secs(100), 10);
        assert_eq!(over.chars().count(), 10, "{over}");
    }

    /// The indicator outranks a notice, because the four seconds after a
    /// message is exactly when you are least sure what the keyboard is doing.
    #[test]
    fn the_mode_shows_even_while_a_notice_is_up() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "i");
        ui.notify("something happened");

        let out = render(&ui, &ctx, 100, 24);
        assert!(out.contains("-- TRANSPORT --"), "{out}");
        assert!(out.contains("something happened"), "{out}");
    }

    /// **A mode is a keymap of its own, not a layer over the normal one.** If
    /// it fell through to `common`, `j` would still navigate in the mode where
    /// it is supposed to be volume — which is the whole thing the mode is for.
    #[test]
    fn transport_mode_does_not_inherit_navigation() {
        let store = big_fixture(50);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "j");
        assert_eq!(ui.sorted, 1, "j should navigate in normal mode");

        press(&mut ui, &ctx, "i");
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.sorted, 1, "j navigated in transport mode");
        press(&mut ui, &ctx, "g");
        press(&mut ui, &ctx, "g");
        assert_eq!(ui.sorted, 1, "gg reached through from common");
    }

    /// The same letters, bare — one alphabet reached from two postures.
    #[test]
    fn the_transport_letters_are_bare_in_the_mode() {
        for (normal, in_mode) in [
            ("<leader> c", "c"),
            ("<leader> b", "b"),
            ("<leader> x", "x"),
        ] {
            let bindings = bind::defaults();
            let normal_key = crate::cmd::keys::parse_seq(normal).expect("test keys");
            let mode_key = crate::cmd::keys::parse_seq(in_mode).expect("test keys");
            assert_eq!(
                bind::lookup(&bindings, crate::cmd::Context::Common, &normal_key),
                bind::lookup(&bindings, crate::cmd::Context::Transport, &mode_key),
                "{normal} and {in_mode} should be the same command"
            );
        }
    }

    /// A closed keymap has to carry its own way out, or the mode is a trap.
    #[test]
    fn the_mode_carries_its_own_exit_and_its_own_help() {
        let bindings = bind::defaults();
        for key in ["escape", "?"] {
            let seq = crate::cmd::keys::parse_seq(key).expect("test keys");
            assert!(
                bind::lookup(&bindings, crate::cmd::Context::Transport, &seq).is_some(),
                "{key} is not bound in transport mode"
            );
        }
    }

    /// `<leader>?` lists what is reachable. In a mode nothing falls through,
    /// so listing `common` would be listing keys that do nothing.
    #[test]
    fn the_help_popup_in_a_mode_lists_only_the_mode() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "i");
        press(&mut ui, &ctx, "?");

        assert!(
            render(&ui, &ctx, 100, 24).contains("transport — keys that work here"),
            "the popup should name the mode, not the view it is over"
        );
        let rows = ui.help.as_ref().expect("no help popup");
        assert!(rows.iter().any(|r| r.starts_with("h  seek")), "{rows:?}");
        assert!(
            !rows.iter().any(|r| r.contains("down")),
            "navigation leaked into the mode's help: {rows:?}"
        );
    }

    /// vim's `50%`: a count and `%` goes that far into the track, and bare it
    /// means halfway. The count is read exactly as `12G` reads it.
    #[test]
    fn a_count_and_percent_seeks_into_the_track() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "i");

        // No player in tests, so this asserts the command resolves and runs
        // rather than where the audio went — the seek itself is the player's.
        let seq = crate::cmd::keys::parse_seq("%").expect("test keys");
        assert_eq!(
            bind::lookup(&ui.bindings, crate::cmd::Context::Transport, &seq),
            Some(&Command::SeekPercent)
        );
        press(&mut ui, &ctx, "2");
        press(&mut ui, &ctx, "5");
        press(&mut ui, &ctx, "%");
        assert!(ui.count.is_none(), "the count outlived the command");
    }

    /// A part-typed chord belongs to the keymap it was started in.
    #[test]
    fn changing_mode_abandons_a_pending_chord() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z");
        assert!(!ui.pending.is_empty());
        execute(&mut ui, &ctx, Command::Mode(crate::cmd::Mode::Transport));
        assert!(ui.pending.is_empty(), "the chord survived the mode change");
    }

    /// Completing the chord takes it away again — it is a prompt, not a mode.
    #[test]
    fn the_panel_goes_when_the_chord_completes() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z");
        waited(&mut ui);
        assert!(render(&ui, &ctx, 100, 20).contains("fold open"));
        press(&mut ui, &ctx, "a");
        assert!(
            !render(&ui, &ctx, 100, 20).contains("fold open"),
            "the panel outlived the chord"
        );
    }

    /// A view's own binding hides the common one, exactly as `lookup` has it —
    /// offering both would be offering a key that does something else.
    #[test]
    fn the_panel_applies_context_shadowing() {
        use crate::cmd::bind::{Next, continuations};
        use crate::cmd::keys;

        let mut bindings = bind::defaults();
        let seq = keys::parse_seq("z o").expect("parses");
        bindings.insert((crate::cmd::Context::Queue, seq), Command::Quit);

        let under_z = keys::parse_seq("z").expect("parses");
        let rows = continuations(&bindings, crate::cmd::Context::Queue, &under_z);
        let o = rows
            .iter()
            .find(|(k, _)| k == "o")
            .expect("`o` should still be offered");
        assert_eq!(o.1, Next::Runs(&Command::Quit), "the queue's own wins");
        assert_eq!(
            rows.iter().filter(|(k, _)| k == "o").count(),
            1,
            "and it must be offered once, not twice"
        );
    }

    /// A key bound on its own fires immediately, so anything extending it is
    /// unreachable — the panel must not advertise it as a prefix.
    #[test]
    fn a_key_bound_on_its_own_is_shown_as_what_it_runs() {
        use crate::cmd::bind::{Next, continuations};
        use crate::cmd::keys;

        let mut bindings = bind::defaults();
        bindings.insert(
            (
                crate::cmd::Context::Common,
                keys::parse_seq("z o x").unwrap(),
            ),
            Command::Quit,
        );

        let rows = continuations(
            &bindings,
            crate::cmd::Context::Common,
            &keys::parse_seq("z").unwrap(),
        );
        let o = rows.iter().find(|(k, _)| k == "o").expect("`o` is bound");
        assert!(
            matches!(o.1, Next::Runs(_)),
            "`zo` fires on its own, so `zox` can never be reached: {:?}",
            o.1
        );
    }

    /// The question that started this: close everything **except** where I am.
    #[test]
    fn zx_closes_every_fold_but_the_one_the_cursor_is_in() {
        let store = many_artists(8);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z R");
        let wide = ui.tree.rows(&ui.library, &store).len();
        // Onto a track, three levels down.
        press(&mut ui, &ctx, "j j j");
        let on = ui.tree.rows(&ui.library, &store)[ui.tree.selected].clone();
        assert!(
            matches!(on, tree::Row::Track { .. }),
            "expected a track: {on:?}"
        );

        press(&mut ui, &ctx, "z x");
        let rows = ui.tree.rows(&ui.library, &store);
        assert!(rows.len() < wide, "nothing closed");
        assert_eq!(
            rows[ui.tree.selected], on,
            "the cursor should still be on the track it was on"
        );
        // 8 artists, one of them open with its one album open and that album's
        // 2 tracks — and nothing else.
        assert_eq!(
            rows.len(),
            8 + 1 + 2,
            "something else stayed open: {rows:?}"
        );
    }

    /// On an artist there is nothing to keep open, and `zx` is `zM`.
    #[test]
    fn zx_on_an_artist_is_a_plain_close_everything() {
        let store = many_artists(8);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z R");
        press(&mut ui, &ctx, "z x");
        assert_eq!(ui.tree.rows(&ui.library, &store).len(), 8);
    }

    /// It holds the row on screen, for the same reason `zm` does — closing
    /// everything around the cursor is the moment the cursor must not move.
    #[test]
    fn zx_holds_the_cursor_still_on_screen() {
        let store = many_artists(20);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "z R");
        press(&mut ui, &ctx, "G");
        let _ = render(&ui, &ctx, 90, 14);
        press(&mut ui, &ctx, "5 k");
        let before = render(&ui, &ctx, 90, 14);
        let was = tree_cursor_row(&ui);

        press(&mut ui, &ctx, "z x");
        let after = render(&ui, &ctx, 90, 14);
        assert_eq!(
            tree_cursor_row(&ui),
            was,
            "before:\n{before}\nafter:\n{after}"
        );
    }

    /// `zt` puts the cursor's row at the top of the pane, `zb` at the bottom,
    /// `zz` in the middle — measured by where the row actually lands.
    #[test]
    fn zt_zz_zb_place_the_cursor_row_in_the_pane() {
        let store = many_artists(30);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "1 5 G");
        let _ = render(&ui, &ctx, 90, 14);

        press(&mut ui, &ctx, "z t");
        let top = render(&ui, &ctx, 90, 14);
        assert_eq!(tree_cursor_row(&ui), 0, "zt should go to the top:\n{top}");

        // A 14-row terminal is one tab line, one status line and a 12-row
        // body, whose borders leave ten rows of list.
        press(&mut ui, &ctx, "z b");
        let bottom = render(&ui, &ctx, 90, 14);
        assert_eq!(
            tree_cursor_row(&ui),
            9,
            "zb should go to the last visible row:\n{bottom}"
        );

        press(&mut ui, &ctx, "z z");
        let _ = render(&ui, &ctx, 90, 14);
        assert_eq!(tree_cursor_row(&ui), 5, "zz should go to the middle");
    }

    /// `zt` near the end of a list has nothing below it to show. vim scrolls
    /// anyway and fills with `~`; without the filler this would be ignored.
    #[test]
    fn zt_on_the_last_row_scrolls_rather_than_being_ignored() {
        let store = many_artists(30);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "G");
        let _ = render(&ui, &ctx, 90, 14);
        press(&mut ui, &ctx, "z t");
        let out = render(&ui, &ctx, 90, 14);

        assert_eq!(tree_cursor_row(&ui), 0, "the last row should be at the top");
        assert!(out.contains('~'), "and the rest filler:\n{out}");
    }

    /// It works in the flat lists too, which is where a track index and a
    /// screen row stop being the same number.
    #[test]
    fn placing_works_in_the_sorted_list() {
        let store = many_artists(30);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        press(&mut ui, &ctx, "2 0 G");
        let _ = render(&ui, &ctx, 90, 14);
        press(&mut ui, &ctx, "z t");
        let out = render(&ui, &ctx, 90, 14);
        assert!(
            out.lines().nth(2).is_some_and(|l| l.contains("Track")),
            "the cursor's row should be the first one drawn:\n{out}"
        );
    }

    /// `gp` finds the playing track and opens whatever was hiding it, without
    /// disturbing the rest of the tree.
    #[test]
    fn g_p_reveals_the_playing_track() {
        let store = many_artists(8);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        // Play something three levels down, then close everything over it.
        press(&mut ui, &ctx, "z R");
        press(&mut ui, &ctx, "j j j");
        press(&mut ui, &ctx, "<leader> x");
        let playing = ui.playback.current.expect("nothing started");
        press(&mut ui, &ctx, "z M");
        assert_eq!(ui.tree.rows(&ui.library, &store).len(), 8, "all closed");

        press(&mut ui, &ctx, "g p");
        let rows = ui.tree.rows(&ui.library, &store);
        assert!(
            matches!(&rows[ui.tree.selected], tree::Row::Track { id, .. } if *id == playing),
            "the cursor should be on the playing track: {:?}",
            rows.get(ui.tree.selected)
        );
        assert_eq!(
            rows.len(),
            8 + 1 + 2,
            "it should open the way to the track and nothing else: {rows:?}"
        );
    }

    /// And says so rather than moving somewhere arbitrary.
    #[test]
    fn g_p_with_nothing_playing_says_so() {
        let store = many_artists(4);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Tree;

        press(&mut ui, &ctx, "g p");
        assert_eq!(ui.tree.selected, 0);
        assert!(render(&ui, &ctx, 90, 12).contains("nothing is playing"));
    }

    /// The arithmetic, directly. ratatui scrolls to keep the selection visible,
    /// so it quietly corrects an off-by-one in `zb` — which makes the mistake
    /// invisible in a rendered pane and visible only here.
    #[test]
    fn place_puts_the_row_at_the_edge_it_names() {
        let scroll = Scroll::default();
        // Ten visible rows, cursor on row 20.
        let _ = scroll.state_within(Some(20), 100, 10);

        scroll.place(crate::cmd::Place::Top);
        assert_eq!(scroll.top(), 20, "zt: the cursor's row is the first drawn");

        scroll.place(crate::cmd::Place::Bottom);
        assert_eq!(
            scroll.top(),
            11,
            "zb: rows 11..=20 fill the pane, so 20 is the last drawn"
        );

        scroll.place(crate::cmd::Place::Centre);
        assert_eq!(scroll.top(), 15, "zz: five rows above the cursor");
    }

    /// A pane nothing has drawn yet has no height to divide by.
    #[test]
    fn placing_before_the_first_frame_does_nothing() {
        let scroll = Scroll::default();
        for place in crate::cmd::Place::ALL {
            scroll.place(*place);
            assert_eq!(scroll.top(), 0);
        }
    }

    /// View 3's left pane is a short list that draws itself, so `zz` there has
    /// nothing to act on — and must not reach across and scroll the tracks.
    #[test]
    fn placing_in_the_playlist_pane_leaves_the_tracks_alone() {
        let store = many_artists(30);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Playlist;
        for id in ui.library.sorted() {
            ui.playlist.push(*id);
        }

        // Scroll the right-hand pane, then move focus to the left one.
        ui.playlist_pane = Pane::Tracks;
        press(&mut ui, &ctx, "G");
        let _ = render(&ui, &ctx, 90, 14);
        let parked = ui.playlist_scroll.top();
        assert!(parked > 0, "the tracks should be scrolled to start with");

        ui.playlist_pane = Pane::Tree;
        press(&mut ui, &ctx, "z t");
        assert_eq!(
            ui.playlist_scroll.top(),
            parked,
            "the tracks moved while the cursor was in the other pane"
        );
    }

    /// The same in view 7. The open guide scrolls by its own mechanism, not a
    /// `Scroll`, so placing while focused there does nothing — but it must not
    /// move the list of guides instead.
    #[test]
    fn placing_in_an_open_guide_leaves_the_list_alone() {
        let store = many_artists(4);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Guide;
        ui.guide_pane = Pane::Tree;

        press(&mut ui, &ctx, "G");
        let _ = render(&ui, &ctx, 60, 8);
        let parked = ui.guides_scroll.top();

        ui.guide_pane = Pane::Tracks;
        press(&mut ui, &ctx, "z t");
        assert_eq!(
            ui.guides_scroll.top(),
            parked,
            "the list of guides moved while the cursor was in the guide"
        );
    }

    /// The flat-list reading of #31: the album divider *is* the structure, so
    /// the same keys work in view 2 without a second concept.
    #[test]
    fn braces_step_album_to_album_in_the_sorted_list() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 0;

        let album_of = |ui: &Ui| {
            store
                .get(ui.library.sorted()[ui.sorted])
                .and_then(|t| t.album.clone())
        };
        let first = album_of(&ui);

        press(&mut ui, &ctx, "}");
        assert_ne!(album_of(&ui), first, "`}}` should have crossed a boundary");

        press(&mut ui, &ctx, "{");
        assert_eq!(album_of(&ui), first, "`{{` should have come back");
        assert_eq!(ui.sorted, 0, "and to the album's first track, not its last");
    }

    /// `]]` steps by artist, which in a compilation is a coarser boundary than
    /// the album — the two must not collapse into the same motion.
    #[test]
    fn brackets_step_artist_to_artist() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 0;

        let artist_of = |ui: &Ui| {
            store
                .get(ui.library.sorted()[ui.sorted])
                .map(|t| t.file_under().to_string())
        };
        let first = artist_of(&ui);
        press(&mut ui, &ctx, "] ]");
        assert_ne!(artist_of(&ui), first);
    }

    /// `-` goes to the top of the run you are in, and only then out of it.
    #[test]
    fn minus_climbs_to_the_start_of_the_album_then_out_of_it() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        // Onto the second track of an album that has more than one.
        press(&mut ui, &ctx, "}");
        let start = ui.sorted;
        press(&mut ui, &ctx, "j");
        assert!(
            ui.sorted > start,
            "the fixture needs a multi-track album here"
        );

        press(&mut ui, &ctx, "-");
        assert_eq!(
            ui.sorted, start,
            "`-` should climb to the album's first track"
        );

        press(&mut ui, &ctx, "-");
        assert!(ui.sorted < start, "and then out to the album above");
    }

    /// `]z` is the far end of the run — the thing `G` cannot give you without
    /// leaving the album entirely.
    #[test]
    fn group_end_reaches_the_last_track_of_the_run() {
        // `many_artists` gives every album exactly two tracks, so landing one
        // row late is a row that is still inside an album — which is what
        // makes an off-by-one visible here and invisible in `fixture`.
        let store = many_artists(5);
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 0;

        let album_of = |ui: &Ui| {
            store
                .get(ui.library.sorted()[ui.sorted])
                .and_then(|t| t.album.clone())
        };

        press(&mut ui, &ctx, "] z");
        let last = album_of(&ui);
        assert_eq!(last, album_of(&ui));

        press(&mut ui, &ctx, "j");
        assert_ne!(
            album_of(&ui),
            last,
            "`]z` should have landed on the last track of its album, so the \
             next row belongs to a different one"
        );
    }

    /// Counts, as everywhere else.
    #[test]
    fn a_structural_motion_takes_a_count() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;
        ui.sorted = 0;

        press(&mut ui, &ctx, "}");
        press(&mut ui, &ctx, "}");
        let twice = ui.sorted;

        ui.sorted = 0;
        press(&mut ui, &ctx, "2 }");
        assert_eq!(ui.sorted, twice, "`2}}` should be `}}` twice");
    }

    /// Two albums by one artist, followed by another artist — the shape that
    /// separates `}` from `]]`. The shared fixture cannot: its only
    /// two-album artist sorts last, so there is nothing for `]]` to reach.
    fn two_albums_then_another_artist() -> Store {
        let mut store = Store::new();
        for (path, artist, album, title, n) in [
            ("/m/a1.mp3", "Anna", "First", "one", 1u32),
            ("/m/a2.mp3", "Anna", "First", "two", 2),
            ("/m/a3.mp3", "Anna", "Second", "three", 1),
            ("/m/a4.mp3", "Anna", "Second", "four", 2),
            ("/m/b1.mp3", "Bruno", "Third", "five", 1),
        ] {
            store.upsert(Track {
                path: PathBuf::from(path),
                artist: Some(artist.into()),
                album: Some(album.into()),
                title: Some(title.into()),
                tracknumber: Some(n),
                duration: Duration::from_secs(100),
                ..Default::default()
            });
        }
        store
    }

    /// `]]` and `}` must not collapse into one motion: a section step has to
    /// skip a boundary that a sibling step stops at.
    #[test]
    fn sections_skip_a_second_album_by_the_same_artist() {
        let store = two_albums_then_another_artist();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        let artist_of = |ui: &Ui| {
            store
                .get(ui.library.sorted()[ui.sorted])
                .map(|t| t.file_under().to_string())
        };

        ui.sorted = 0;
        press(&mut ui, &ctx, "}");
        assert_eq!(
            artist_of(&ui).as_deref(),
            Some("Anna"),
            "`}}` should stop at Anna's second album"
        );
        assert_eq!(ui.sorted, 2);

        ui.sorted = 0;
        press(&mut ui, &ctx, "] ]");
        assert_eq!(
            artist_of(&ui).as_deref(),
            Some("Bruno"),
            "`]]` should step past both of Anna's albums"
        );
    }

    /// And `[[` goes to the top of the artist you are in before it leaves —
    /// vim's, and what makes it usable for "back to the start of this one".
    #[test]
    fn prev_section_climbs_to_the_top_of_this_artist_first() {
        let store = two_albums_then_another_artist();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Sorted;

        ui.sorted = 3; // the second track of Anna's second album
        press(&mut ui, &ctx, "[ [");
        assert_eq!(ui.sorted, 0, "`[[` should reach the top of Anna");

        ui.sorted = 4; // Bruno
        press(&mut ui, &ctx, "[ [");
        assert_eq!(ui.sorted, 0, "and from Bruno's only track, back to Anna");
    }
}
