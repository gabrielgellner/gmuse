//! M2 — the audio API.
//!
//! `PlayerHandle` (in `audio`) is the whole interface: load, play, pause,
//! stop, seek, volume, position, and an event stream. This binary drives it
//! from stdin lines, standing in for the UI until M4.
//!
//! Verification target from DESIGN §9: seek is sample-accurate and the
//! position display does not jump on seek.

mod audio;
mod cache;
mod cli;
mod library;
mod playlist;
mod repl;
mod scan;
mod store;
mod ui;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

use audio::{PlayerEvent, PlayerHandle, State};
use cli::Cli;
use library::Library;
use playlist::{Playlist, Queue};
use repl::Line;
use store::Store;

/// Everything the prompt operates on.
struct App {
    player: PlayerHandle,
    store: Store,
    roots: Vec<PathBuf>,
    cache_path: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Cli::parse();
    let player = PlayerHandle::new(args.player_config())?;
    println!("gmuse — device at {} Hz", player.device_rate());

    let cache_path =
        (!args.no_cache).then(|| args.cache.clone().unwrap_or_else(cache::default_path));

    // Load the cached library, then reconcile it with the roots we were given.
    let (store, mut roots) = match cache_path.as_deref().map(cache::load) {
        Some(Ok(Some((store, roots)))) => (store, roots),
        Some(Err(e)) => {
            eprintln!("cache: {e}");
            (Store::new(), Vec::new())
        }
        _ => (Store::new(), Vec::new()),
    };
    for root in &args.library {
        if !roots.contains(root) {
            roots.push(root.clone());
        }
    }

    let mut app = App {
        player,
        store,
        roots,
        cache_path,
    };

    if !app.roots.is_empty() {
        if args.rescan {
            // Forget mtimes so every file is re-read.
            app.store = Store::new();
        }
        let roots = app.roots.clone();
        let report = scan::scan(&mut app.store, &roots);
        println!("library: {report}");
        report_errors(&report);
        save_cache(&app);
    } else if !app.store.is_empty() {
        println!("library: {} tracks (cached)", app.store.len());
    }

    if !args.repl {
        // A file named on the command line still plays, even though the views
        // themselves cannot start playback until M5 — silently ignoring an
        // explicit argument would be worse than the milestone boundary.
        if let Some(path) = &args.file {
            app.player.load(path.clone());
        }
        return run_tui(&app, args.smart_artist_sort);
    }

    match &args.file {
        Some(path) => app.player.load(path.clone()),
        None => println!("{}", repl::HELP),
    }

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        drain_events(&app.player);

        print!("> ");
        std::io::stdout().flush()?;

        let Some(line) = lines.next() else { break };
        let line = line?;

        match repl::parse(&line) {
            Ok(Line::Quit) => break,
            Ok(Line::Empty) => {}
            Ok(cmd) => run(&mut app, cmd),
            Err(e) => println!("{e}"),
        }

        // Give the decoder a moment so its reply lands in this iteration
        // rather than looking like it belongs to the next command.
        std::thread::sleep(Duration::from_millis(60));
        drain_events(&app.player);
    }

    save_cache(&app);
    Ok(())
}

fn run(app: &mut App, cmd: Line) {
    let player = &app.player;
    match cmd {
        Line::Load(p) => player.load(p),
        Line::Play => player.play(),
        Line::Pause => player.pause(),
        Line::Toggle => player.toggle(),
        Line::Stop => player.stop(),
        Line::Seek(to) => player.seek(to),
        Line::Volume(v) => {
            player.set_volume(v);
            println!("volume {:.2}", player.volume());
        }
        Line::Status => print_status(player),
        Line::Scan(dir) => {
            if let Some(dir) = dir
                && !app.roots.contains(&dir)
            {
                app.roots.push(dir);
            }
            if app.roots.is_empty() {
                println!("no library roots — try 'scan <dir>'");
                return;
            }
            let roots = app.roots.clone();
            let report = scan::scan(&mut app.store, &roots);
            println!("{report}");
            report_errors(&report);
            save_cache(app);
        }
        Line::Library => print_library(app),
        Line::Find(query) => print_matches(app, &query),
        Line::Help => println!("{}", repl::HELP),
        Line::Quit | Line::Empty => {}
    }
}

/// Draw the views until the user quits.
///
/// The terminal is restored by the panic hook `ratatui::init` installs, so a
/// panic mid-render leaves a usable terminal rather than a wedged one.
fn run_tui(app: &App, smart_artist_sort: bool) -> Result<(), Box<dyn std::error::Error>> {
    let library = Library::rebuild(&app.store, smart_artist_sort);
    let playlist = Playlist::new();
    let queue = Queue::new();
    let start_dir = app
        .roots
        .first()
        .cloned()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));

    let ctx = ui::Context {
        store: &app.store,
        library: &library,
        playlist: &playlist,
        queue: &queue,
    };
    let mut state = ui::Ui::new(start_dir);

    let mut terminal = ratatui::init();
    let result = ui::run(&mut terminal, &mut state, &ctx);
    ratatui::restore();
    result?;
    Ok(())
}

fn save_cache(app: &App) {
    if let Some(path) = &app.cache_path
        && let Err(e) = cache::save(path, &app.store, &app.roots)
    {
        eprintln!("cache: {e}");
    }
}

/// Show the first few failures rather than a wall of them; a systematically
/// broken library produces thousands of identical lines otherwise.
fn report_errors(report: &scan::ScanReport) {
    for (path, err) in report.errors.iter().take(5) {
        println!("  skipped {}: {err}", path.display());
    }
    if report.errors.len() > 5 {
        println!("  ... and {} more", report.errors.len() - 5);
    }
}

fn print_library(app: &App) {
    use std::collections::BTreeSet;
    let mut artists: BTreeSet<&str> = BTreeSet::new();
    let mut albums: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut total = Duration::ZERO;
    let mut untagged = 0usize;

    for (_, t) in app.store.iter() {
        artists.insert(t.file_under());
        albums.insert((t.file_under(), t.album_or_unknown()));
        total += t.duration;
        if t.title.is_none() {
            untagged += 1;
        }
    }

    println!(
        "{} tracks, {} albums, {} artists, {:.1} hours",
        app.store.len(),
        albums.len(),
        artists.len(),
        total.as_secs_f64() / 3600.0
    );
    if untagged > 0 {
        println!("{untagged} without a title tag");
    }
    if app.store.tombstones() > 0 {
        println!(
            "{} removed this session ({} ids in use)",
            app.store.tombstones(),
            app.store.capacity()
        );
    }
    for root in &app.roots {
        println!("  root: {}", root.display());
    }
}

fn print_matches(app: &App, query: &str) {
    let needle = query.to_lowercase();
    let hit = |s: Option<&str>| {
        s.map(|s| s.to_lowercase().contains(&needle))
            .unwrap_or(false)
    };

    let mut shown = 0;
    let mut total = 0;
    for (_, t) in app.store.iter() {
        if hit(t.artist.as_deref())
            || hit(t.albumartist.as_deref())
            || hit(t.album.as_deref())
            || hit(t.title.as_deref())
        {
            total += 1;
            if shown < 20 {
                println!(
                    "  {} — {} — {}",
                    t.file_under(),
                    t.album_or_unknown(),
                    t.display_title()
                );
                shown += 1;
            }
        }
    }
    if total == 0 {
        println!("no matches");
    } else if total > shown {
        println!("  ... {total} matches, showing {shown}");
    }
}

fn print_status(player: &PlayerHandle) {
    let state = match player.state() {
        State::Playing => "playing",
        State::Paused => "paused",
        State::Stopped => "stopped",
    };
    println!(
        "{state}  {}  vol {:.2}  underruns {}",
        fmt_time(player.position()),
        player.volume(),
        player.underruns()
    );
}

fn drain_events(player: &PlayerHandle) {
    while let Ok(event) = player.events().try_recv() {
        match event {
            PlayerEvent::Loaded {
                path,
                duration,
                source_rate,
                channels,
                resampling,
            } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let len = duration.map(fmt_time).unwrap_or_else(|| "??:??".into());
                let conv = if resampling { " (resampling)" } else { "" };
                println!("\n♪ {name}  [{len}]  {source_rate} Hz, {channels} ch{conv}");
            }
            PlayerEvent::Finished => println!("\nfinished"),
            PlayerEvent::Error(e) => println!("\nerror: {e}"),
            PlayerEvent::DeviceLost(e) => println!("\ndevice lost: {e}"),
        }
    }
}

/// `mm:ss.mmm` — millisecond resolution, because second resolution cannot show
/// whether a seek landed where it was asked to.
fn fmt_time(d: Duration) -> String {
    let total = d.as_secs_f64();
    let mins = (total / 60.0).floor();
    format!("{:02}:{:06.3}", mins as u64, total - mins * 60.0)
}
