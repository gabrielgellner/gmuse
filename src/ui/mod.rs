//! The terminal UI — DESIGN §8 `ui/`.
//!
//! M4 scope: views 1–5, navigation, `Tab`, `Space`. Read-only; nothing here
//! starts playback, which is M5's job.

pub mod list;
pub mod statusline;
pub mod tree;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};

use crate::library::Library;
use crate::playlist::{Playlist, Queue};
use crate::store::Store;
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
}

impl View {
    pub fn title(self) -> &'static str {
        match self {
            View::Tree => "library",
            View::Sorted => "sorted",
            View::Playlist => "playlist",
            View::Queue => "queue",
            View::Browser => "browser",
        }
    }

    fn from_digit(c: char) -> Option<Self> {
        Some(match c {
            '1' => View::Tree,
            '2' => View::Sorted,
            '3' => View::Playlist,
            '4' => View::Queue,
            '5' => View::Browser,
            _ => return None,
        })
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
    quit: bool,
}

impl Ui {
    pub fn new(start_dir: PathBuf) -> Self {
        Self {
            view: View::Tree,
            tree: TreeState::default(),
            sorted: 0,
            playlist_sel: 0,
            queue_sel: 0,
            browser: BrowserState::at(start_dir),
            status: String::new(),
            quit: false,
        }
    }
}

/// What the UI needs to render. Borrowed, never owned — the library is the
/// application's, not the UI's.
pub struct Context<'a> {
    pub store: &'a Store,
    pub library: &'a Library,
    pub playlist: &'a Playlist,
    pub queue: &'a Queue,
}

/// Run the event loop until the user quits.
pub fn run<B>(terminal: &mut Terminal<B>, ui: &mut Ui, ctx: &Context<'_>) -> io::Result<()>
where
    B: ratatui::backend::Backend<Error = io::Error>,
{
    while !ui.quit {
        terminal.draw(|frame| draw(frame, ui, ctx))?;
        // Poll rather than block, so a future status refresh (M5's position
        // display) has somewhere to happen.
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.is_press()
        {
            handle_key(key, ui, ctx);
        }
    }
    Ok(())
}

fn handle_key(key: KeyEvent, ui: &mut Ui, ctx: &Context<'_>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let page = 10usize;

    // View switching and quit are global.
    match key.code {
        KeyCode::Char(c) if View::from_digit(c).is_some() => {
            ui.view = View::from_digit(c).unwrap();
            ui.status.clear();
            return;
        }
        KeyCode::Char('q') => {
            ui.quit = true;
            return;
        }
        _ => {}
    }

    let len = current_len(ui, ctx);
    let cursor = current_cursor(ui);

    let moved = match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            Some(cursor.saturating_add(1).min(len.saturating_sub(1)))
        }
        KeyCode::Char('k') | KeyCode::Up => Some(cursor.saturating_sub(1)),
        KeyCode::Char('g') | KeyCode::Home => Some(0),
        KeyCode::Char('G') | KeyCode::End => Some(len.saturating_sub(1)),
        KeyCode::Char('f') if ctrl => Some((cursor + page).min(len.saturating_sub(1))),
        KeyCode::Char('b') if ctrl => Some(cursor.saturating_sub(page)),
        KeyCode::PageDown => Some((cursor + page).min(len.saturating_sub(1))),
        KeyCode::PageUp => Some(cursor.saturating_sub(page)),
        _ => None,
    };

    if let Some(next) = moved {
        set_cursor(ui, next);
        return;
    }

    match key.code {
        KeyCode::Tab if ui.view == View::Tree => {
            ui.tree.pane = match ui.tree.pane {
                Pane::Tree => Pane::Tracks,
                Pane::Tracks => Pane::Tree,
            };
        }
        KeyCode::Char(' ') if ui.view == View::Tree && ui.tree.pane == Pane::Tree => {
            ui.tree.toggle(ctx.library, ctx.store);
        }
        KeyCode::Enter => match ui.view {
            View::Browser => ui.browser.enter(),
            // Playback is wired up in M5; saying so beats doing nothing.
            _ => ui.status = "playback lands in M5".into(),
        },
        KeyCode::Char('E') if ui.view == View::Tree => ui.tree.expand_all(ctx.library),
        KeyCode::Char('C') if ui.view == View::Tree => {
            ui.tree.collapse_all();
            ui.tree.selected = 0;
        }
        _ => {}
    }
}

fn current_len(ui: &Ui, ctx: &Context<'_>) -> usize {
    match ui.view {
        View::Tree => match ui.tree.pane {
            Pane::Tree => ui.tree.rows(ctx.library, ctx.store).len(),
            Pane::Tracks => ui.tree.tracks_for(ctx.library, ctx.store).len(),
        },
        View::Sorted => ctx.library.sorted().len(),
        View::Playlist => ctx.playlist.len(),
        View::Queue => ctx.queue.len(),
        View::Browser => ui.browser.entries.len(),
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
    }
}

pub fn draw(frame: &mut Frame, ui: &Ui, ctx: &Context<'_>) {
    let [tabs, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(statusline::tabs(ui.view), tabs);
    frame.render_widget(statusline::status(ui, ctx), status);

    match ui.view {
        View::Tree => draw_tree(frame, body, ui, ctx),
        View::Sorted => draw_tracks(
            frame,
            body,
            ui,
            ctx,
            ctx.library.sorted(),
            ui.sorted,
            "sorted",
        ),
        View::Playlist => draw_tracks(
            frame,
            body,
            ui,
            ctx,
            ctx.playlist.entries(),
            ui.playlist_sel,
            "playlist",
        ),
        View::Queue => {
            let entries: Vec<_> = ctx.queue.entries().copied().collect();
            draw_tracks(frame, body, ui, ctx, &entries, ui.queue_sel, "queue")
        }
        View::Browser => draw_browser(frame, body, ui),
    }
}

fn draw_tree(frame: &mut Frame, area: Rect, ui: &Ui, ctx: &Context<'_>) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

    let rows = ui.tree.rows(ctx.library, ctx.store);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| ListItem::new(Line::from(row.label())))
        .collect();

    let focused = ui.tree.pane == Pane::Tree;
    let mut state = ListState::default().with_selected(Some(ui.tree.selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block("artist / album", focused))
            .highlight_style(highlight(focused)),
        left,
        &mut state,
    );

    let tracks = ui.tree.tracks_for(ctx.library, ctx.store);
    let items = list::track_items(ctx.store, &tracks);
    let mut state = ListState::default().with_selected(Some(ui.tree.track_selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block("tracks", !focused))
            .highlight_style(highlight(!focused)),
        right,
        &mut state,
    );
}

fn draw_tracks(
    frame: &mut Frame,
    area: Rect,
    _ui: &Ui,
    ctx: &Context<'_>,
    ids: &[crate::store::TrackId],
    selected: usize,
    title: &str,
) {
    if ids.is_empty() {
        frame.render_widget(
            Paragraph::new(empty_hint(title)).block(pane_block(title, true)),
            area,
        );
        return;
    }
    let items = list::track_items(ctx.store, ids);
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(pane_block(title, true))
            .highlight_style(highlight(true)),
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
            .block(pane_block(&ui.browser.cwd.display().to_string(), true))
            .highlight_style(highlight(true)),
        area,
        &mut state,
    );
}

fn empty_hint(title: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  the {title} is empty"),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  adding lands with the command layer in M6",
            Style::default().fg(Color::DarkGray),
        )),
    ]
}

fn pane_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
}

/// The unfocused pane keeps a dimmer highlight rather than losing it, so you
/// can still see where you left the cursor after `Tab`.
fn highlight(focused: bool) -> Style {
    if focused {
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Track;
    use ratatui::backend::TestBackend;
    use std::time::Duration;

    /// A library with the three things §9 says the tree must get right: a
    /// compilation, a multi-disc album, and a track with no tags at all.
    fn fixture() -> (Store, Library) {
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

        let lib = Library::rebuild(&store, true);
        (store, lib)
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
        let (store, library) = fixture();
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let ctx = Context {
            store: &store,
            library: &library,
            playlist: &playlist,
            queue: &queue,
        };
        let ui = Ui::new(PathBuf::from("/m"));
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
        let (store, library) = fixture();
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let ctx = Context {
            store: &store,
            library: &library,
            playlist: &playlist,
            queue: &queue,
        };
        let mut ui = Ui::new(PathBuf::from("/m"));
        // Select "Various Artists" and expand it.
        let rows = ui.tree.rows(&library, &store);
        ui.tree.selected = rows
            .iter()
            .position(|r| r.label().contains("Various Artists"))
            .unwrap();
        ui.tree.toggle(&library, &store);

        let out = render(&ui, &ctx, 74, 12);
        assert!(out.contains("▾ Various Artists"), "{out}");
        assert!(out.contains("YouTube Sessions"), "{out}");
        // Both tracks show, despite having different `artist` tags.
        assert!(out.contains("The Mountain"), "{out}");
        assert!(out.contains("Years"), "{out}");
    }

    #[test]
    fn multi_disc_album_does_not_interleave() {
        let (store, library) = fixture();
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let ctx = Context {
            store: &store,
            library: &library,
            playlist: &playlist,
            queue: &queue,
        };
        let mut ui = Ui::new(PathBuf::from("/m"));
        let rows = ui.tree.rows(&library, &store);
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
        let (store, library) = fixture();
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let ctx = Context {
            store: &store,
            library: &library,
            playlist: &playlist,
            queue: &queue,
        };
        let mut ui = Ui::new(PathBuf::from("/m"));
        let rows = ui.tree.rows(&library, &store);
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
        let (store, library) = fixture();
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let ctx = Context {
            store: &store,
            library: &library,
            playlist: &playlist,
            queue: &queue,
        };
        let mut ui = Ui::new(PathBuf::from("/m"));
        ui.view = View::Playlist;
        let out = render(&ui, &ctx, 74, 10);
        assert!(out.contains("the playlist is empty"), "{out}");
        assert!(out.contains("3:playlist"), "{out}");
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        let (store, library) = fixture();
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let ctx = Context {
            store: &store,
            library: &library,
            playlist: &playlist,
            queue: &queue,
        };
        let mut ui = Ui::new(PathBuf::from("/m"));
        let rows = ui.tree.rows(&library, &store).len();

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
    fn tab_moves_focus_and_digits_switch_views() {
        let (store, library) = fixture();
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let ctx = Context {
            store: &store,
            library: &library,
            playlist: &playlist,
            queue: &queue,
        };
        let mut ui = Ui::new(PathBuf::from("/m"));
        assert_eq!(ui.tree.pane, Pane::Tree);
        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        assert_eq!(ui.tree.pane, Pane::Tracks);
        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        assert_eq!(ui.tree.pane, Pane::Tree);

        for (c, want) in [
            ('2', View::Sorted),
            ('4', View::Queue),
            ('5', View::Browser),
            ('1', View::Tree),
        ] {
            handle_key(KeyEvent::from(KeyCode::Char(c)), &mut ui, &ctx);
            assert_eq!(ui.view, want);
        }
    }

    /// Moving in the tree must reset the track cursor: the old index would
    /// point into a different album's list.
    #[test]
    fn moving_the_tree_cursor_resets_the_track_pane() {
        let (store, library) = fixture();
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let ctx = Context {
            store: &store,
            library: &library,
            playlist: &playlist,
            queue: &queue,
        };
        let mut ui = Ui::new(PathBuf::from("/m"));
        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        handle_key(KeyEvent::from(KeyCode::Char('j')), &mut ui, &ctx);
        assert_eq!(ui.tree.track_selected, 1);

        handle_key(KeyEvent::from(KeyCode::Tab), &mut ui, &ctx);
        handle_key(KeyEvent::from(KeyCode::Char('j')), &mut ui, &ctx);
        assert_eq!(ui.tree.track_selected, 0, "stale track cursor after moving");
    }
}
