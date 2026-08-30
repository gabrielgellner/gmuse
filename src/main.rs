//! M2 — the audio API.
//!
//! `PlayerHandle` (in `audio`) is the whole interface: load, play, pause,
//! stop, seek, volume, position, and an event stream. This binary drives it
//! from stdin lines, standing in for the UI until M4.
//!
//! Verification target from DESIGN §9: seek is sample-accurate and the
//! position display does not jump on seek.

mod audio;
mod audit;
mod cache;
mod catalog;
mod cli;
mod cmd;
mod config;
mod format;
mod library;
mod next;
mod playlist;
mod repl;
mod scan;
mod session;
mod store;
mod ui;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

use audio::{PlayerEvent, PlayerHandle, State};
use cli::Cli;
use library::Library;
use repl::Line;
use store::Store;

/// Everything the prompt operates on.
struct App {
    player: PlayerHandle,
    store: Store,
    roots: Vec<PathBuf>,
    cache_path: Option<PathBuf>,
    catalogs: catalog::Catalogs,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Cli::parse();
    let player = PlayerHandle::new(args.player_config())?;
    println!("gmuse — device at {} Hz", player.device_rate());

    // Read the config first: it defines the catalogs, which decide what to
    // scan.
    let loaded = if args.no_config {
        config::Loaded::default()
    } else {
        config::load(&args.config.clone().unwrap_or_else(config::default_path))
    };

    let cache_path =
        (!args.no_cache).then(|| args.cache.clone().unwrap_or_else(cache::default_path));

    // Load the cached library, then reconcile it with the roots we were given.
    let (store, mut roots, cached_active) = match cache_path.as_deref().map(cache::load) {
        Some(Ok(cache::Outcome::Loaded {
            store,
            roots,
            active,
        })) => (store, roots, Some(active)),
        Some(Ok(cache::Outcome::Discarded)) => {
            // Costs a rescan, so say so. Silently starting from nothing is how
            // a library appears to vanish.
            eprintln!("cache: discarded (wrong version or corrupt); rescanning");
            (Store::new(), Vec::new(), None)
        }
        Some(Err(e)) => {
            eprintln!("cache: {e}");
            (Store::new(), Vec::new(), None)
        }
        _ => (Store::new(), Vec::new(), None),
    };
    // Catalogs come from the config; `--library` roots join an implicit
    // "default" catalog so a user who configures none sees no change.
    let mut catalogs = loaded.catalogs.clone();
    let config_chose_catalogs = loaded.active_specified;
    if !args.library.is_empty() {
        catalogs.define(
            catalog::DEFAULT,
            args.library.iter().map(|p| p.to_path_buf()).collect(),
        );
    }
    // A remembered choice wins over the config default, unless the config
    // names one explicitly — otherwise picking a catalog would not stick.
    if let Some(active) = cached_active
        && !config_chose_catalogs
    {
        catalogs.set_active(active);
    }

    // Scanning covers every catalog root regardless of what is active, so
    // switching catalogs later needs no rescan.
    for root in catalogs.all_roots() {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }

    let mut app = App {
        player,
        store,
        roots,
        cache_path,
        catalogs,
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
    } else {
        println!(
            "library: empty — scan one with `gmuse --library <dir>`, or set\n\
             `library = [\"~/Music\"]` in {}",
            config::default_path().display()
        );
    }

    if !args.repl {
        // A file named on the command line still plays, even though the views
        // themselves cannot start playback until M5 — silently ignoring an
        // explicit argument would be worse than the milestone boundary.
        if let Some(path) = &args.file {
            app.player
                .load(crate::audio::TrackSource::plain(path.clone()));
        }
        return run_tui(
            &app,
            args.smart_artist_sort,
            loaded,
            args.log_path(),
            !args.no_resume,
        );
    }

    // The config sets up UI state, which the line prompt does not have. Say so
    // rather than loading it and silently doing nothing.
    if !loaded.commands.is_empty() {
        println!(
            "note: {} config command(s) ignored — the config applies to the TUI, not --repl",
            loaded.commands.len()
        );
    }
    for e in &loaded.errors {
        eprintln!("config: {e}");
    }
    // Same shape as the config: the log records UI-loop events, which the line
    // prompt has none of. Say so rather than opening a file nothing writes to.
    if args.log_path().is_some() {
        println!("note: --log applies to the TUI, not --repl");
    }

    match &args.file {
        Some(path) => app
            .player
            .load(crate::audio::TrackSource::plain(path.clone())),
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
        Line::Load(p) => player.load(crate::audio::TrackSource::plain(p)),
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
        Line::Rescan(dir) => {
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
fn run_tui(
    app: &App,
    smart_artist_sort: bool,
    loaded: config::Loaded,
    log_path: Option<PathBuf>,
    resume: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let library = Library::from_members(
        &app.store,
        app.catalogs.members(&app.store),
        smart_artist_sort,
    );
    let start_dir = app
        .roots
        .first()
        .cloned()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));

    let ctx = ui::Context {
        store: &app.store,
        player: Some(&app.player),
    };
    let mut state = ui::Ui::new(start_dir, library);
    state.catalogs = app.catalogs.clone();

    // Opened before the terminal is taken over, so a failure is visible.
    if let Some(path) = log_path {
        match audit::Audit::open(&path) {
            Ok(mut audit) => {
                audit.line("device", &format!("{} Hz output", app.player.device_rate()));
                audit.line(
                    "library",
                    &format!(
                        "{} tracks, catalogs: {}",
                        app.store.len(),
                        app.catalogs.label().unwrap_or_else(|| "all".into())
                    ),
                );
                println!("logging to {}", path.display());
                state.audit = audit;
            }
            Err(e) => eprintln!("log: cannot open {}: {e}", path.display()),
        }
    }

    state.theme = loaded.theme;
    state.now_playing = loaded.now_playing.clone();

    // Which settings the config actually spoke about. The resumption rule is
    // "config wins where it speaks, the session fills the rest": writing a
    // setting in config.toml should take effect immediately rather than being
    // shadowed by whatever a stale session file remembers. Collected before
    // the commands are consumed below.
    let fixed: std::collections::HashSet<cmd::SettingKey> = loaded
        .commands
        .iter()
        .filter_map(|c| match c {
            cmd::Command::Set { key, .. } => Some(*key),
            _ => None,
        })
        .collect();

    // The config's bindings and settings are commands, so they run through the
    // same execute() a keypress does — there is no second configuration path
    // to keep in step.
    for cmd in loaded.commands {
        ui::execute(&mut state, &ctx, cmd);
    }
    if !loaded.errors.is_empty() {
        state.notify(format!("{} problem(s) in the config", loaded.errors.len()));
        for e in &loaded.errors {
            eprintln!("config: {e}");
        }
    }

    // Where we were last time, restored paused. After the config, so `fixed`
    // means something; before the settings summary, so the summary reports
    // what is actually in force.
    if resume {
        let path = session::default_path();
        if let Some(saved) = session::load(&path) {
            ui::restore_session(&mut state, &ctx, &saved, &fixed);
            state.audit.line(
                "session",
                &format!(
                    "restored {} queued, {}",
                    saved.queue.len(),
                    saved
                        .current
                        .as_ref()
                        .map(|p| format!("at {:.1}s in {}", saved.position_secs, p.display()))
                        .unwrap_or_else(|| "nothing playing".into())
                ),
            );
        }
        state.session_path = Some(path);
    }

    // *After* the config has run, or this records the defaults rather than
    // what is actually in force. The settings that decide how loud things
    // play: `replaygain=auto` resolves against `shuffle`, so neither alone
    // says which figure a gain came from.
    state.audit.line(
        "settings",
        &format!(
            "replaygain={} shuffle={} aaa_mode={}",
            state.replaygain.label(),
            state.playback.shuffle,
            state.playback.aaa_mode.label(),
        ),
    );

    let mut terminal = ratatui::init();
    let result = ui::run(&mut terminal, &mut state, &ctx);
    ratatui::restore();
    // Written after the terminal is restored, so a failure is visible.
    ui::save_session(&mut state, &ctx);
    // Written after the terminal is restored, so the summary is visible.
    if state.audit.is_enabled() {
        let underruns = app.player.underruns();
        state
            .audit
            .line("session", &format!("ended, {underruns} underruns in total"));
    }
    result?;
    Ok(())
}

fn save_cache(app: &App) {
    // Never write an empty cache. With no roots there is nothing to record,
    // and overwriting a good cache with an empty one loses the roots stored in
    // it — which is exactly how a working library turns into an empty one
    // after a format bump.
    if app.store.is_empty() && app.roots.is_empty() {
        return;
    }
    if let Some(path) = &app.cache_path
        && let Err(e) = cache::save(path, &app.store, &app.roots, app.catalogs.active())
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

    // Report what the active catalogs actually cover, not the whole store —
    // otherwise this contradicts what the views show.
    let members = app.catalogs.members(&app.store);
    let mut counted = 0usize;

    for (id, t) in app.store.iter() {
        if !members.contains(&id) {
            continue;
        }
        counted += 1;
        artists.insert(t.file_under());
        albums.insert((t.file_under(), t.album_or_unknown()));
        total += t.duration;
        if t.title.is_none() {
            untagged += 1;
        }
    }

    println!(
        "{} tracks, {} albums, {} artists, {:.1} hours",
        counted,
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
    if let Some(label) = app.catalogs.label() {
        println!("  catalogs: {label}");
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
    // Both numbers: the fader position is what was set, the gain is what the
    // samples are actually multiplied by, and the two are no longer the same.
    let gain = player.gain();
    let db = if gain > 0.0 {
        format!("{:+.1} dB", 20.0 * gain.log10())
    } else {
        "silent".into()
    };
    println!(
        "{state}  {}  vol {:.0}% ({db})  underruns {}",
        fmt_time(player.position()),
        player.volume() * 100.0,
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
                gain: _,
            } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let len = duration.map(fmt_time).unwrap_or_else(|| "??:??".into());
                let conv = if resampling { " (resampling)" } else { "" };
                println!("\n♪ {name}  [{len}]  {source_rate} Hz, {channels} ch{conv}");
            }
            // `--repl` plays whatever file it was handed, one at a time. It
            // has no pool to resolve a successor from, so decline the splice
            // and let the track finish.
            PlayerEvent::NeedNext => player.set_next(None),
            PlayerEvent::Finished => println!("\nfinished"),
            PlayerEvent::Error(e) => println!("\nerror: {e}"),
            PlayerEvent::DeviceLost(e) => println!("\ndevice lost: {e}"),
            PlayerEvent::DeviceChanged => println!("\naudio output changed"),
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
