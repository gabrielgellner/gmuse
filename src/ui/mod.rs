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

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent};
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
}

impl Prompt {
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
    pub settings_sel: usize,
    pub theme: theme::Theme,
    /// The library projection. Owned rather than borrowed, because changing
    /// the active catalogs — or `smart_artist_sort` — rebuilds it.
    pub library: Library,
    pub catalogs: crate::catalog::Catalogs,
    /// Keep playing past the end of the current album/artist pool. Stored here
    /// because it is a setting the user toggles, not a property of a track.
    pub continue_playing: bool,
    /// Divide track lists by album. cmus does; the alternative is one
    /// undifferentiated column.
    pub album_headers: bool,
    pub smart_artist_sort: bool,
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
            settings_sel: 0,
            theme: theme::Theme::default(),
            library,
            catalogs: crate::catalog::Catalogs::default(),
            continue_playing: true,
            album_headers: true,
            smart_artist_sort: true,
            status_until: None,
            quit: false,
        }
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
        ui.expire_status();
    }
    Ok(())
}

/// Turn player events into UI state, and advance when a track ends.
fn drain_player_events(ui: &mut Ui, ctx: &Context<'_>) {
    let Some(player) = ctx.player else { return };
    let mut finished = false;
    while let Ok(event) = player.events().try_recv() {
        match event {
            crate::audio::PlayerEvent::Finished => finished = true,
            crate::audio::PlayerEvent::Error(e) => ui.notify(format!("error: {e}")),
            crate::audio::PlayerEvent::DeviceLost(e) => ui.notify(format!("device lost: {e}")),
            crate::audio::PlayerEvent::DeviceChanged => ui.notify("audio output changed"),
            crate::audio::PlayerEvent::Loaded { .. } => {}
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
    if let Some(player) = ctx.player {
        player.load(track.path.clone());
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
            });
            return;
        }
        KeyCode::Char('/') => {
            ui.prompt = Some(Prompt {
                kind: PromptKind::Search(SearchDir::Forward),
                buffer: String::new(),
            });
            return;
        }
        KeyCode::Char('?') => {
            ui.prompt = Some(Prompt {
                kind: PromptKind::Search(SearchDir::Backward),
                buffer: String::new(),
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
        // Wait for the rest of the chord. Showing it is what stops a
        // half-typed `g` feeling like a dropped keypress.
        bind::Resolved::Prefix => {
            let so_far = crate::cmd::keys::format_seq(&ui.pending);
            let prefix = match ui.count {
                Some(n) => format!("{n}{so_far}"),
                None => so_far,
            };
            ui.status = prefix;
            ui.status_until = None;
        }
        bind::Resolved::None => {
            ui.pending.clear();
            ui.count = None;
            ui.status.clear();
        }
    }
}

fn handle_prompt_key(key: KeyEvent, ui: &mut Ui, ctx: &Context<'_>) {
    let Some(prompt) = ui.prompt.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Esc => ui.prompt = None,
        KeyCode::Backspace => {
            if prompt.buffer.pop().is_none() {
                // Backspacing past the sigil closes the line, as in vi.
                ui.prompt = None;
            }
        }
        KeyCode::Char(c) => prompt.buffer.push(c),
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
        Command::WinDown => set_cursor(ui, (cursor + n).min(len.saturating_sub(1))),
        Command::WinUp => set_cursor(ui, cursor.saturating_sub(n)),
        Command::WinTop => set_cursor(ui, 0),
        // `G` goes to the end; `<count>G` goes to that row, 1-based, as in vim.
        Command::WinBottom => match count {
            Some(row) => set_cursor(ui, row.saturating_sub(1).min(len.saturating_sub(1))),
            None => set_cursor(ui, len.saturating_sub(1)),
        },
        Command::WinPageDown => set_cursor(ui, (cursor + page * n).min(len.saturating_sub(1))),
        Command::WinPageUp => set_cursor(ui, cursor.saturating_sub(page * n)),
        Command::WinNextPane => {
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
        Command::WinPaneLeft => {
            if ui.view == View::Tree {
                ui.tree.pane = Pane::Tree;
            }
        }
        Command::WinPaneRight => {
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
        Command::WinToggle => {
            if ui.view == View::Tree && ui.tree.pane == Pane::Tree {
                ui.tree.toggle(&ui.library, ctx.store);
            }
        }
        Command::WinExpandAll => {
            if ui.view == View::Tree {
                ui.tree.expand_all(&ui.library);
            }
        }
        Command::WinCollapseAll => {
            if ui.view == View::Tree {
                ui.tree.collapse_all();
                ui.tree.selected = 0;
            }
        }
        Command::WinActivate => match ui.view {
            View::Browser => ui.browser.enter(),
            View::Settings => {}
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
        Command::WinRemove => match ui.view {
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
        Command::WinUpdate | Command::Update => ui.notify("rescan lands with :update in the app"),

        // ---- transport
        Command::PlayerPlay => {
            if ui.playback.current.is_some() {
                if let Some(p) = ctx.player {
                    p.play();
                }
            } else if let Some(id) = ui.selected_track(ctx) {
                play(ui, ctx, id);
            }
        }
        Command::PlayerPause => {
            if let Some(p) = ctx.player {
                p.toggle();
            }
        }
        Command::PlayerStop => {
            if let Some(p) = ctx.player {
                p.stop();
            }
        }
        Command::PlayerNext => {
            for _ in 0..n {
                advance(ui, ctx, true);
            }
        }
        Command::PlayerPrev => {
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
fn rebuild_library(ui: &mut Ui, ctx: &Context<'_>) {
    let members = ui.catalogs.members(ctx.store);
    ui.library = Library::from_members(ctx.store, members, ui.smart_artist_sort);
    ui.tree.selected = 0;
    ui.tree.track_selected = 0;
    ui.sorted = 0;
    // Playlists and the queue are deliberately untouched: they hold TrackIds
    // straight into the Store and are meant to cross catalogs.
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
    match (key, value) {
        (SettingKey::AaaMode, SettingValue::Aaa(mode)) => ui.playback.aaa_mode = mode,
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
        View::Filters => 0,
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
        View::Filters => 0,
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
        View::Filters => {}
        View::Settings => ui.settings_sel = next,
    }
}

pub fn draw(frame: &mut Frame, ui: &Ui, ctx: &Context<'_>) {
    let [tabs, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

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
                title: "playlist",
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
        View::Filters => frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  filters arrive with the filter language in M8",
                    ui.theme.get(theme::Role::StatusDim),
                )),
            ])
            .block(pane_block(&ui.theme, "filters", true)),
            body,
        ),
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
        list::Gutter {
            relative: ui.relative_numbers,
            selected: spec.selected,
        },
    );
    let mut state =
        ListState::default().with_selected(list::row_of_track(&rows, spec.selected).or(Some(0)));
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(&ui.theme, spec.title, spec.focused))
            .highlight_style(highlight(&ui.theme, spec.focused)),
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
        type_line(&mut ui, &ctx, "bind common j win-top");
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
        type_line(&mut ui, &ctx, "bind queue j win-bottom");

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
        assert!(
            out.contains("player-") || out.contains("win-"),
            "no bindings visible:\n{out}"
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
        catalogs.define("paak", vec![PathBuf::from("/m/paak")]);
        catalogs.define("va", vec![PathBuf::from("/m/va")]);
        let mut ui = Ui::new(
            PathBuf::from("/m"),
            Library::from_members(store, catalogs.members(store), true),
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
        assert_eq!(ui.status, "g");

        // An unmatched follow-up clears rather than firing something random.
        handle_key(KeyEvent::from(KeyCode::Char('q')), &mut ui, &ctx);
        assert!(ui.pending.is_empty());
        assert!(!ui.quit, "the q leaked to its own binding");
        assert!(ui.status.is_empty());
    }

    #[test]
    fn a_count_shows_beside_a_pending_chord() {
        let store = fixture();
        let ctx = ctx_of(&store);
        let mut ui = ui_of(&store, true);
        press(&mut ui, &ctx, "4");
        handle_key(KeyEvent::from(KeyCode::Char('g')), &mut ui, &ctx);
        assert_eq!(ui.status, "4g");
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
        type_line(&mut ui, &ctx, "bind common \"G x\" win-top");

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
        type_line(&mut ui, &ctx, "bind common \"w w\" win-bottom");
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

        // `z` is player-prev by default.
        press(&mut ui, &ctx, ":");
        type_line(&mut ui, &ctx, "bind common \"z z\" win-bottom");
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
        type_line(&mut ui, &ctx, "in tree win-bottom");
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
}
