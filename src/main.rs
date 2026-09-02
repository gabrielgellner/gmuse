//! M2 — the audio API.
//!
//! `PlayerHandle` (in `audio`) is the whole interface: load, play, pause,
//! stop, seek, volume, position, and an event stream. This binary drives it
//! from stdin lines, standing in for the UI until M4.
//!
//! Verification target from DESIGN §9: seek is sample-accurate and the
//! position display does not jump on seek.

mod audio;
mod audio_id;
mod audit;
mod cache;
mod catalog;
mod cli;
mod cmd;
mod config;
mod datadir;
mod events;
mod filter;
mod format;
mod guides;
mod history;
mod library;
mod lock;
mod m3u;
mod next;
mod playlist;
mod remote;
mod scan;
mod session;
mod smart;
mod sort;
mod store;
mod text;
mod ui;
mod undo;

use std::path::PathBuf;

use clap::Parser;

use audio::PlayerHandle;
use cli::Cli;
use library::Library;
use store::Store;

/// Everything the prompt operates on.
struct App {
    player: PlayerHandle,
    store: Store,
    roots: Vec<PathBuf>,
    cache_path: Option<PathBuf>,
    catalogs: catalog::Catalogs,
    /// Files recognised at a new path by the startup scan.
    ///
    /// Carried here because the scan runs before the history is loaded, and
    /// the history is what most needs to know (#25).
    moves: Vec<scan::Move>,
    /// Files that looked like audio and could not be read at all.
    ///
    /// Carried for the same reason as `moves`: the scan happens before the TUI
    /// exists, and printing to a terminal that is about to be taken over by a
    /// full-screen program is the same as not saying it (#53).
    unreadable: Vec<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Cli::parse();

    // Before the lock, and before anything is opened: a remote client is not a
    // player. It writes one line to the running player and prints the reply.
    if let Some(line) = &args.remote {
        return match remote::send(&remote::default_path(), line) {
            Ok(reply) => {
                println!("{reply}");
                // A refusal from the player is the client's failure too, or a
                // status bar would show a stale line and call it success.
                if reply.starts_with("err ") {
                    std::process::exit(1);
                }
                Ok(())
            }
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        };
    }

    // Before the device is opened, so a refused second instance never makes a
    // sound and never touches state. Held for the whole of `main`: the binding
    // must outlive the player, and the kernel drops it whichever way we exit.
    let _lock = if args.no_lock {
        None
    } else {
        match lock::acquire(&lock::default_path()) {
            Ok(held) => Some(held),
            Err(err @ lock::LockError::Held { .. }) => {
                eprintln!("{err}");
                eprintln!(
                    "\nTwo players share one audio device and one session.toml, so this one\n\
                     stops here. Reach the running player instead, or pass --no-lock if you\n\
                     meant to have two."
                );
                // Exit rather than return: `main`'s error type prints its
                // Debug form, which would repeat all of the above as a struct
                // literal underneath the sentence written for a person.
                std::process::exit(1);
            }
            // Anything other than "someone has it" is a real failure — an
            // unwritable state directory, say. Refusing to start over it would
            // be worse than the problem, so say it and carry on unlocked.
            Err(err) => {
                eprintln!("warning: {err}; continuing without a lock");
                None
            }
        }
    };

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
    let (store, cached_roots, cached_active) = match cache_path.as_deref().map(cache::load) {
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
        catalogs.define(catalog::DEFAULT, args.library_roots(), None);
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
    //
    // The cache's roots are a **fallback**, not an addition: they exist so
    // that `gmuse -L ~/Music` once, then bare afterwards, keeps working. Adding
    // them to what the config and `--library` already say is what let a
    // one-off `-L` linger for ever, and a stale root that happened to be the
    // parent of two real ones made every file scan twice (#21).
    let roots = scan_roots(catalogs.all_roots(), cached_roots);

    let mut app = App {
        player,
        store,
        roots,
        cache_path,
        catalogs,
        moves: Vec::new(),
        unreadable: Vec::new(),
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
        app.moves = report.moves.clone();
        app.unreadable = report.errors.iter().map(|(p, _)| p.clone()).collect();
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

    run_tui(
        &app,
        args.smart_artist_sort,
        loaded,
        args.log_path(),
        !args.no_resume,
        args.migrate_data,
    )
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
    migrate_data: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // History is loaded before the library, not after: a catalog may be defined
    // by `play_count` or `days_since_added`, and building the projection
    // without it would silently give the wrong membership until something
    // happened to trigger a rebuild.
    let history_path = crate::history::path_in(&loaded.data_dir);
    // Before the first read, so an upgrade finds the ratings rather than an
    // empty file that looks exactly like having lost them.
    if let Some(from) = crate::history::migrate(&history_path) {
        println!(
            "history: moved {} -> {}",
            from.display(),
            history_path.display()
        );
    }
    // Did `data_dir` move out from under the data? Checked before the first
    // read, so an empty history is explained rather than merely reported.
    let state_dir = config::default_state_dir();
    match datadir::check(
        datadir::remembered(&state_dir).as_deref(),
        &loaded.data_dir,
        datadir::has_data,
    ) {
        datadir::Check::Fine => datadir::remember(&state_dir, &loaded.data_dir),
        datadir::Check::Stranded { from } => {
            if migrate_data {
                match datadir::move_data(&from, &loaded.data_dir) {
                    Ok(moved) => {
                        println!(
                            "data_dir: moved {} item(s) from {}",
                            moved.took.len(),
                            from.display()
                        );
                        for m in &moved.merged {
                            println!(
                                "  recovered {} event(s) into {}{}",
                                m.took,
                                m.at.display(),
                                match m.already {
                                    0 => String::new(),
                                    n => format!(" ({n} already had a home)"),
                                }
                            );
                        }
                        for left in &moved.left {
                            println!(
                                "  left {}: something of that name is already there",
                                left.display()
                            );
                        }
                        // Remembered only when nothing was left: otherwise the
                        // next launch should say so again.
                        if moved.left.is_empty() {
                            datadir::remember(&state_dir, &loaded.data_dir);
                        }
                    }
                    Err(e) => eprintln!("data_dir: {e}"),
                }
            } else {
                // Deliberately not remembering the new directory: a launch that
                // ignored this would otherwise never be warned again.
                eprintln!(
                    "data_dir: changed to {}, but {} still holds your ratings.\n\
                     Move them with `gmuse --migrate-data`, or by hand — both \
                     history.toml and events-*.log.",
                    loaded.data_dir.display(),
                    from.display()
                );
            }
        }
    }

    // The base plus every log, this machine's and any that arrived by sync.
    let machine = crate::events::machine_id(&config::default_state_dir());
    let events_path = crate::events::log_path(&loaded.data_dir, &machine);
    let (mut history, applied) = crate::events::folded(&history_path, &loaded.data_dir);
    if applied > 0 {
        println!("history: {applied} events");
    }
    let log = |event: crate::events::Kind| {
        if let Err(e) = crate::events::append(&events_path, &machine, crate::history::now(), event)
        {
            eprintln!("history: {e}");
        }
    };

    // Before `note_seen` below, which would otherwise create a fresh entry at
    // the new path and make the carried one lose to it — leaving the ratings
    // stranded under a path no file has.
    let mut carried = 0;
    for m in &app.moves {
        if history.rename(&m.from, &m.to) {
            carried += 1;
            // Recorded rather than re-inferred: another machine folding this
            // log has no scan to work it out from.
            log(crate::events::Kind::Moved(m.from.clone(), m.to.clone()));
        }
    }
    if carried > 0 {
        println!("history: carried {carried} across a move");
    }

    let first_run = history.is_empty();
    let known = history.len();
    let now = crate::history::now();
    for (_, track) in app.store.iter() {
        if history.note_seen(&track.path, now) {
            log(crate::events::Kind::Seen(track.path.clone()));
        }
    }
    let fresh = history.len() - known;

    let library = Library::from_members(
        &app.store,
        app.catalogs.members(
            &app.store,
            crate::filter::Env {
                history: Some(&history),
                now,
            },
        ),
        smart_artist_sort,
    );
    let start_dir = app
        .roots
        .first()
        .cloned()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));

    // A socket that will not bind is a warning, not a failure: the player is
    // still a player without one, and refusing to start over a missing remote
    // would be the tail wagging the dog.
    let server = match remote::serve(&remote::default_path()) {
        Ok(server) => Some(server),
        Err(err) => {
            eprintln!("warning: {err}; --remote will not reach this player");
            None
        }
    };

    let ctx = ui::Context {
        store: &app.store,
        player: Some(&app.player),
        remote: server.as_ref(),
    };
    let mut state = ui::Ui::new(start_dir, library);
    state.catalogs = app.catalogs.clone();
    state.playlists = loaded.playlists.clone();
    state.saved = Some(playlist::Playlists::new(&loaded.data_dir));

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

    state.set_leader(loaded.leader.unwrap_or_else(config::default_leader));
    state.theme = loaded.theme;
    state.now_playing = loaded.now_playing.clone();
    if let Some(t) = loaded.track_line.clone() {
        state.track_line = t;
    }

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
    // Said in the TUI, not just on the terminal it replaced. A file gmuse
    // cannot read is a song missing from the library, and the one place that
    // matters is the place you are looking at (#53).
    //
    // The count is enough to say here: the tag fallback means these are *in*
    // the library rather than absent, filed under "Unknown Artist" by their
    // file name, so the message points at where to find them rather than at a
    // list this UI has nowhere to put.
    if !app.unreadable.is_empty() {
        state.notify(format!(
            "{} file(s) had unreadable tags — filed under Unknown Artist",
            app.unreadable.len()
        ));
        for path in &app.unreadable {
            state.audit.line("unreadable", &path.display().to_string());
        }
    }
    if !loaded.errors.is_empty() {
        state.notify(format!("{} problem(s) in the config", loaded.errors.len()));
        for e in &loaded.errors {
            eprintln!("config: {e}");
        }
    }

    // Every track gets a first-seen date, so "recently added" means something
    // from here on. It cannot be recovered for an existing library: the file's
    // mtime is not it — writing ReplayGain tags moved every one of them.
    {
        let total = history.len();
        state.history = history;
        state.events_path = Some(events_path);
        state.machine = machine.clone();
        state.audit.line(
            "history",
            &if first_run {
                format!("{fresh} tracks, all first seen now — added dates start here")
            } else {
                format!("{total} known, {fresh} new since last run")
            },
        );
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

/// `mm:ss.mmm` — millisecond resolution, because second resolution cannot show
/// whether a seek landed where it was asked to.
/// What to scan: what the config and `--library` say, or — only if they say
/// nothing — what the cache remembered.
///
/// The cache's roots are a **fallback, not an addition**. They exist so that
/// `gmuse -L ~/Music` once and bare afterwards keeps working. Adding them to
/// what is already known is what let a one-off `-L` linger for ever, and a
/// stale root that happened to be the parent of two real ones made every file
/// scan twice (#21).
fn scan_roots(from_catalogs: Vec<PathBuf>, cached: Vec<PathBuf>) -> Vec<PathBuf> {
    if from_catalogs.is_empty() {
        cached
    } else {
        from_catalogs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented workflow: scan once with `--library`, then run bare.
    #[test]
    fn the_cache_supplies_roots_when_nothing_else_does() {
        let cached = vec![PathBuf::from("/m/Music")];
        assert_eq!(scan_roots(Vec::new(), cached.clone()), cached);
    }

    /// And the bug: a one-off `-L` used to join the remembered set for ever,
    /// so a stale parent root kept every file being walked twice (#21).
    #[test]
    fn configured_roots_replace_what_the_cache_remembered() {
        let cached = vec![
            PathBuf::from("/m"),
            PathBuf::from("~/MusicLibrary"),
            PathBuf::from("/tmp/one-off"),
        ];
        let configured = vec![PathBuf::from("/m/Dance"), PathBuf::from("/m/General")];
        assert_eq!(
            scan_roots(configured.clone(), cached),
            configured,
            "a stale root outlived the run that introduced it"
        );
    }

    #[test]
    fn nothing_anywhere_scans_nothing() {
        assert!(scan_roots(Vec::new(), Vec::new()).is_empty());
    }
}
