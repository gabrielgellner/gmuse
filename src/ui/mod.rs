//! The terminal UI — DESIGN §8 `ui/`.
//!
//! M4 scope: views 1–5, navigation, `Tab`, `Space`. Read-only; nothing here
//! starts playback, which is M5's job.

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
    AddTarget, Command, SearchDir, SeekSpec, SettingKey, SettingValue, ViewId, VolSpec, parse,
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
    Settings = 7,
}

impl View {
    pub fn title(self) -> &'static str {
        match self {
            View::Tree => "library",
            View::Sorted => "sorted",
            View::Playlist => "playlist",
            View::Queue => "queue",
            View::Browser => "browser",
            View::Filters => "filters",
            View::Settings => "settings",
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
            ViewId::Settings => View::Settings,
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
        View::Settings,
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
            View::Settings => C::Settings,
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
    /// The `:` or `/` line, when one is open.
    pub prompt: Option<Prompt>,
    /// The last search, for `n` and `N`.
    pub last_search: Option<(String, SearchDir)>,
    /// Keys typed so far that have not resolved to a binding, and the count
    /// prefix in front of them.
    pub pending: Vec<Key>,
    pub count: Option<usize>,
    /// The view before the current one, for `g<Tab>`.
    pub previous_view: View,
    /// Show a relative-distance gutter beside list rows.
    pub relative_numbers: bool,
    /// Session log. Disabled unless `--log` was given.
    pub audit: crate::audit::Audit,
    /// Where to write "where I was". `None` disables resumption entirely.
    pub session_path: Option<PathBuf>,
    /// Play counts and first-seen dates. Empty and unsaved unless a path is set.
    pub history: crate::history::History,
    pub history_path: Option<PathBuf>,
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
    pub settings_sel: usize,
    /// Cursor in view 6, the catalog list.
    pub catalog_sel: usize,
    pub theme: theme::Theme,
    /// How the now-playing description is written. Parsed at config load; the
    /// status line only renders it.
    pub now_playing: crate::format::Template,
    /// The library projection. Owned rather than borrowed, because changing
    /// the active catalogs — or `smart_artist_sort` — rebuilds it.
    pub library: Library,
    pub catalogs: crate::catalog::Catalogs,
    /// Smart playlists, from the config (§7, issue #15).
    pub playlists: crate::smart::SmartPlaylists,
    /// Which smart playlist view 3 is currently showing, if any.
    ///
    /// The name rather than the list: the tracks live in `playlist` like any
    /// other, and this is what marks them as **owned by a rule** — which is
    /// what makes editing refuse and `:freeze` mean something. Cleared by
    /// `:freeze` and by anything that fills view 3 by hand.
    pub smart: Option<String>,
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
        Self {
            view: View::Tree,
            tree: TreeState::default(),
            sorted: 0,
            playlist_sel: 0,
            queue_sel: 0,
            browser: BrowserState::at(start_dir),
            status: String::new(),
            playlist: Playlist::new(),
            playlists: crate::smart::SmartPlaylists::default(),
            smart: None,
            queue: Queue::new(),
            playback: Playback::default(),
            volume_hint: None,
            bindings: bind::defaults(),
            prompt: None,
            last_search: None,
            pending: Vec::new(),
            count: None,
            previous_view: View::Tree,
            relative_numbers: false,
            pending_next: None,
            session_path: None,
            history: crate::history::History::default(),
            history_path: None,
            counted_current: false,
            resuming: false,
            audit: crate::audit::Audit::disabled(),
            seen_underruns: 0,
            settings_sel: 0,
            catalog_sel: 0,
            theme: theme::Theme::default(),
            now_playing: crate::format::Template::default(),
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

    pub fn completion_world(&self) -> crate::cmd::complete::World {
        crate::cmd::complete::World {
            catalogs: self
                .catalogs
                .defined()
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            playlists: self
                .playlists
                .names()
                .into_iter()
                .map(str::to_string)
                .collect(),
            bound: bind::sorted(&self.bindings)
                .into_iter()
                .map(|(ctx, key, _)| (ctx, key))
                .collect(),
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
            View::Tree => {
                let tracks = self.tree.tracks_for(&self.library, ctx.store);
                match self.tree.pane {
                    // With the tree pane focused, Enter plays the first track
                    // of whatever is selected rather than doing nothing.
                    Pane::Tree => tracks.first().copied(),
                    Pane::Tracks => tracks.get(self.tree.track_selected).copied(),
                }
            }
            View::Sorted => self.library.sorted().get(self.sorted).copied(),
            View::Playlist => self.playlist.entries().get(self.playlist_sel).copied(),
            View::Queue => self.queue.entries().nth(self.queue_sel).copied(),
            View::Browser | View::Filters | View::Settings => None,
        }
    }
}

/// What the UI needs to render. Borrowed, never owned — the library is the
/// application's, not the UI's.
pub struct Context<'a> {
    pub store: &'a Store,
    /// `None` in tests, which have no audio device.
    pub player: Option<&'a PlayerHandle>,
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
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.is_press()
        {
            handle_key(key, ui, ctx);
        }
        drain_player_events(ui, ctx);
        note_play_progress(ui, ctx);
        audit_underruns(ui, ctx);
        ui.expire_status();
    }
    Ok(())
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
    if ui.counted_current || ui.history_path.is_none() {
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
    save_history(ui);
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

    if ui.history_path.is_none() {
        ui.notify("ratings need a history file");
        return;
    }
    // `note_rating` takes zero as "clear", which is the storage-level spelling
    // of `RateSpec::Clear`; the command language does not have a zero.
    let stars = match spec {
        RateSpec::Set(n) => n,
        _ => 0,
    };
    ui.history.note_rating(&path, stars, crate::history::now());
    save_history(ui);

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
pub fn save_history(ui: &mut Ui) {
    let Some(path) = ui.history_path.clone() else {
        return;
    };
    if !ui.history.is_dirty() {
        return;
    }
    match ui.history.save(&path) {
        Ok(()) => ui.history.mark_saved(),
        Err(e) => ui.audit.line("history", &format!("could not save: {e}")),
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

/// Route a key.
///
/// A prompt swallows everything while it is open. Otherwise the key is
/// normalised, looked up in the bindings (context first, then `Common`), and
/// the resulting [`Command`] is handed to [`execute`] — which is the only
/// place UI state is mutated, per §6.
fn handle_key(key: KeyEvent, ui: &mut Ui, ctx: &Context<'_>) {
    if ui.prompt.is_some() {
        handle_prompt_key(key, ui, ctx);
        return;
    }

    // Opening a prompt is a UI mode, not a command: there is nothing for it to
    // do until a line has been typed.
    match key.code {
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
            ui.prompt = Some(Prompt {
                kind: PromptKind::Search(SearchDir::Forward),
                buffer: String::new(),
                menu: None,
                selected: 0,
            });
            return;
        }
        KeyCode::Char('?') => {
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

    ui.pending.push(key);
    match bind::resolve(&ui.bindings, ui.view.context(), &ui.pending) {
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
            }
        }
        KeyCode::Backspace => {
            if prompt.buffer.pop().is_none() {
                // Backspacing past the sigil closes the line, as in vi.
                ui.prompt = None;
            } else {
                // Widens the menu again, rather than dismissing it.
                refilter(ui);
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
                crate::cmd::complete::complete_setting_value(last).map(|c| {
                    let head = &buffer[..buffer.len() - last.len()];
                    (format!("{head}{}", c.line), c.options)
                })
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
                PromptKind::Search(dir) => execute(
                    ui,
                    ctx,
                    Command::Search {
                        pattern: prompt.buffer,
                        dir,
                    },
                ),
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
    let page = 10usize;

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
        Command::PageDown => set_cursor(ui, (cursor + page * n).min(len.saturating_sub(1))),
        Command::PageUp => set_cursor(ui, cursor.saturating_sub(page * n)),
        Command::PaneNext => {
            if ui.view == View::Tree {
                ui.tree.pane = match ui.tree.pane {
                    Pane::Tree => Pane::Tracks,
                    Pane::Tracks => Pane::Tree,
                };
            }
        }
        // Directional rather than a toggle: with two panes they behave the
        // same, but the binding says what it means and keeps working when a
        // third arrives.
        Command::PaneLeft => {
            if ui.view == View::Tree {
                ui.tree.pane = Pane::Tree;
            }
        }
        Command::PaneRight => {
            if ui.view == View::Tree {
                ui.tree.pane = Pane::Tracks;
            }
        }
        // Run a command as though the named pane had focus, then put focus
        // back. Reusing the ordinary command rather than duplicating every
        // motion per pane is the whole point of the wrapper.
        Command::In { pane, cmd } => {
            if ui.view != View::Tree {
                return;
            }
            let focused = ui.tree.pane;
            ui.tree.pane = match pane {
                crate::cmd::PaneTarget::Tree => Pane::Tree,
                crate::cmd::PaneTarget::Tracks => Pane::Tracks,
            };
            execute_counted(ui, ctx, *cmd, count);
            ui.tree.pane = focused;
        }
        Command::Expand => {
            if ui.view == View::Tree && ui.tree.pane == Pane::Tree {
                ui.tree.toggle(&ui.library, ctx.store);
            } else if ui.view == View::Filters {
                // Space turns one catalog on or off. The union of what is on
                // is the library, so this reads as "include this too".
                toggle_catalog_under_cursor(ui, ctx);
            }
        }
        Command::ExpandAll => {
            if ui.view == View::Tree {
                ui.tree.expand_all(&ui.library);
            }
        }
        Command::CollapseAll => {
            if ui.view == View::Tree {
                ui.tree.collapse_all();
                ui.tree.selected = 0;
            }
        }
        Command::Activate => match ui.view {
            View::Browser => ui.browser.enter(),
            View::Settings => {}
            // Enter makes this the only catalog — the common case, and one
            // keypress rather than clearing the others by hand.
            View::Filters => {
                let Some(name) = catalog_under_cursor(ui) else {
                    return;
                };
                apply_catalog_spec(ui, ctx, crate::catalog::Spec::Only(vec![name]));
            }
            _ => {
                ui.playback.active = match ui.view {
                    View::Playlist => ActiveSource::Playlist,
                    _ => ActiveSource::Library,
                };
                if let Some(id) = ui.selected_track(ctx) {
                    if ui.view == View::Queue {
                        ui.queue.pop_front();
                    }
                    play(ui, ctx, id);
                }
            }
        },
        Command::Remove => match ui.view {
            View::Playlist if ui.smart.is_some() => ui.notify(rule_owned("remove from")),
            View::Playlist => {
                ui.playlist.remove(ui.playlist_sel);
                ui.playlist_sel = ui.playlist_sel.min(ui.playlist.len().saturating_sub(1));
            }
            View::Queue => {
                ui.queue.remove(ui.queue_sel);
                ui.queue_sel = ui.queue_sel.min(ui.queue.len().saturating_sub(1));
            }
            _ => ui.notify("nothing to remove here"),
        },
        Command::Rescan => ui.notify("rescan lands with :rescan in the app"),
        Command::Playlist(name) => load_smart(ui, ctx, &name),
        Command::Freeze => match ui.smart.take() {
            Some(name) => ui.notify(format!(
                "froze {name} — {} tracks, now editable",
                ui.playlist.len()
            )),
            None => ui.notify("the playlist is already an ordinary one"),
        },

        // ---- transport
        Command::Play => {
            if ui.playback.current.is_some() {
                if let Some(p) = ctx.player {
                    p.play();
                }
            } else if let Some(id) = ui.selected_track(ctx) {
                play(ui, ctx, id);
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
            };
            apply_setting(ui, ctx, key, value);
        }
        Command::Bind {
            ctx: bctx,
            key,
            cmd,
        } => {
            ui.bindings.insert((bctx, key), *cmd);
        }
        Command::Unbind { ctx: bctx, key } => {
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
            run_search(ui, ctx, &pattern, dir);
        }
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
fn rule_owned(what: &str) -> String {
    format!("this playlist is a rule — `:freeze` it to {what} it by hand")
}

/// Fill view 3 from a named smart playlist.
fn load_smart(ui: &mut Ui, ctx: &Context<'_>, name: &str) {
    let Some(spec) = ui.playlists.get(name) else {
        ui.notify(if ui.playlists.is_empty() {
            "no smart playlists are defined — see `[playlists.…]` in the config".into()
        } else {
            format!(
                "no smart playlist {name:?} — try one of: {}",
                ui.playlists.names().join(", ")
            )
        });
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
    ui.smart = Some(name.to_string());
    switch_view(ui, View::Playlist);
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
    let Some(name) = ui.smart.clone() else {
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
    ui.tree.track_selected = 0;
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
        View::Tree if ui.tree.pane == Pane::Tree => ui.tree.tracks_for(&ui.library, ctx.store),
        _ => ui.selected_track(ctx).into_iter().collect(),
    };
    if ids.is_empty() {
        ui.notify("nothing selected");
        return;
    }
    let count = ids.len();
    match target {
        AddTarget::Playlist => {
            if ui.smart.is_some() {
                ui.notify(rule_owned("add to"));
                return;
            }
            for id in ids {
                ui.playlist.push(id);
            }
            ui.notify(format!("added {count} to the playlist"));
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

/// Move the cursor to the next row matching `pattern`, wrapping once.
fn run_search(ui: &mut Ui, ctx: &Context<'_>, pattern: &str, dir: SearchDir) {
    let needle = pattern.to_lowercase();
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

/// Lower-cased text for each row of the current view, for searching.
fn search_rows(ui: &Ui, ctx: &Context<'_>) -> Vec<String> {
    let describe = |id: &crate::store::TrackId| {
        ctx.store
            .get(*id)
            .map(|t| {
                format!(
                    "{} {} {}",
                    t.file_under(),
                    t.album_or_unknown(),
                    t.display_title()
                )
                .to_lowercase()
            })
            .unwrap_or_default()
    };
    match ui.view {
        View::Tree => match ui.tree.pane {
            Pane::Tree => ui
                .tree
                .rows(&ui.library, ctx.store)
                .iter()
                .map(|r| r.label().to_lowercase())
                .collect(),
            Pane::Tracks => ui
                .tree
                .tracks_for(&ui.library, ctx.store)
                .iter()
                .map(describe)
                .collect(),
        },
        View::Sorted => ui.library.sorted().iter().map(describe).collect(),
        View::Playlist => ui.playlist.entries().iter().map(describe).collect(),
        View::Queue => ui.queue.entries().map(describe).collect(),
        View::Browser => ui
            .browser
            .entries
            .iter()
            .map(|p| p.display().to_string().to_lowercase())
            .collect(),
        View::Filters | View::Settings => Vec::new(),
    }
}

fn current_len(ui: &Ui, ctx: &Context<'_>) -> usize {
    match ui.view {
        View::Tree => match ui.tree.pane {
            Pane::Tree => ui.tree.rows(&ui.library, ctx.store).len(),
            Pane::Tracks => ui.tree.tracks_for(&ui.library, ctx.store).len(),
        },
        View::Sorted => ui.library.sorted().len(),
        View::Playlist => ui.playlist.len(),
        View::Queue => ui.queue.len(),
        View::Browser => ui.browser.entries.len(),
        View::Filters => ui.catalogs.defined().len(),
        View::Settings => {
            bind::sorted(&ui.bindings).len() + SettingKey::ALL.len() + ui.catalogs.defined().len()
        }
    }
}

fn current_cursor(ui: &Ui) -> usize {
    match ui.view {
        View::Tree => match ui.tree.pane {
            Pane::Tree => ui.tree.selected,
            Pane::Tracks => ui.tree.track_selected,
        },
        View::Sorted => ui.sorted,
        View::Playlist => ui.playlist_sel,
        View::Queue => ui.queue_sel,
        View::Browser => ui.browser.selected,
        View::Filters => ui.catalog_sel,
        View::Settings => ui.settings_sel,
    }
}

fn set_cursor(ui: &mut Ui, next: usize) {
    match ui.view {
        View::Tree => match ui.tree.pane {
            Pane::Tree => {
                ui.tree.selected = next;
                // Moving in the tree resets the track pane: keeping the old
                // index would point into a different album's list.
                ui.tree.track_selected = 0;
            }
            Pane::Tracks => ui.tree.track_selected = next,
        },
        View::Sorted => ui.sorted = next,
        View::Playlist => ui.playlist_sel = next,
        View::Queue => ui.queue_sel = next,
        View::Browser => ui.browser.selected = next,
        View::Filters => ui.catalog_sel = next,
        View::Settings => ui.settings_sel = next,
    }
}

/// View 6: the catalogs, which §7 settled are named, persistent filters.
///
/// cmus lists filters here and uses space to *select* then enter to activate.
/// This toggles directly — space turns one on or off, enter makes it the only
/// one — because the result is visible immediately either way and the two-step
/// exists in cmus to batch a change you cannot see until you commit it.
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
    let menu_height = ui
        .prompt
        .as_ref()
        .map(|p| menu_height(p, frame.area().width))
        .unwrap_or(0);

    let [tabs, body, menu, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(menu_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    if menu_height > 0
        && let Some(prompt) = &ui.prompt
    {
        draw_completions(frame, menu, ui, prompt);
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
    let playlist_title = match &ui.smart {
        Some(name) => format!("playlist: {name}"),
        None => "playlist".to_string(),
    };

    match ui.view {
        View::Tree => draw_tree(frame, body, ui, ctx),
        View::Sorted => draw_tracks(
            frame,
            body,
            ui,
            ctx,
            TrackList {
                ids: ui.library.sorted(),
                selected: ui.sorted,
                title: "sorted",
                focused: true,
                divide: true,
            },
        ),
        View::Playlist => draw_tracks(
            frame,
            body,
            ui,
            ctx,
            TrackList {
                ids: ui.playlist.entries(),
                selected: ui.playlist_sel,
                title: &playlist_title,
                focused: true,
                divide: false,
            },
        ),
        View::Queue => {
            let entries: Vec<_> = ui.queue.entries().copied().collect();
            draw_tracks(
                frame,
                body,
                ui,
                ctx,
                TrackList {
                    ids: &entries,
                    selected: ui.queue_sel,
                    title: "queue",
                    focused: true,
                    divide: false,
                },
            )
        }
        View::Browser => draw_browser(frame, body, ui),
        View::Filters => draw_catalogs(frame, body, ui),
        View::Settings => draw_settings(frame, body, ui),
    }
}

/// View 7. §6 promised this would cost nothing extra once bindings and
/// settings were data, and it does: it is a rendering of both maps.
fn draw_settings(frame: &mut Frame, area: Rect, ui: &Ui) {
    let mut items: Vec<ListItem> = Vec::new();

    for key in SettingKey::ALL.iter().copied() {
        let value = match key {
            SettingKey::AaaMode => ui.playback.aaa_mode.label().to_string(),
            SettingKey::ReplayGain => ui.replaygain.label().to_string(),
            SettingKey::Shuffle => ui.playback.shuffle.to_string(),
            SettingKey::Repeat => ui.playback.repeat.to_string(),
            SettingKey::RepeatCurrent => ui.playback.repeat_current.to_string(),
            SettingKey::Continue => ui.continue_playing.to_string(),
            SettingKey::SmartArtistSort => ui.smart_artist_sort.to_string(),
            SettingKey::ResampleQuality => "balanced".into(),
            SettingKey::AlbumHeaders => ui.album_headers.to_string(),
            SettingKey::RelativeNumbers => ui.relative_numbers.to_string(),
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("{:<20}", key.name()),
                ui.theme.get(theme::Role::SettingKey),
            ),
            Span::raw(value),
        ])));
    }

    if !ui.catalogs.defined().is_empty() {
        let active: Vec<&str> = ui.catalogs.active_names();
        for catalog in ui.catalogs.defined() {
            let on = active.contains(&catalog.name.as_str());
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<20}", format!("catalog {}", catalog.name)),
                    ui.theme.get(theme::Role::SettingKey),
                ),
                Span::raw(format!(
                    "{}  {}",
                    if on { "active" } else { "  off " },
                    catalog
                        .roots
                        .iter()
                        .map(|r| r.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            ])));
        }
    }

    for (bctx, key, cmd) in bind::sorted(&ui.bindings) {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("{:<10}", bctx.name()),
                ui.theme.get(theme::Role::BindingContext),
            ),
            Span::styled(
                format!("{:<10}", key.to_string()),
                ui.theme.get(theme::Role::BindingKey),
            ),
            Span::raw(parse::format(cmd)),
        ])));
    }

    let mut state = ListState::default().with_selected(Some(ui.settings_sel));
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&ui.theme, "settings and bindings", true))
            .highlight_style(highlight(&ui.theme, true)),
        area,
        &mut state,
    );
}

/// Rows the completion menu needs, borders included. Zero when there is
/// nothing to show.
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

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

    let rows = ui.tree.rows(&ui.library, ctx.store);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| ListItem::new(Line::from(row.label())))
        .collect();

    let focused = ui.tree.pane == Pane::Tree;
    let mut state = ListState::default().with_selected(Some(ui.tree.selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&ui.theme, "artist / album", focused))
            .highlight_style(highlight(&ui.theme, focused)),
        left,
        &mut state,
    );

    let tracks = ui.tree.tracks_for(&ui.library, ctx.store);
    draw_track_list(
        frame,
        right,
        ui,
        ctx,
        TrackList {
            ids: &tracks,
            selected: ui.tree.track_selected,
            title: "tracks",
            focused: !focused,
            divide: true,
        },
    );
}

/// What to draw a track list as.
struct TrackList<'a> {
    ids: &'a [crate::store::TrackId],
    /// A **track** index, not a screen row.
    selected: usize,
    title: &'a str,
    focused: bool,
    /// Divide by album. Off for the playlist and queue: they are deliberate
    /// orderings, and dividing an interleaved one puts a header on nearly
    /// every row.
    divide: bool,
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
    let rows = if spec.divide && ui.album_headers {
        list::rows(ctx.store, spec.ids)
    } else {
        spec.ids.iter().copied().map(list::Row::Track).collect()
    };
    let items = list::items(
        &ui.theme,
        ctx.store,
        &rows,
        list::RowStyle {
            relative: ui.relative_numbers,
            selected: spec.selected,
            playing: ui.playback.current,
        },
    );
    let mut state =
        ListState::default().with_selected(list::row_of_track(&rows, spec.selected).or(Some(0)));
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
    let mut state = ListState::default().with_selected(Some(ui.browser.selected));
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
        };
        let mut ui = ui_of(&store, true);
        // Select "Various Artists" and expand it.
        let rows = ui.tree.rows(&ui.library, &store);
        ui.tree.selected = rows
            .iter()
            .position(|r| r.label().contains("Various Artists"))
            .unwrap();
        let lib = std::mem::take(&mut ui.library);
        ui.tree.toggle(&lib, &store);
        ui.library = lib;

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
        };
        let mut ui = ui_of(&store, true);
        let rows = ui.tree.rows(&ui.library, &store);
        ui.tree.selected = rows
            .iter()
            .position(|r| r.label().contains("Wall Builders"))
            .unwrap();

        let out = render(&ui, &ctx, 74, 12);
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
        };
        let mut ui = ui_of(&store, true);
        let rows = ui.tree.rows(&ui.library, &store);
        ui.tree.selected = rows
            .iter()
            .position(|r| r.label().contains("Unknown Artist"))
            .unwrap();

        let out = render(&ui, &ctx, 74, 12);
        assert!(out.contains("unlabelled.mp3"), "{out}");
        assert!(out.contains("1:05"), "duration missing:\n{out}");
    }

    #[test]
    fn empty_playlist_says_so_rather_than_rendering_blank() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
        };
        let mut ui = ui_of(&store, true);
        ui.view = View::Playlist;
        let out = render(&ui, &ctx, 74, 10);
        assert!(out.contains("the playlist is empty"), "{out}");
        assert!(out.contains("3:playlist"), "{out}");
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
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

    #[test]
    fn tab_moves_focus_between_panes() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
        };
        let mut ui = ui_of(&store, true);
        assert_eq!(ui.tree.pane, Pane::Tree);
        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        assert_eq!(ui.tree.pane, Pane::Tracks);
        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        assert_eq!(ui.tree.pane, Pane::Tree);
    }

    /// Moving in the tree must reset the track cursor: the old index would
    /// point into a different album's list.
    #[test]
    fn moving_the_tree_cursor_resets_the_track_pane() {
        let store = fixture();
        let ctx = Context {
            store: &store,
            player: None,
        };
        let mut ui = ui_of(&store, true);
        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        handle_key(KeyEvent::from(KeyCode::Char('j')), &mut ui, &ctx);
        assert_eq!(ui.tree.track_selected, 1);

        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        handle_key(KeyEvent::from(KeyCode::Char('j')), &mut ui, &ctx);
        assert_eq!(ui.tree.track_selected, 0, "stale track cursor after moving");
    }
    // ---- playback wiring (M5) -------------------------------------------
    //
    // `ctx.player` is None here: these cover the decisions the UI makes, which
    // is where the bugs are. Whether cpal then makes a sound is M1/M2's ground.

    fn ctx_of(store: &Store) -> Context<'_> {
        Context {
            store,
            player: None,
        }
    }

    /// A `Ui` over the whole fixture library.
    fn ui_of(store: &Store, smart: bool) -> Ui {
        Ui::new(PathBuf::from("/m"), Library::rebuild(store, smart))
    }

    #[test]
    fn enter_in_the_track_pane_plays_the_selected_track() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx); // focus tracks
        handle_key(KeyEvent::from(KeyCode::Char('j')), &mut ui, &ctx);
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);

        let played = ui.playback.current.expect("nothing started playing");
        let expected = ui.tree.tracks_for(&ui.library, &store)[1];
        assert_eq!(played, expected);
    }

    #[test]
    fn enter_on_an_artist_row_starts_that_artists_first_track() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);

        let played = ui.playback.current.expect("nothing started playing");
        let first = ui.tree.tracks_for(&ui.library, &store)[0];
        assert_eq!(played, first, "Enter on a tree row should not be a no-op");
    }

    #[test]
    fn b_and_z_walk_forwards_and_backwards() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
        let first = ui.playback.current.unwrap();

        handle_key(KeyEvent::from(KeyCode::Char('b')), &mut ui, &ctx);
        let second = ui.playback.current.unwrap();
        assert_ne!(first, second);

        handle_key(KeyEvent::from(KeyCode::Char('z')), &mut ui, &ctx);
        assert_eq!(ui.playback.current, Some(first), "z should come back");
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
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
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

        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
        let playing = ui.playback.current.expect("nothing started");
        assert!(render(&ui, &ctx, 90, 8).contains("Anderson .Paak"));

        handle_key(KeyEvent::from(KeyCode::Char('v')), &mut ui, &ctx);
        assert_eq!(
            ui.playback.current,
            Some(playing),
            "stop must leave the track current, or resuming loses the title"
        );

        // Still named in the status line while stopped.
        let out = render(&ui, &ctx, 90, 8);
        assert!(out.contains("Anderson .Paak"), "{out}");

        // And `b` still steps from where we stopped, rather than restarting.
        handle_key(KeyEvent::from(KeyCode::Char('b')), &mut ui, &ctx);
        assert_ne!(ui.playback.current, Some(playing));
    }
    // ---- the command layer (M6) ------------------------------------------

    use crate::cmd::keys;
    use ratatui::crossterm::event::KeyModifiers;

    /// Feed a key sequence, e.g. `"j"`, `"g g"`, `"^f"`.
    fn press(ui: &mut Ui, ctx: &Context<'_>, keys_text: &str) {
        let seq = keys::parse_seq(keys_text)
            .unwrap_or_else(|e| panic!("bad test key {keys_text:?}: {e}"));
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
        ui.history_path = Some(PathBuf::from("/dev/null"));
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
        type_line(&mut ui, &ctx, "playlist electronic");

        assert_eq!(ui.view, View::Playlist);
        assert_eq!(ui.smart.as_deref(), Some("electronic"));
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
        type_line(&mut ui, &ctx, "playlist electronic");

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
        type_line(&mut ui, &ctx, "playlist nope");
        assert!(ui.status.contains("electronic"), "{}", ui.status);
        assert!(ui.status.contains("good"), "{}", ui.status);
        assert!(ui.smart.is_none());
    }

    #[test]
    fn a_rule_matching_nothing_says_what_the_rule_was() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist good");
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
        type_line(&mut ui, &ctx, "playlist electronic");
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
        type_line(&mut ui, &ctx, "playlist electronic");
        let before = ui.playlist.len();

        execute(&mut ui, &ctx, Command::Freeze);
        assert!(ui.smart.is_none());
        assert_eq!(ui.playlist.len(), before, "freezing must keep the tracks");

        ui.view = View::Playlist;
        execute(&mut ui, &ctx, Command::Remove);
        assert_eq!(ui.playlist.len(), before - 1, "still not editable");
    }

    #[test]
    fn freezing_an_ordinary_playlist_says_so() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);
        execute(&mut ui, &ctx, Command::Freeze);
        assert!(ui.status.contains("already an ordinary"), "{}", ui.status);
    }

    /// A live query, not a snapshot: rating a track has to change what a
    /// rating-based rule answers, without anyone reloading it by hand.
    #[test]
    fn rating_a_track_re_evaluates_the_loaded_rule() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist good");
        assert!(ui.playlist.is_empty(), "nothing is rated yet");

        // Rate the track under the cursor in view 2.
        ui.view = View::Sorted;
        ui.sorted = 0;
        execute(&mut ui, &ctx, Command::Rate(crate::cmd::RateSpec::Set(5)));

        assert_eq!(ui.playlist.len(), 1, "the rule did not notice the rating");
        assert_eq!(ui.smart.as_deref(), Some("good"));
    }

    #[test]
    fn view_three_names_the_rule_it_is_showing() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "playlist electronic");
        let out = render(&ui, &ctx, 74, 10);
        assert!(out.contains("playlist: electronic"), "{out}");

        execute(&mut ui, &ctx, Command::Freeze);
        let out = render(&ui, &ctx, 74, 10);
        assert!(!out.contains("playlist: electronic"), "{out}");
    }

    #[test]
    fn tab_completes_smart_playlist_names() {
        let store = genre_fixture();
        let ctx = ctx_of(&store);
        let mut ui = with_playlists(&store);

        press(&mut ui, &ctx, ":");
        for c in "playlist e".chars() {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
        }
        tab(&mut ui, &ctx);
        assert_eq!(typed(&ui), "playlist electronic ");
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
            // The binding must be reachable through lookup from a real view.
            let view = match bctx {
                crate::cmd::Context::Common => View::Tree,
                other => View::from_id(match other {
                    crate::cmd::Context::Library => ViewId::Library,
                    crate::cmd::Context::SortedLibrary => ViewId::Sorted,
                    crate::cmd::Context::Playlist => ViewId::Playlist,
                    crate::cmd::Context::Queue => ViewId::Queue,
                    crate::cmd::Context::Browser => ViewId::Browser,
                    crate::cmd::Context::Filters => ViewId::Filters,
                    crate::cmd::Context::Settings => ViewId::Settings,
                    crate::cmd::Context::Common => unreachable!(),
                }),
            };
            ui.view = view;
            let found = crate::cmd::bind::lookup(&ui.bindings, view.context(), &key);
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

        // Wrapping, so you can always get anywhere with one key.
        for _ in 0..6 {
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

        // Artist row selected: `y` adds everything under it.
        let expected = ui.tree.tracks_for(&ui.library, &store).len();
        press(&mut ui, &ctx, "y");
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
        press(&mut ui, &ctx, "y");
        let before = ui.playlist.len();

        ui.view = View::Playlist;
        press(&mut ui, &ctx, "D");
        assert_eq!(ui.playlist.len(), before - 1);

        // In the tree there is nothing to remove, and it must say so rather
        // than deleting something else.
        ui.view = View::Tree;
        press(&mut ui, &ctx, "D");
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

    #[test]
    fn view_seven_lists_settings_and_bindings() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        ui.view = View::Settings;
        let out = render(&ui, &ctx, 100, 24);
        assert!(out.contains("aaa_mode"), "{out}");
        assert!(out.contains("shuffle"), "{out}");
        assert!(
            out.contains("7:settings"),
            "the tab strip should list it:\n{out}"
        );

        // Bindings render in the same list, as §6 promised — below the
        // settings, so scroll to them.
        ui.settings_sel = crate::cmd::SettingKey::ALL.len() + 20;
        let out = render(&ui, &ctx, 100, 24);
        assert!(out.contains("common"), "{out}");
        // A command name from the defaults, whatever it happens to be —
        // asserting on a naming *prefix* made this test depend on the
        // vocabulary's shape rather than on bindings being drawn.
        let some_verb = crate::cmd::parse::format(&crate::cmd::Command::Down);
        assert!(out.contains(&some_verb), "no bindings visible:\n{out}");
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
        handle_key(KeyEvent::from(KeyCode::Enter), &mut ui, &ctx);
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

    #[test]
    fn catalogs_show_in_view_seven() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = catalog_ui(&store);
        ui.view = View::Settings;
        let out = render(&ui, &ctx, 110, 24);
        assert!(out.contains("catalog paak"), "{out}");
        assert!(out.contains("catalog va"), "{out}");
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

        // `z` is prev by default.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind common \"z z\" bottom");
        press(&mut ui, &ctx, "z z");
        assert_eq!(ui.sorted, 0, "the chord fired despite `z` being bound");

        // Freeing the key makes the chord reachable.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "unbind common z");
        press(&mut ui, &ctx, "z z");
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

    #[test]
    fn h_and_l_move_between_panes() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        assert_eq!(ui.tree.pane, Pane::Tree);

        press(&mut ui, &ctx, "l");
        assert_eq!(ui.tree.pane, Pane::Tracks);
        press(&mut ui, &ctx, "h");
        assert_eq!(ui.tree.pane, Pane::Tree);

        // Directional, not a toggle: pressing the same one twice stays put.
        press(&mut ui, &ctx, "h");
        assert_eq!(ui.tree.pane, Pane::Tree);
        press(&mut ui, &ctx, "l");
        press(&mut ui, &ctx, "l");
        assert_eq!(ui.tree.pane, Pane::Tracks);
    }

    /// The point of the whole feature: step album to album from the track
    /// pane, without a Tab over and a Tab back each time.
    #[test]
    fn ctrl_j_drives_the_tree_without_leaving_the_track_pane() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, "l"); // focus the tracks
        assert_eq!(ui.tree.pane, Pane::Tracks);
        let first = ui.tree.tracks_for(&ui.library, &store);

        press(&mut ui, &ctx, "^j");
        assert_eq!(ui.tree.selected, 1, "the tree did not move");
        assert_eq!(ui.tree.pane, Pane::Tracks, "focus was stolen");

        // And the track pane now shows a different set.
        let second = ui.tree.tracks_for(&ui.library, &store);
        assert_ne!(first, second, "the track list did not follow");

        press(&mut ui, &ctx, "^k");
        assert_eq!(ui.tree.selected, 0);
        assert_eq!(ui.tree.pane, Pane::Tracks);
    }

    #[test]
    fn driving_the_tree_resets_the_track_cursor() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "l");
        press(&mut ui, &ctx, "j");
        assert_eq!(ui.tree.track_selected, 1);

        // Otherwise the cursor would point into the previous album's list.
        press(&mut ui, &ctx, "^j");
        assert_eq!(ui.tree.track_selected, 0);
    }

    #[test]
    fn a_count_passes_through_the_pane_wrapper() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "l");

        press(&mut ui, &ctx, "2");
        press(&mut ui, &ctx, "^j");
        assert_eq!(ui.tree.selected, 2, "the count was dropped by `in`");
    }

    #[test]
    fn the_in_wrapper_is_expressible_on_the_command_line() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "in tree bottom");
        let rows = ui.tree.rows(&ui.library, &store).len();
        assert_eq!(ui.tree.selected, rows - 1);
        // Focus is unchanged: `in` aims a command, it does not move you.
        assert_eq!(ui.tree.pane, Pane::Tree);
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
        press(&mut ui, &ctx, "space");
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

        let dir = std::env::temp_dir().join("gmuse-history-ui");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ui.history_path = Some(dir.join("history.toml"));

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

    /// The history is written when it changes, and the file is real.
    #[test]
    fn the_history_is_saved_when_dirty() {
        let store = fixture();
        let mut ui = ui_of(&store, true);
        let dir = std::env::temp_dir().join("gmuse-history-save");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.toml");
        ui.history_path = Some(path.clone());

        ui.history.note_played(std::path::Path::new("/m/x.mp3"), 5);
        assert!(ui.history.is_dirty());
        save_history(&mut ui);
        assert!(!ui.history.is_dirty(), "saving should clear the flag");
        assert!(path.exists());

        let back = crate::history::History::load(&path);
        assert_eq!(back.get(std::path::Path::new("/m/x.mp3")).unwrap().plays, 1);
    }

    /// With no history path — the default until one is set — nothing is
    /// recorded and nothing is written.
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

    fn ui_with_history(store: &Store, name: &str) -> Ui {
        let mut ui = ui_of(store, true);
        let dir = std::env::temp_dir().join(format!("gmuse-rating-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ui.history_path = Some(dir.join("history.toml"));
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
        let mut ui = ui_with_history(&store, "saved");
        let path = ui.history_path.clone().unwrap();
        let id = store.iter().next().unwrap().0;
        ui.playback.current = Some(id);

        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "rate 4");

        let back = crate::history::History::load(&path);
        assert_eq!(
            back.get(&store.get(id).unwrap().path).unwrap().rating,
            Some(4)
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
        // Laid out in columns, so several verbs share a line.
        assert!(out.contains("activate"), "{out}");
        assert!(out.contains("quit"), "{out}");
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

    /// An unfocused pane should still read as unfocused, even sitting on the
    /// playing track.
    #[test]
    fn an_unfocused_pane_keeps_its_dim_selection() {
        use ratatui::style::Color;
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);

        // Tree focused, so the track pane is not.
        let tracks = ui.tree.tracks_for(&ui.library, &store);
        ui.playback.current = tracks.first().copied();
        ui.tree.track_selected = 0;
        ui.tree.pane = Pane::Tree;

        let title = store.get(tracks[0]).unwrap().display_title().to_string();
        let (fg, _) = cursor_row_colours(&ui, &ctx, &title);
        assert_ne!(fg, Color::Green, "an unfocused pane looked focused");
    }
}
