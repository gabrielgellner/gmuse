//! M2 — the audio API.
//!
//! `PlayerHandle` (in `audio`) is the whole interface: load, play, pause,
//! stop, seek, volume, position, and an event stream. This binary drives it
//! from stdin lines, standing in for the UI until M4.
//!
//! Verification target from DESIGN §9: seek is sample-accurate and the
//! position display does not jump on seek.

/// **What this process is holding** — #255, and only when asked for.
///
/// `gmuse-library`'s `accounting` allocator, installed here so the number is the *engine's*
/// rather than a library test's. #255 asked for exactly that, because a fold measured in
/// isolation is 122 MB and the engine's resident set is 390 MB, and nothing said what the
/// difference was.
#[cfg(feature = "measure-memory")]
#[global_allocator]
static ACCOUNTING: gmuse_library::accounting::Accounting = gmuse_library::accounting::Accounting;

/// Say what is held at a named point in startup — #255.
///
/// **The peak is the number that matters and it lands before the socket opens**, which is why
/// the periodic reporter could not see it: by its first tick the engine had already been to
/// 310.9 MB and come back to 120. A resident set tracks the peak, not the settled heap, so
/// where the peak comes from is the whole question.
#[cfg(feature = "measure-memory")]
fn mark(what: &str) {
    use gmuse_library::accounting::{live, mb, peak};
    eprintln!(
        "memory: {what:<28} {:>7.1} MB live  {:>7.1} MB peak",
        mb(live()),
        mb(peak())
    );
}

#[cfg(not(feature = "measure-memory"))]
fn mark(_what: &str) {}

use std::path::PathBuf;

use clap::Parser;

use gmuse::audio::PlayerHandle;
use gmuse::cli::Cli;
use gmuse::operations::scan::Holdings;
use gmuse::shelf::Shelf;

/// **Where playback is** — #219, I11.
///
/// Two arms rather than a `Box<dyn Transport>` because the local one is not only
/// a transport: `PlayerHandle` also answers the decoder's `NeedNext`, and that
/// path is named separately on `Context` precisely so what is temporary looks
/// temporary. An enum keeps both facts in one place instead of a handle and a
/// flag that could disagree.
enum Playback {
    /// This process owns the device, which is every launch that is not attached.
    Local(PlayerHandle),
    /// The engine owns it and this is a view. No device, no lock, and nothing
    /// it does on the way in or out disturbs what is playing.
    Attached(gmuse::ui::remote_transport::Remote),
}

impl Playback {
    fn transport(&self) -> &dyn gmuse::ui::transport::Transport {
        match self {
            Self::Local(player) => player,
            Self::Attached(remote) => remote,
        }
    }

    /// **The handle itself, and only when playback is here.**
    ///
    /// `None` when attached is not a gap: the engine answers `NeedNext` from the
    /// queue it owns (#232), which is where that responsibility went.
    fn gapless(&self) -> Option<&PlayerHandle> {
        match self {
            Self::Local(player) => Some(player),
            Self::Attached(_) => None,
        }
    }

    fn attached(&self) -> bool {
        matches!(self, Self::Attached(_))
    }

    /// What the output is running at, for the audit line.
    fn device_rate(&self) -> u32 {
        match self {
            Self::Local(player) => player.device_rate(),
            Self::Attached(remote) => remote.device_rate(),
        }
    }
}

/// Everything the prompt operates on.
struct App {
    player: Playback,
    store: Holdings,
    roots: Vec<PathBuf>,
    cache_path: Option<PathBuf>,
    /// The view in force, as its source line.
    view: Option<String>,
    /// Files that looked like audio and could not be read at all.
    ///
    /// Carried for the same reason as `moves`: the scan happens before the TUI
    /// exists, and printing to a terminal that is about to be taken over by a
    /// full-screen program is the same as not saying it (#53).
    unreadable: Vec<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Cli::parse();

    // **A library operation is not a launch**, so it happens before anything a
    // launch needs: no audio device, no cache, no store scan. `apply` does its
    // own scan of exactly the roots it was given, and opening a device for a
    // command that plays nothing was eighteen seconds and a hardware claim for
    // no reason.
    //
    // **Before the lock, too** (#62), and for the reason the remote client is:
    // it opens no device and writes only the log, so refusing it because a
    // player is running would make `import` and `apply` unusable exactly when
    // somebody is listening to the library they are editing.
    // **The engine, before the lock and before any device is opened.**
    //
    // It takes its own lock — `engine.lock`, beside its socket — which is a
    // different question from the player's: that one asks "is a *player*
    // holding this device", this one asks "is an *engine* already listening".
    // Conflating them would make the engine refuse to start while a TUI was
    // open, which is precisely the arrangement §4.2 is heading for.
    // **`ctl` and `events` before everything**, for the reason the remote
    // client already goes first: they open no device, take no lock and write
    // nothing. They are clients, and a client that had to wait for a player to
    // start would not be one.
    if let Some(gmuse::cli::Command::Ctl { words, json }) = &args.command {
        return run_ctl(words, *json);
    }
    if let Some(gmuse::cli::Command::Events { topics, count }) = &args.command {
        return run_events(topics, *count);
    }

    if let Some(gmuse::cli::Command::Engine {
        foreground,
        null_audio,
    }) = &args.command
    {
        // The same flags a local player takes: `gmuse --device ua-101 engine` now means
        // something (#241).
        return run_engine(*foreground, *null_audio, args.player_config());
    }

    if let Some(gmuse::cli::Command::Shelf(op)) = &args.command {
        let loaded = if args.no_config {
            // **`--no-config` still keeps the log where the log lives.**
            // `Loaded::default()` leaves `data_dir` empty, and an empty path
            // reads as nothing (`read_dir("")` fails) while writing perfectly
            // well relative to the working directory — so a library operation
            // silently started a fresh log in whatever folder it was run from
            // and wrote it there.
            gmuse::config::Loaded {
                data_dir: gmuse::config::default_data_dir(),
                ..gmuse::config::Loaded::default()
            }
        } else {
            gmuse::config::load(
                &args
                    .config
                    .clone()
                    .unwrap_or_else(gmuse::config::default_path),
            )
        };
        let roots = if args.library.is_empty() {
            loaded.library.clone()
        } else {
            args.library_roots()
        };
        return run_library(op, &loaded, &roots, stale_cache(&args));
    }

    // Before the device is opened, so a refused second instance never makes a
    // sound and never touches state. Held for the whole of `main`: the binding
    // must outlive the player, and the kernel drops it whichever way we exit.
    //
    // **An attached view does not take it** — #219. The lock exists because two
    // *players* share one audio device; a view opens no device, and refusing a
    // second one would refuse the thing attaching is for. The engine's own
    // `engine.lock` is what keeps there being one player, and #214 made losing
    // that race a clean refusal rather than a crash.
    let _lock = if args.no_lock || args.attached() {
        None
    } else {
        match gmuse::lock::acquire(&gmuse::lock::path_in(&gmuse::config::default_state_dir())) {
            Ok(held) => Some(held),
            Err(err @ gmuse::lock::LockError::Held { .. }) => {
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

    // **Attached or local, decided once and here** — #219, §4.2.
    //
    // The order matters: `attach` starts an engine if none answers, and the
    // engine opens the device. Opening one here first and then attaching would
    // mean two processes holding it, and on CoreAudio the second one simply gets
    // silence rather than an error.
    let player = if args.attached() {
        // `die`, not `?`: `main` prints its error in `Debug`, and an engine that will not
        // answer is exactly the failure whose message is the repair.
        let client = std::sync::Arc::new(
            gmuse::engine::attach::attach("gmuse-tui", env!("CARGO_PKG_VERSION"))
                .unwrap_or_else(|e| die(e)),
        );
        let hz = client.hello().sample_rate;
        let remote = gmuse::ui::remote_transport::Remote::attach(client)?;
        println!("gmuse — attached to the engine, device at {hz} Hz");
        Playback::Attached(remote)
    } else {
        let player = PlayerHandle::new(args.player_config())?;
        println!("gmuse — device at {} Hz", player.device_rate());
        Playback::Local(player)
    };

    // Read the config first: it defines the catalogs, which decide what to
    // scan.
    let loaded = if args.no_config {
        gmuse::config::Loaded::default()
    } else {
        gmuse::config::load(
            &args
                .config
                .clone()
                .unwrap_or_else(gmuse::config::default_path),
        )
    };

    let cache_path = (!args.no_cache).then(|| {
        args.cache
            .clone()
            .unwrap_or_else(gmuse::cache::default_path)
    });

    // Load the cached library, then reconcile it with the roots we were given.
    let (store, cached_roots, cached_view) = match cache_path.as_deref().map(gmuse::cache::load) {
        Some(Ok(gmuse::cache::Outcome::Loaded { store, roots, view })) => (store, roots, view),
        Some(Ok(gmuse::cache::Outcome::Discarded)) => {
            // Costs a rescan, so say so. Silently starting from nothing is how
            // a library appears to vanish.
            eprintln!("cache: discarded (wrong version or corrupt); rescanning");
            (Holdings::new(), Vec::new(), None)
        }
        Some(Err(e)) => {
            eprintln!("cache: {e}");
            (Holdings::new(), Vec::new(), None)
        }
        _ => (Holdings::new(), Vec::new(), None),
    };
    // **The view in force**, which is one source line rather than a set of
    // names (#119). The config's `view =` wins where it speaks; otherwise the
    // last session's is restored, so a chosen view sticks.
    let view = loaded.view.clone().or(cached_view);

    // **Where the music is, which is `library` and nothing else** (#94). A
    // catalog used to carry roots too, so what a catalog *held* and where the
    // files *were* were the same field; scanning had to union every catalog's
    // roots to avoid a rescan on every switch. Now there is one answer.
    //
    // The cache's roots are a **fallback**, not an addition: they exist so
    // that `gmuse -L ~/Music` once, then bare afterwards, keeps working. Adding
    // them to what the config and `--library` already say is what let a
    // one-off `-L` linger for ever, and a stale root that happened to be the
    // parent of two real ones made every file scan twice (#21).
    let mut configured = loaded.library.clone();
    for root in args.library_roots() {
        if !configured.contains(&root) {
            configured.push(root);
        }
    }
    let roots = scan_roots(configured, cached_roots);

    let mut app = App {
        player,
        store,
        roots,
        cache_path,
        view,
        unreadable: Vec::new(),
    };

    // **Reconciled at start, on the librarian thread.** The 2026-09-10 entry
    // settled that a launch scans: the tree is an output the library owns, so
    // a scan cannot find news, only drift — a file moved, removed or replaced
    // by something outside the system.
    //
    // The scan itself is issued once the desk exists, and nothing waits on it.
    // The rows are already there: holdings come from the cached store and from
    // the log, so this closes drift rather than supplying the library.
    if args.rescan {
        // Forget mtimes so every file is re-read rather than settled by a stat.
        app.store = Holdings::new();
    }
    if app.roots.is_empty() {
        println!(
            "library: empty — scan one with `gmuse --library <dir>`, or set\n\
             `library = [\"~/Music\"]` in {}",
            gmuse::config::default_path().display()
        );
    } else {
        println!(
            "library: {} tracks, {}",
            app.store.len(),
            // **Said, because it stopped being true when attached** — #219. The
            // engine reconciles its own roots; this view would be a second
            // appender on one log (§4.4), so it does not. Printing the old line
            // anyway would have been the message describing work nobody did.
            if app.player.attached() {
                "the engine reconciles, not this view"
            } else {
                "reconciling on start"
            }
        );
    }

    if args.migrate_log {
        let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
        match gmuse::model::migrate::run(
            &loaded.data_dir,
            &app.roots,
            &machine,
            gmuse::history::now(),
        ) {
            Ok(outcome) => {
                println!("{outcome}");
                for (path, what) in &outcome.report.unresolved {
                    println!("  unresolved {what}: {path}");
                }
                println!("{}", outcome.census);
                println!("{} distinct artists", outcome.artists);
                for path in &outcome.orphaned {
                    println!("  orphaned acquisition date: {path}");
                }
            }
            Err(e) => {
                eprintln!("migrate: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    run_tui(
        &mut app,
        args.smart_artist_sort,
        loaded,
        args.log_path(),
        !args.no_resume,
        args.migrate_data,
    )
}

/// Which cache a library operation should discard, once it has moved music.
///
/// `None` when the run was told not to use one, so `--no-cache` means what it
/// says at both ends.
fn stale_cache(args: &gmuse::cli::Cli) -> Option<std::path::PathBuf> {
    (!args.no_cache).then(|| {
        args.cache
            .clone()
            .unwrap_or_else(gmuse::cache::default_path)
    })
}

/// Throw away a cache this command has just made wrong.
///
/// **The writer says so, because the reader is forbidden to guess.** A launch
/// deliberately does not scan — a change to the library is something you make,
/// not something a launch notices on your behalf — so the cache is believed
/// until something replaces it. That is right for files that appear behind
/// gmuse's back and wrong for files gmuse itself moved: an import renames
/// every file it places, and the player went on opening the old names and
/// showing nothing at all for the copies the cache had never seen.
/// The library root a retirement moves beneath.
fn root_of(roots: &[std::path::PathBuf]) -> std::path::PathBuf {
    roots.first().cloned().unwrap_or_default()
}

fn drop_cache(path: Option<&std::path::Path>) {
    if let Some(path) = path
        && let Err(e) = gmuse::cache::discard(path)
    {
        eprintln!("cache: {e}");
    }
}

/// One library operation, and then exit.
/// **Land what an operation computed: in the log, or in the engine's** — #218, §4.4.
///
/// Every writing `shelf` subcommand does its work against an in-memory log and then
/// has to put it somewhere. Only one process per log may append (#218), so "somewhere"
/// has two answers and this is the one place that picks:
///
/// - **The append lock is free**, so this process is the appender: `commit`, as before.
/// - **An engine holds it**, so it is: the facts go over the socket as `library.apply`
///   and the engine stamps and appends them with its own clock.
/// - **Neither** — the lock is held by something that is not answering — and the
///   refusal says so rather than guessing.
///
/// **Facts rather than records**, and no stamp crosses. A stamp says which device said
/// something and in what order; a client minting its own would put a second device id
/// in one machine's log, which is the contradiction the lock exists to prevent.
/// `Instance::pending` is exactly what `commit` would have flushed, by the same two
/// conditions, so the two answers carry the same thing.
///
/// The same shape `ui::record` takes for a view (#236) — desk first, socket second —
/// which is not a coincidence: it is the one question every writer in the program has
/// to ask.
fn land(
    log: &mut gmuse::model::instance::Instance,
    data_dir: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    match gmuse::librarian::Appending::claim(data_dir) {
        Ok(appending) => Ok(gmuse::librarian::commit(log, data_dir, &appending)?),
        Err(held) => {
            let facts = log.pending();
            if facts.is_empty() {
                // Nothing to land, so nothing to argue about. A dry-run-shaped pass
                // that changed nothing must not fail because a player is running.
                return Ok(0);
            }
            let socket = gmuse::engine::socket::default_path();
            if !gmuse::engine::attach::is_live(&socket) {
                eprintln!("{held}");
                return Err("the log is held by something that is not answering on \
                            the socket; stop it and run this again"
                    .into());
            }
            let counted = facts.len();
            // **Say who is about to answer, before waiting on them** — #263.
            //
            // Handing the facts over is the ordinary path and it is invisible: the operation
            // has already printed its summary, and what follows is a socket call whose only
            // sign of life is the process still existing. An import sat here for thirty
            // minutes against a wedged engine, at 0% CPU, saying nothing — diagnosed from
            // `ps`, because nothing else distinguished it from a slow import.
            eprintln!("handing {counted} fact(s) to the running engine…");
            let client =
                gmuse_client::Client::connect(&socket, "gmuse-shelf", env!("CARGO_PKG_VERSION"))
                    .map_err(|e| {
                        format!(
                            "the running engine would not shake hands: {e}\n\
                     The operation's own work is done and its files are moved; what is left \
                     is landing {counted} fact(s). Stop the engine with `gmuse ctl \
                     engine.shutdown` and run this again — with nothing holding the log, this \
                     process appends them itself."
                        )
                    })?;
            // **An array of facts, like `library.edit`** — one shape for one thing.
            //
            // It could not be, until #240: an audio id serialised as a `u128` and a
            // `serde_json::Value` cannot hold one, so this sent NDJSON in a string to
            // get around the protocol's `Value`-shaped ingress. `AudioId` says
            // `"v1:8d2f…"` now, so the dodge is gone and so is the second shape.
            // **Bounded, because the engine may be alive and not answering** — #263.
            //
            // `Client::call` waits as long as it takes, which is right for `gmuse ctl` where
            // a person can interrupt it and wrong here: this runs unattended, after an
            // operation that has already moved files, and a wait with no end is
            // indistinguishable from work.
            //
            // Generous rather than tight. Applying is not a long-running op (§8.4 names only
            // scan and compact), but the engine stamps and appends every fact, and an import
            // of a few hundred files hands over a few thousand — so the number has to be
            // about the worst honest case rather than the common one.
            // **Overridable, because the honest default is too long to test against.** The
            // same `GMUSE_*` shape the socket and the data dir already use; a person with a
            // genuinely slow engine has the same dial.
            let applying = std::env::var("GMUSE_APPLY_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .map(std::time::Duration::from_secs)
                .unwrap_or(std::time::Duration::from_secs(120));
            client
                .call_within(
                    gmuse_proto::request::LibraryOp::Apply.method(),
                    Some(serde_json::json!({ "facts": facts })),
                    applying,
                )
                .map_err(|e| {
                    format!(
                        "the running engine did not take the facts within {}s: {e}\n\
                         The operation's own work is done and its files are moved; what is \
                         left is landing {counted} fact(s). Stop the engine with `gmuse ctl \
                         engine.shutdown` and run this again — with nothing holding the log, \
                         this process appends them itself.",
                        applying.as_secs()
                    )
                })?;
            eprintln!("{counted} fact(s) handed to the running engine");
            Ok(counted)
        }
    }
}

/// **Say whether the library already holds this album, before the summary** — #261.
///
/// The gap the September 2026 ingestion found (`ingestion-flow.md` §4): nothing asks. The
/// per-file "joins a recording already held" line is emitted *after* you have decided to
/// import, one line per track, and it says nothing about the album. Bringing in a 13-track
/// FLAC of *Classics* meant noticing by hand that the library already had it as MP3 — and that
/// decides both which MusicBrainz release to match against and what to do with the old copy.
///
/// **Two signals, and the second is a hint on purpose.**
///
/// - **Recording ids**, from `report.joins`. Exact: these files carry ids the library already
///   binds. It is the FLAC-beside-MP3 case stated at album scale.
/// - **Artist, album and track count.** What ids miss, and they miss exactly the material most
///   likely to be a duplicate: an untagged re-rip binds no id and joins nothing. Reported as a
///   possibility rather than a fact, in `shelf copies`' spirit — an album of the same name by
///   the same artist with the same number of tracks is worth a look and is not proof.
///
/// Reported, never decided (`CLAUDE.md`). Holding one record in two formats is a supported
/// shape and often wanted; which to keep is not decidable from here.
fn already_held(
    report: &gmuse::operations::populate::Report,
    data_dir: &std::path::Path,
    device: gmuse::model::id::Id<gmuse::model::entity::Device>,
) {
    use gmuse::model::rows::Listing;
    use std::collections::{BTreeMap, BTreeSet};

    // **The files the import saw, not the paths it was given.** An argument is usually a
    // folder — `shelf import ~/staged/Classics` — and grouping *those* by their parent puts
    // one directory under `/staged` and asks the tag reader to read a directory.
    let mut arriving: BTreeMap<std::path::PathBuf, Vec<&std::path::PathBuf>> = BTreeMap::new();
    let considered = report
        .identified
        .keys()
        .chain(report.joins.iter().map(|(p, _, _)| p))
        .chain(report.unidentified.iter().map(|(p, _, _)| p))
        .chain(report.contradicts.iter().map(|(p, _)| p));
    for path in considered {
        if let Some(dir) = path.parent() {
            arriving.entry(dir.to_path_buf()).or_default().push(path);
        }
    }
    let joined: BTreeSet<&std::path::PathBuf> = report.joins.iter().map(|(p, _, _)| p).collect();

    // **Only read the library if there is a question to ask.** A fold is ~0.5s and an import
    // of material the library has never seen has nothing to compare against.
    let names: Vec<(std::path::PathBuf, String, String, usize)> = arriving
        .iter()
        .filter_map(|(dir, paths)| {
            let scan = paths
                .iter()
                .find_map(|p| gmuse::model::ingest::read(p).ok())?;
            let album = scan.album?;
            let artist = scan.album_artist.unwrap_or(scan.artist);
            Some((dir.clone(), artist, album, paths.len()))
        })
        .collect();
    if names.is_empty() {
        return;
    }

    let projection = gmuse::librarian::reading(data_dir, device, gmuse::history::now_ms());
    let library = gmuse::model::seam::Librarian::from_catalogue(projection.catalogue());
    // One walk of the rows, not one lookup per album.
    let mut held: BTreeMap<(String, String), usize> = BTreeMap::new();
    for id in library.rows() {
        if let Some(row) = library.listed(id) {
            let listed = row.listed();
            *held
                .entry((listed.file_under.to_string(), listed.album.to_string()))
                .or_default() += 1;
        }
    }

    for (dir, artist, album, count) in names {
        let mine = arriving.get(&dir).map(Vec::as_slice).unwrap_or_default();
        let joins = mine.iter().filter(|p| joined.contains(**p)).count();
        let same = held
            .get(&(artist.clone(), album.clone()))
            .copied()
            .unwrap_or(0);

        match verdict(joins, count, same) {
            Some(Held::Certain { joins }) => eprintln!(
                "already held: {joins} of {count} track(s) in {album:?} join recordings this \
                 library already has.\n    One recording in two formats is a supported shape — \
                 `shelf copies` lists them, and `shelf remove` retires one."
            ),
            Some(Held::Possibly { same }) => eprintln!(
                "possibly already held: {same} track(s) are filed under {artist:?} / \
                 {album:?}, and this is {count}.\n    No MusicBrainz id connects them, so this \
                 is a name agreeing with a name — `shelf copies` and `shelf duplicates` are \
                 what settle it."
            ),
            None => {}
        }
    }
}

/// What the numbers say, without the disk or the library.
#[derive(Debug, PartialEq, Eq)]
enum Held {
    /// Ids agree: these recordings are bound to encodings the library holds.
    Certain { joins: usize },
    /// A name agrees with a name and nothing else does.
    Possibly { same: usize },
}

/// **A pure function of the things that decide it**, so the rule is testable without a library
/// to read or an album to import — the shape `which_key_due` takes, for the same reason.
///
/// **An id beats a name.** An album whose recordings join also has a matching name, and saying
/// both would read as two findings about one album.
fn verdict(joins: usize, arriving: usize, same: usize) -> Option<Held> {
    let _ = arriving;
    if joins > 0 {
        return Some(Held::Certain { joins });
    }
    (same > 0).then_some(Held::Possibly { same })
}

#[cfg(test)]
mod already_held_says_which_kind_of_held {
    use super::{Held, verdict};

    /// **An id beats a name, and nothing is said about an album nothing matches** — #261.
    ///
    /// The two signals answer different questions and the weaker one is a hint on purpose:
    /// recording ids are exact, and artist-plus-album is a name agreeing with a name.
    #[test]
    fn an_id_beats_a_name_and_silence_beats_both() {
        assert_eq!(verdict(13, 13, 0), Some(Held::Certain { joins: 13 }));
        // Even when the names agree too — one finding, not two.
        assert_eq!(verdict(13, 13, 13), Some(Held::Certain { joins: 13 }));
        // Part of an album joining is still certain about that part.
        assert_eq!(verdict(2, 13, 0), Some(Held::Certain { joins: 2 }));

        // No id, but a name agrees: a question rather than a statement.
        assert_eq!(verdict(0, 9, 13), Some(Held::Possibly { same: 13 }));

        // **Nothing to say.** An import of material the library has never seen must be quiet,
        // or the line means nothing when it does appear.
        assert_eq!(verdict(0, 12, 0), None);
        assert_eq!(verdict(0, 0, 0), None);
    }
}

fn run_library(
    op: &gmuse::cli::Shelf,
    loaded: &gmuse::config::Loaded,
    roots: &[std::path::PathBuf],
    cache_path: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    match op {
        gmuse::cli::Shelf::Restore { paths, write } => {
            let mut files: Vec<std::path::PathBuf> = Vec::new();
            for path in paths {
                if path.is_dir() {
                    for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
                        if entry.file_type().is_file()
                            && gmuse::operations::scan::is_audio(entry.path())
                        {
                            files.push(entry.path().to_path_buf());
                        }
                    }
                } else if gmuse::operations::scan::is_audio(path) {
                    files.push(path.clone());
                }
            }
            files.sort();
            if files.is_empty() {
                eprintln!("no audio under what was named");
                return Ok(());
            }

            let (mut log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            let cat = log.catalogue();
            // **Resolved by hashing, like a retirement**, for the same reason:
            // identity is the audio and a path belongs to one device's copy.
            // It is what lets the bytes be moved back first and named here by
            // wherever they now sit.
            let mut coming = Vec::new();
            for file in &files {
                match gmuse::model::ingest::read(file) {
                    Ok(scanned) => {
                        let id = gmuse::model::id::Id::<gmuse::model::entity::Encoding>::of_audio(
                            &scanned.audio,
                        );
                        if !cat.retired.contains(&id) {
                            eprintln!("not retired: {}", file.display());
                        } else {
                            coming.push(id);
                        }
                    }
                    Err(why) => eprintln!("unreadable: {} — {why}", file.display()),
                }
            }

            let fresh = if *write {
                let now = gmuse::history::now_ms();
                for id in &coming {
                    log.restore_encoding(now, *id);
                }
                land(&mut log, &loaded.data_dir)?
            } else {
                0
            };
            eprintln!(
                "{} file(s), {} to restore, {fresh} record(s) written",
                files.len(),
                coming.len()
            );
            if !*write && !coming.is_empty() {
                eprintln!("dry run — nothing written. Add --write.");
            }
        }
        gmuse::cli::Shelf::Remove { paths, write } => {
            // Every audio file under what was named, `.departed` included:
            // an album retired by hand before this existed is exactly the case
            // that wants a tombstone and no move.
            let mut files: Vec<std::path::PathBuf> = Vec::new();
            for path in paths {
                if path.is_dir() {
                    for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
                        if entry.file_type().is_file()
                            && gmuse::operations::scan::is_audio(entry.path())
                        {
                            files.push(entry.path().to_path_buf());
                        }
                    }
                } else if gmuse::operations::scan::is_audio(path) {
                    files.push(path.clone());
                }
            }
            files.sort();
            if files.is_empty() {
                eprintln!("no audio under what was named");
                return Ok(());
            }

            let (mut log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            let cat = log.catalogue();
            // **Resolved by hashing, not by path.** A retirement is about
            // bytes, and the path is a property of one device's copy — which
            // is the whole reason nothing here is keyed by one. It also means
            // a file already sitting in `.departed` still resolves.
            // **Where every device says it holds each copy.** Identity is
            // the audio, so two paths can be one encoding — and retiring the
            // one you meant would retire the one you did not.
            let departed = root_of(roots).join(gmuse::operations::scan::DEPARTED);
            // **Elsewhere means somewhere that is not going.** The guard below
            // asks whether these bytes survive this retirement, and the files
            // being retired are not an answer to that — every one of them is
            // held at its own path, so without this every file reports itself
            // as the reason it cannot go and `remove` retires nothing at all.
            //
            // It could only ever tombstone a file somebody had already moved
            // into `.departed` by hand, which is the one case the path filter
            // below happened to exclude.
            let going: std::collections::BTreeSet<String> =
                files.iter().map(|f| f.display().to_string()).collect();
            let elsewhere = surviving_holders(&cat, &departed, &going);

            let mut retiring = Vec::new();
            for file in &files {
                match gmuse::model::ingest::read(file) {
                    Ok(scanned) => {
                        let id = gmuse::model::id::Id::<gmuse::model::entity::Encoding>::of_audio(
                            &scanned.audio,
                        );
                        if !cat.encodings.contains_key(&id) {
                            eprintln!("not in the library: {}", file.display());
                        } else if let Some(held) = elsewhere.get(&id) {
                            // **The bytes have not left.** `Early James` was
                            // retired by moving one of two folders holding
                            // identical rips: ten of its twelve files are the
                            // same encoding as a file still in the library, so
                            // retiring "the departed copy" retired the live one
                            // too. Measured before this guard existed — the
                            // library went from 6,648 settled to 6,638.
                            eprintln!(
                                "still held, so not retired: {}\n    the same bytes are at {}",
                                file.display(),
                                held.join("\n    and at ")
                            );
                        } else {
                            retiring.push((file.clone(), id));
                        }
                    }
                    Err(why) => eprintln!("unreadable: {} — {why}", file.display()),
                }
            }

            let root = root_of(roots);
            let done = gmuse::operations::storage::retire(
                &root,
                &retiring.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>(),
                *write,
            );
            for (path, why) in &done.failed {
                eprintln!("refused: {} — {why}", path.display());
            }

            let fresh: usize = if *write {
                let now = gmuse::history::now_ms();
                let before = log.records().len();
                for (_, id) in &retiring {
                    log.retire_encoding(now, *id);
                }
                let _ = before;
                land(&mut log, &loaded.data_dir)?
            } else {
                0
            };
            eprintln!(
                "{} file(s), {} to retire, {} would move to {}, {} refused",
                files.len(),
                retiring.len(),
                done.moved,
                gmuse::operations::scan::DEPARTED,
                done.failed.len(),
            );
            if *write {
                eprintln!("{fresh} tombstone(s) written, {} moved", done.moved);
                drop_cache(cache_path.as_deref());
            } else {
                eprintln!("dry run — nothing written. Add --write.");
            }
        }
        gmuse::cli::Shelf::Scan { write } => {
            // **It walks the disk here and lands the facts through `land`** — #218.
            // An earlier cut handed the whole operation to the engine via
            // `library.scan`; walking the disk locally and handing over what it found
            // is better, because the person asking gets the report. The engine still
            // reconciles on its own schedule, and `worth_asserting` means two scans
            // that agree write nothing twice.
            let (mut log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            let before = log.records().len();
            let mut reads = gmuse::cache::load_measurements(&gmuse::cache::measurements_path());
            let report = gmuse::operations::populate::reconcile(
                &mut log,
                roots,
                gmuse::history::now_ms(),
                &mut reads,
            );

            // **Named, not counted.** A file the library has never been told
            // about is the one thing a scan can find that somebody has to act
            // on, and `import` is the only act that takes it in (#160).
            for path in report.unknown.iter().take(20) {
                println!("unknown\t{}", path.display());
            }
            if report.unknown.len() > 20 {
                println!("… and {} more", report.unknown.len() - 20);
            }
            // Two paths holding one set of bytes: the second buys no
            // capability, and this is the only place it comes to light.
            for (extra, kept) in &report.duplicates {
                println!("duplicate\t{}\t{}", extra.display(), kept.display());
            }
            for (path, why) in report.unreadable.iter().take(10) {
                eprintln!("unreadable: {} — {why}", path.display());
            }

            let (hit, read) = reads.tally();
            eprintln!(
                "{} file(s), {} unknown, {} duplicate, {} unreadable ({hit} unchanged, {read} read)",
                report.files,
                report.unknown.len(),
                report.duplicates.len(),
                report.unreadable.len(),
            );
            // **The one refusal that must not be read as "nothing changed"** —
            // #233. An empty result over a library that had files means the roots
            // are wrong, and the scan declines rather than erasing them.
            if let Some(had) = report.would_empty {
                eprintln!(
                    "refused: this found none of the {had} file(s) the library holds, \
                     so its holdings were left alone — are the roots right? {:?}",
                    loaded.library
                );
            }

            if *write {
                let n = land(&mut log, &loaded.data_dir)?;
                eprintln!(
                    "{n} record(s) written ({} new)",
                    log.records().len() - before
                );
                reads.prune_missing();
                gmuse::cache::save_measurements(&gmuse::cache::measurements_path(), &reads);
                // The locators moved, so anything holding old ones is stale.
                drop_cache(cache_path.as_deref());
            } else {
                eprintln!("dry run — nothing written. Add --write.");
            }
        }
        gmuse::cli::Shelf::Compact { write } => {
            let (mut log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            let before = log.records().clone();
            // Through the instance rather than the free function, so there is
            // one way to compact and the in-memory set matches what is about
            // to be on disk.
            log.compact();
            let after = log.records().clone();

            let dropped = before.len() - after.len();
            // Bytes as they sit on disk, so the number reported is the number
            // a person can check with `ls`.
            let weigh = |rs: &std::collections::BTreeSet<gmuse::facts::Record>| -> usize {
                rs.iter()
                    .filter_map(|r| serde_json::to_string(r).ok())
                    .map(|l| l.len() + 1)
                    .sum()
            };
            let (was, now) = (weigh(&before), weigh(&after));
            eprintln!(
                "{dropped} superseded holdings assertion(s), {:.1} MB of {:.1} MB",
                (was - now) as f64 / 1e6,
                was as f64 / 1e6,
            );

            if !*write {
                eprintln!("dry run — nothing written. Add --write.");
            } else if dropped == 0 {
                eprintln!("nothing to drop.");
            } else {
                // **Claimed, or handed over** — #249, §4.4, I15.
                //
                // This arm used to hold its own copy of the per-device loop and call
                // `durable::rewrite` directly, which claimed nothing. So compacting —
                // the one operation that *removes* — could rewrite a log from outside
                // the lock while an engine held it, deleting whatever the engine had
                // appended since this process folded. `durable::rewrite` is
                // `pub(crate)` now, so the shape is unavailable rather than merely
                // discouraged.
                //
                // The shape is `land`'s, because the question is the same one every
                // writer in this program has to ask: hold the log, or ask whoever
                // does. `library.compact` is the engine's answer and it already
                // existed — it was simply unreachable from here.
                match gmuse::librarian::Appending::claim(&loaded.data_dir) {
                    Ok(appending) => {
                        let total =
                            gmuse::librarian::rewrite_all(&log, &loaded.data_dir, &appending)?;
                        eprintln!("{total} record(s) kept, logs rewritten");
                        // The player's cache names paths and nothing here moved a
                        // file, so it stays valid. Said out loud because every other
                        // command in this block drops it.
                    }
                    Err(held) => {
                        let socket = gmuse::engine::socket::default_path();
                        if !gmuse::engine::attach::is_live(&socket) {
                            eprintln!("{held}");
                            return Err("the log is held by something that is not \
                                        answering on the socket; stop it and run this \
                                        again"
                                .into());
                        }
                        let client = gmuse_client::Client::connect(
                            &socket,
                            "gmuse-shelf",
                            env!("CARGO_PKG_VERSION"),
                        )?;
                        client.call(gmuse_proto::method::LIBRARY_COMPACT, None)?;
                        eprintln!(
                            "the engine holds the log; it will compact its own copy — \
                             {dropped} assertion(s) to drop"
                        );
                    }
                }
            }
        }
        gmuse::cli::Shelf::Tidy { write } => {
            // **A walk, not a query** — these are not in the library and never were.
            // Nothing recorded them, so nothing can be asked about them; the disk is
            // the only place the answer lives.
            //
            // The same roots everything else uses, and `worth_entering` keeps it out
            // of `.gmuse` and `.departed` — a tombstone's folder is not a place to
            // tidy, and the data directory's own lock file is not junk.
            let mut found: Vec<std::path::PathBuf> = Vec::new();
            for root in roots {
                for entry in walkdir::WalkDir::new(root)
                    .into_iter()
                    .filter_entry(gmuse::operations::scan::worth_entering)
                    .flatten()
                {
                    let path = entry.path();
                    if entry.file_type().is_file() && gmuse::operations::scan::is_junk(path) {
                        found.push(path.to_path_buf());
                    }
                }
            }
            found.sort();

            // Named, not counted: a deletion a person cannot check is one they have to
            // take on trust, and there are few enough of these to print.
            for path in &found {
                println!("{}", path.display());
            }

            let mut gone = 0;
            let mut failed = Vec::new();
            if *write {
                for path in &found {
                    match std::fs::remove_file(path) {
                        Ok(()) => gone += 1,
                        Err(e) => failed.push((path.clone(), e.to_string())),
                    }
                }
            }
            for (path, why) in &failed {
                eprintln!("could not remove {}: {why}", path.display());
            }
            eprintln!(
                "{} file(s) the system left{}",
                found.len(),
                if *write {
                    format!(", {gone} removed")
                } else {
                    String::new()
                }
            );
            if !*write && !found.is_empty() {
                eprintln!("dry run — nothing removed. Add --write.");
            }
        }
        gmuse::cli::Shelf::Names => {
            // **A read, so no scan** — #220. Walking the disk here answered
            // with what the tags say over what the library recorded, which is
            // the one thing the library exists to settle.
            //
            // The log itself is opened, because this arm reads **facts** and
            // not only the fold: a read may derive, it may not discover.
            let (log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            // The snapshot for the fold; the log above is open because this
            // arm walks **facts**, which the projection does not carry.
            let projection =
                gmuse::librarian::reading(&loaded.data_dir, device, gmuse::history::now_ms());
            let cat = projection.catalogue();

            // Every name each id has ever been given, in log order, so the
            // last one — the one in force — reads last.
            //
            // **Through the merge**, because the raw log still names the loser
            // and the loser is no longer a row. Without this, every artist
            // merge adds a permanent entry here: an id nothing holds, listed
            // as being in doubt about a name it no longer owns.
            let mut said: std::collections::BTreeMap<_, Vec<&str>> = Default::default();
            for record in log.records() {
                if let gmuse::facts::Fact::ArtistNamed { artist, name } = &record.fact {
                    let seen: &mut Vec<&str> =
                        said.entry(cat.artist_merges.resolve(*artist)).or_default();
                    if seen.last() != Some(&name.as_str()) {
                        seen.push(name);
                    }
                }
            }
            let credited: std::collections::BTreeSet<_> =
                cat.credits.keys().map(|(_, artist, ..)| *artist).collect();

            let mut nameless = 0usize;
            for artist in credited
                .iter()
                .filter(|a| !cat.artist_names.contains_key(a))
            {
                println!("{artist}  <<never named>>");
                nameless += 1;
            }
            // **A line is offered only for the exact signature of the fault** — an
            // earlier name, followed by a joiner, followed by somebody else. That is
            // what #145 describes: the id is the *primary* artist's and the string
            // names the whole credit, so `Bob Dylan` becomes `Bob Dylan & The Band`.
            //
            // The looser rule — offer whichever earlier name carries no joiner — was
            // wrong on half of this library's cases. `MGMT` against `The Management`,
            // `Antigone` against `ANTIGONE`, `Bush` against `Bush X`, `Sidney Carter`
            // against `Mrs. Sidney Carter`: four artists with two real names, where
            // which one the library should use is a decision and not a defect. It
            // caught eight and four of them were that.
            //
            // Narrowed to the prefix, it offers exactly the four that are the bug.
            let renamed_by_a_joiner = |was: &str, now: &str| {
                now.strip_prefix(was).is_some_and(|rest| {
                    [
                        " & ", ", ", ",", " and ", " feat. ", " ft. ", " vs. ", " with ",
                    ]
                    .iter()
                    .any(|j| rest.starts_with(j))
                })
            };
            let mut renamed = 0usize;
            let mut offered = 0usize;
            let mut declared = 0usize;
            for (artist, names) in said.iter().filter(|(_, n)| n.len() > 1) {
                // **A name somebody declared an alias is not a name in
                // dispute** — #172. This report could only ever say "artists
                // with two real names, and which to use is yours to say",
                // because the log recorded no reason for the second one.
                // Where every earlier name has been aliased, the reason is on
                // the record and there is nothing left to decide.
                let aliased = cat.aliases.iter().any(|(subject, value, _)| {
                    *subject == gmuse::model::reference::Cataloguable::Artist(*artist)
                        && names.iter().rev().skip(1).any(|n| n == value)
                });
                if aliased {
                    declared += 1;
                    continue;
                }
                renamed += 1;
                let current = names.last().copied().unwrap_or_default();
                println!("{artist}");
                // The distinct names, because an alternating history repeats
                // the same two a dozen times and says nothing more than twice.
                let mut shown: Vec<&str> = Vec::new();
                for name in names {
                    if !shown.contains(name) {
                        shown.push(name);
                    }
                }
                for name in &shown {
                    let mark = if *name == current { "->" } else { "  " };
                    println!("  {mark} {name}");
                }
                if let Some(plain) = shown.iter().find(|n| renamed_by_a_joiner(n, current)) {
                    offered += 1;
                    println!(
                        "  {}",
                        serde_json::to_string(&serde_json::json!({
                            "fact": "named",
                            "artist": artist.to_string(),
                            "name": plain,
                        }))?
                    );
                }
            }
            eprintln!(
                "\n{nameless} credited artist(s) have no name, {renamed} have been named more than \
                 once — `->` marks the one in force. {offered} are named for a credit they merely \
                 begin, and the line under each puts the artist's own name back. The rest are \
                 artists with two real names, and which to use is yours to say.\n\
                 {declared} more are settled: an earlier name is on the record as an alias, so it \
                 is a name this artist also has rather than one in dispute."
            );
        }
        gmuse::cli::Shelf::Duplicates => {
            // **A read, so the snapshot and never a scan** — #220. Walking the
            // disk here answered with what the tags say over what the library
            // recorded, which is the one thing the library exists to settle.
            let projection =
                gmuse::librarian::reading(&loaded.data_dir, device, gmuse::history::now_ms());
            let cat = projection.catalogue();
            let library = gmuse::model::seam::Librarian::from_catalogue(cat);

            // How many files sit on each album, which is what tells a dead
            // duplicate from a real split. A group with none is an identity
            // that moved; two with files each is one record in two halves.
            let mut held: std::collections::BTreeMap<_, usize> = Default::default();
            for slot in cat.placed.values() {
                if let Some(track) = cat.tracks.get(slot)
                    && let Some(medium) = cat.media.get(&track.medium)
                    && let Some(release) = cat.releases.get(&medium.release)
                {
                    *held.entry(release.group).or_default() += 1;
                }
            }

            let mut by_name: std::collections::BTreeMap<(String, String), Vec<_>> =
                Default::default();
            for (id, group) in &cat.groups {
                let who = library
                    .reference
                    .credited_as(gmuse::model::reference::CreditOn::ReleaseGroup(*id));
                // **Folded, because capitalisation is a typo and not a
                // distinction.** `normalise` folds case when it mints a group
                // id for exactly that reason, so a report that compares titles
                // exactly is asking a different question from the one that
                // created the duplicate. `Early James — Strange Time To Be
                // Alive` and `… to Be Alive` are one record held twice, and
                // this listed neither.
                by_name
                    .entry((
                        gmuse::model::ingest::normalise(&who),
                        gmuse::model::ingest::normalise(&group.title),
                    ))
                    .or_default()
                    .push(*id);
            }

            let mut found = 0usize;
            // What the name rule already said, so the overlap pass below adds
            // to the report rather than repeating it.
            let mut reported: Vec<gmuse::model::id::Id<gmuse::model::entity::ReleaseGroup>> =
                Vec::new();
            for ((_, _), mut ids) in by_name {
                if ids.len() < 2 {
                    continue;
                }
                found += 1;
                reported.extend(ids.iter().copied());
                // Printed from the winner rather than the folded key, so what
                // is shown is a title somebody actually wrote.
                let (who, title) = (
                    library
                        .reference
                        .credited_as(gmuse::model::reference::CreditOn::ReleaseGroup(ids[0])),
                    cat.groups[&ids[0]].title.clone(),
                );
                // Most files first, so the obvious winner reads first.
                ids.sort_by_key(|id| std::cmp::Reverse(held.get(id).copied().unwrap_or(0)));
                println!("{who} — {title}");
                for id in &ids {
                    let n = held.get(id).copied().unwrap_or(0);
                    println!("  {id}  {n:4} file(s)");
                }
                println!(
                    "  {}",
                    serde_json::to_string(&serde_json::json!({
                        "fact": "groups_merged",
                        "loser": ids[1].to_string(),
                        "winner": ids[0].to_string(),
                    }))?
                );
            }
            eprintln!(
                "\n{found} album(s) held more than once. The line under each is the merge that \
                 would join them — **read it first**: two records can share a name, and \
                 `AC/DC — High Voltage` is two."
            );

            // **And the ones a name cannot find** — #185.
            //
            // The rule above compares titles, so it misses a record imported
            // twice under names its tags spelled differently:
            // `Selected Ambient Works Vol.1 [85-92]` against
            // `Selected Ambient Works 85–92`, which sat side by side until a
            // person noticed. Comparing *recordings* corroborates against
            // something the title rule did not produce — lesson 1 — and needs
            // no fuzzy matching to do it.
            let mut of_group: std::collections::BTreeMap<
                gmuse::model::id::Id<gmuse::model::entity::ReleaseGroup>,
                std::collections::BTreeSet<gmuse::model::id::Id<gmuse::model::entity::Recording>>,
            > = Default::default();
            for placed in cat.tracks.values() {
                if let Some(medium) = cat.media.get(&placed.medium)
                    && let Some(release) = cat.releases.get(&medium.release)
                {
                    of_group
                        .entry(cat.group_merges.resolve(release.group))
                        .or_default()
                        .insert(placed.recording);
                }
            }
            let listed: std::collections::BTreeSet<_> = reported.iter().copied().collect();
            let keys: Vec<_> = of_group
                .iter()
                .filter(|(_, recs)| recs.len() >= 2)
                .map(|(id, _)| *id)
                .collect();
            let mut overlaps = Vec::new();
            for (i, a) in keys.iter().enumerate() {
                for b in &keys[i + 1..] {
                    let (left, right) = (&of_group[a], &of_group[b]);
                    let shared = left.intersection(right).count();
                    if shared == 0 {
                        continue;
                    }
                    let union = left.len() + right.len() - shared;
                    // **Two numbers, because they answer different
                    // questions.** Containment says every track of the
                    // smaller is on the larger, which a *single inside its
                    // album* satisfies perfectly — `Come to Daddy Remixed` is
                    // 100% contained in `Come to Daddy EP` and is a different
                    // record. Only the share of the *union* separates one
                    // record held twice from one record inside another.
                    let contains = shared as f64 / left.len().min(right.len()) as f64;
                    let both = shared as f64 / union as f64;
                    if contains < 0.6 || (listed.contains(a) && listed.contains(b)) {
                        continue;
                    }
                    // **Most files wins**, as the name rule above decides it:
                    // the row that has to exist for the merge to mean
                    // anything is the one holding the copies.
                    let (keep, drop) =
                        if held.get(a).copied().unwrap_or(0) >= held.get(b).copied().unwrap_or(0) {
                            (*a, *b)
                        } else {
                            (*b, *a)
                        };
                    overlaps.push((both, contains, keep, drop));
                }
            }
            overlaps.sort_by(|x, y| y.0.total_cmp(&x.0));
            if !overlaps.is_empty() {
                println!();
                for (both, contains, a, b) in &overlaps {
                    let name = |id| {
                        format!(
                            "{} — {}",
                            library
                                .reference
                                .credited_as(gmuse::model::reference::CreditOn::ReleaseGroup(id)),
                            cat.groups[&id].title
                        )
                    };
                    println!(
                        "{:.0}% shared, {:.0}% of the smaller",
                        both * 100.0,
                        contains * 100.0
                    );
                    for id in [a, b] {
                        println!(
                            "  {id}  {:4} file(s)  {}",
                            held.get(id).copied().unwrap_or(0),
                            name(*id)
                        );
                    }
                    println!(
                        "  {}",
                        serde_json::to_string(&serde_json::json!({
                            "fact": "groups_merged",
                            "loser": b.to_string(),
                            "winner": a.to_string(),
                        }))?
                    );
                }
                eprintln!(
                    "\n{} pair(s) share recordings under different names. **The first number \
                     decides.** A high share of the *union* is one record held twice; a high \
                     share of the smaller alone is one record inside another — a single on its \
                     album, an EP inside the edition that absorbed it — and merging those loses \
                     a record you own.",
                    overlaps.len()
                );
            }

            // **A group holding several releases** — #185, and the split the
            // name rule cannot see at all because the group is not duplicated.
            // `Analogue Bubblebath` is one group whose mp3s and flacs sit on
            // two pressings, so they never share a slot, `shown_copy` never
            // gets to choose between them, and the tree draws every track
            // twice in rows that read identically.
            let mut pressings: std::collections::BTreeMap<_, Vec<_>> = Default::default();
            for (id, release) in &cat.releases {
                pressings
                    .entry(cat.group_merges.resolve(release.group))
                    .or_insert_with(Vec::new)
                    .push(*id);
            }
            // **Only where two of them hold copies.** A pressing with no
            // files is the ordinary case and not a split: a tracklist names
            // the whole record and #155 says a track you do not own is a row
            // on purpose, so most groups with two releases have one real
            // pressing and one list. 60 rows of that buries the four that
            // matter.
            let files_on = |release: &gmuse::model::id::Id<gmuse::model::entity::Release>| {
                let on: std::collections::BTreeSet<_> = cat
                    .media
                    .iter()
                    .filter(|(_, m)| m.release == *release)
                    .map(|(id, _)| *id)
                    .collect();
                cat.placed
                    .values()
                    .filter(|t| cat.tracks.get(t).is_some_and(|p| on.contains(&p.medium)))
                    .count()
            };
            let split: Vec<_> = pressings
                .iter()
                .filter(|(_, r)| r.iter().filter(|id| files_on(id) > 0).count() > 1)
                .collect();
            if !split.is_empty() {
                println!();
                for (group, releases) in &split {
                    let Some(grouped) = cat.groups.get(group) else {
                        continue;
                    };
                    println!(
                        "{} — {}   {} pressings",
                        library
                            .reference
                            .credited_as(gmuse::model::reference::CreditOn::ReleaseGroup(**group)),
                        grouped.title,
                        releases.len()
                    );
                    for release in *releases {
                        println!("  {release}  {:4} file(s)", files_on(release));
                    }
                }
                eprintln!(
                    "\n{} group(s) hold copies on more than one pressing. A CD and a vinyl of \
                     one record are genuinely two releases — but where the copies are the *same* \
                     tracks in two formats they never share a slot, so `shown_copy` never gets \
                     to choose between them and the tree draws each track twice in rows that \
                     read identically.",
                    split.len()
                );
            }
        }
        gmuse::cli::Shelf::Copies => {
            // **A read, so the snapshot and never a scan** — #220. Walking the
            // disk here answered with what the tags say over what the library
            // recorded, which is the one thing the library exists to settle.
            let projection =
                gmuse::librarian::reading(&loaded.data_dir, device, gmuse::history::now_ms());
            let cat = projection.catalogue();
            let here = cat.holdings.get(&device);
            let at: std::collections::BTreeMap<_, &String> = here
                .map(|a| a.held.iter().map(|(id, path)| (*id, path)).collect())
                .unwrap_or_default();

            // **Grouped by recording, and only the ones this device can open
            // twice.** A recording held once here and once on another machine
            // is not a choice anybody can make from this keyboard.
            let mut by_recording: std::collections::BTreeMap<_, Vec<_>> = Default::default();
            for (encoding, path) in &at {
                if cat.retired.contains(encoding) {
                    continue;
                }
                if let Some(recording) = cat.bound.get(encoding) {
                    by_recording
                        .entry(*recording)
                        .or_insert_with(Vec::new)
                        .push((*encoding, *path));
                }
            }

            let mut found = 0usize;
            let mut extra = 0usize;
            for (recording, mut copies) in by_recording {
                if copies.len() < 2 {
                    continue;
                }
                found += 1;
                extra += copies.len() - 1;
                let title = cat
                    .recordings
                    .get(&recording)
                    .map(|r| r.title.as_str())
                    .unwrap_or("<untitled>");
                let who = cat
                    .recordings
                    .get(&recording)
                    .and_then(|r| cat.artist_names.get(&r.artist))
                    .map(String::as_str)
                    .unwrap_or("");
                println!("{who} — {title}");
                // **Best first, by what is measurable**, which is a ranking and
                // not a verdict: bitrate says nothing about which copy carries
                // the better tags, and the `Early James` pair is the standing
                // reminder that the worse-sounding side can be the one worth
                // keeping.
                copies.sort_by_key(|(e, _)| {
                    std::cmp::Reverse(cat.encodings.get(e).map_or(0, |x| x.bitrate))
                });
                for (encoding, path) in copies {
                    let it = cat.encodings.get(&encoding);
                    // **`lofty` reports kilobits already**, so this is the
                    // number as stored — dividing it once gave every mp3 in
                    // the library a bitrate of `0k`.
                    let kbps = it.map_or(0, |x| x.bitrate);
                    let fmt = it
                        .and_then(|x| x.format)
                        .map_or_else(|| "?".into(), |f| format!("{f:?}").to_lowercase());
                    let secs = it.map_or(0, |x| x.length_ms) as f64 / 1000.0;
                    println!("  {kbps:>4}k {fmt:<5} {secs:>7.1}s  {path}");
                }
            }
            eprintln!(
                "\n{found} recording(s) held more than once here, {extra} file(s) beyond the first."
            );
            if found > 0 {
                eprintln!(
                    "Sorted by bitrate, which is a ranking and not a recommendation — a second\n\
                     copy can carry the better tags. `gmuse shelf remove <path>` retires one."
                );
            }
        }
        gmuse::cli::Shelf::Slots { work } => {
            use gmuse::model::entity::{Medium, Recording};
            use gmuse::model::id::Id;

            // **A read, so the snapshot and never a scan** — #220. Walking the
            // disk here answered with what the tags say over what the library
            // recorded, which is the one thing the library exists to settle.
            let projection =
                gmuse::librarian::reading(&loaded.data_dir, device, gmuse::history::now_ms());
            let cat = projection.catalogue();
            let here = cat.holdings.get(&device);

            // Which recordings this device can actually open, so the report can
            // say which claimant is the one with a file behind it — that is the
            // difference between "two versions on one disc" and "the row that
            // is right is the row nobody can play".
            let mut playable: std::collections::BTreeMap<Id<Recording>, u32> = Default::default();
            for encoding in here.map(|a| a.held.keys()).into_iter().flatten() {
                if !cat.retired.contains(encoding)
                    && let Some(recording) = cat.bound.get(encoding)
                {
                    playable.insert(
                        *recording,
                        cat.encodings.get(encoding).map_or(0, |e| e.length_ms),
                    );
                }
            }

            // The finding itself is `Librarian::contested_slots`, so it is
            // testable and there is one definition of what "contested" means.
            // What is added here is device-local: which claimant has a file.
            let library = gmuse::model::seam::Librarian::from_catalogue(cat);
            // **The same rows `query` hands out, narrowed to the contest.**
            // A tool listening to these needs a locator and an id, which is
            // what a work list is — and building a second shape for it would
            // be a second thing to keep in step with `Queried`.
            if *work {
                let contested: std::collections::BTreeSet<Id<Recording>> = library
                    .contested_slots()
                    .into_values()
                    .flatten()
                    .filter(|r| playable.contains_key(r))
                    .collect();
                let library = gmuse::model::seam::Librarian::from_catalogue(projection.catalogue());
                for row in gmuse::query::rows(
                    projection.catalogue(),
                    &library,
                    device,
                    gmuse::history::now_ms(),
                    gmuse::query::Only::Everything,
                    // A contested seat is a question about the *recording* —
                    // which performance sits here — so one copy answers it.
                    gmuse::query::Copies::Shown,
                    None,
                ) {
                    if row.recording.is_some_and(|r| contested.contains(&r)) {
                        println!("{}", serde_json::to_string(&row)?);
                    }
                }
                return Ok(());
            }

            let mut found = 0usize;
            let mut media: std::collections::BTreeSet<Id<Medium>> = Default::default();
            for ((medium, position), claims) in library.contested_slots() {
                found += 1;
                media.insert(medium);
                let album = cat
                    .media
                    .get(&medium)
                    .and_then(|m| cat.releases.get(&m.release))
                    .map(|r| r.title.as_str())
                    .unwrap_or("<untitled>");
                println!("{album} — track {position}");
                for recording in claims {
                    let title = cat
                        .recordings
                        .get(&recording)
                        .map(|r| r.title.as_str())
                        .unwrap_or("<untitled>");
                    // **Held or not is the whole diagnostic.** Where exactly
                    // one claimant has a file, the other is what an authority
                    // says belongs there — and the file is on the wrong
                    // recording rather than missing.
                    // **The measured length of the file, where there is one.**
                    // It is the evidence that decides these: an identification
                    // matching on duration took the live `Someone Like You` by
                    // 1.4 seconds over the studio one's 10, and seeing the
                    // number beside both claimants is what makes that legible.
                    let (mark, secs) = match playable.get(&recording) {
                        Some(ms) => ("held", f64::from(*ms) / 1000.0),
                        None => ("    ", 0.0),
                    };
                    println!("  {mark}  {secs:>7.1}s  {recording}  {title}");
                }
            }
            eprintln!(
                "\n{found} position(s) claimed by more than one recording, across {} medium(s).",
                media.len()
            );
            if found > 0 {
                eprintln!(
                    "A slot is minted from medium and recording, not medium and position, so two\n\
                     claimants are two slots rather than one correction. Where exactly one is held,\n\
                     the file is bound to the wrong recording and `relink` moves it."
                );
            }
        }
        gmuse::cli::Shelf::Verify => {
            // **A read, so the snapshot and never a scan** — #220. Walking the
            // disk here answered with what the tags say over what the library
            // recorded, which is the one thing the library exists to settle.
            let projection =
                gmuse::librarian::reading(&loaded.data_dir, device, gmuse::history::now_ms());
            let cat = projection.catalogue();
            // **This device's holdings.** A locator another machine published
            // names a path that is not here, and hashing it is not something
            // this run can do or should claim to have done.
            let mine = cat.holdings.get(&device);
            let held: Vec<(
                &String,
                gmuse::model::id::Id<gmuse::model::entity::Encoding>,
            )> = mine
                .map(|a| a.held.iter().map(|(id, at)| (at, *id)).collect())
                .unwrap_or_default();

            let (mut agrees, mut differs, mut unreadable, mut gone) =
                (0usize, 0usize, 0usize, 0usize);
            for (at, said) in &held {
                match still_holds(at, *said) {
                    Holds::Agrees => agrees += 1,
                    // **Both ids, because the difference is the finding.** What
                    // the library believes it is holding, and what is there.
                    Holds::Changed(now) => {
                        differs += 1;
                        println!("changed\t{at}\t{said}\t{now}");
                    }
                    Holds::Unreadable(why) => {
                        unreadable += 1;
                        println!("unreadable\t{at}\t{why}");
                    }
                    Holds::Gone => {
                        gone += 1;
                        println!("gone\t{at}");
                    }
                }
            }
            eprintln!(
                "{} holding(s) checked: {agrees} agree, {differs} changed, {unreadable} unreadable, {gone} not on disk",
                held.len()
            );
            if differs == 0 && unreadable == 0 && gone == 0 {
                eprintln!("every copy still hashes to what the library says it is");
            } else if differs > 0 {
                eprintln!(
                    "\nA changed copy means the bytes were replaced without the library being told.\n\
                     Its recording, its slot and everything said about it still name the old audio.\n\
                     `gmuse shelf import <path>` takes the new bytes in as what they are."
                );
            }
        }
        gmuse::cli::Shelf::Artists => {
            // **A read, so no scan** — #220. Walking the disk here answered
            // with what the tags say over what the library recorded, which is
            // the one thing the library exists to settle.
            //
            // The log itself is opened, because this arm reads **facts** and
            // not only the fold: a read may derive, it may not discover.
            let (log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            // The snapshot for the fold; the log above is open because this
            // arm walks **facts**, which the projection does not carry.
            let projection =
                gmuse::librarian::reading(&loaded.data_dir, device, gmuse::history::now_ms());
            let cat = projection.catalogue();

            // **Every name an id has ever held, not just the one in force.**
            // A name-derived id can be renamed afterwards — `Flume & Chet
            // Faker` was minted and is now called `Flume` — and testing only
            // the current name would call that id MusicBrainz-derived and
            // merge the wrong way. So an id is name-derived if *any* name it
            // has ever been given mints it.
            let mut from_name: std::collections::BTreeSet<
                gmuse::model::id::Id<gmuse::model::entity::Artist>,
            > = Default::default();
            for record in log.records() {
                if let gmuse::facts::Fact::ArtistNamed { artist, name } = &record.fact
                    && gmuse::model::id::Id::<gmuse::model::entity::Artist>::of_artist(None, name)
                        == *artist
                {
                    from_name.insert(*artist);
                }
            }

            // How much work sits under each id, so a person can see which half
            // of a split is the big one. Credits rather than files: an artist
            // is credited on recordings, and not every recording is held.
            let mut weight: std::collections::BTreeMap<_, usize> = Default::default();
            for (_, artist, ..) in cat.credits.keys() {
                *weight.entry(*artist).or_default() += 1;
            }

            let mut by_name: std::collections::BTreeMap<String, Vec<_>> = Default::default();
            for (id, name) in &cat.artist_names {
                by_name
                    .entry(gmuse::model::ingest::normalise(name))
                    .or_default()
                    .push(*id);
            }

            let (mut split, mut refused) = (0usize, 0usize);
            for (_, ids) in by_name {
                if ids.len() < 2 {
                    continue;
                }
                let (mut minted, mut named): (Vec<_>, Vec<_>) =
                    ids.iter().partition(|id| !from_name.contains(id));
                println!("{}", cat.artist_names[&ids[0]]);
                for id in &ids {
                    let how = if from_name.contains(id) {
                        "from the name"
                    } else {
                        "from MusicBrainz"
                    };
                    let n = weight.get(id).copied().unwrap_or(0);
                    println!("  {id}  {n:4} credit(s)  {how}  {:?}", cat.artist_names[id]);
                }
                // **Two MusicBrainz ids are two artists.** MusicBrainz saying
                // so is evidence for two, not a collision to tidy away, and
                // `Christian Löffler` is held under two here. No line, because
                // there is nothing this can propose that would be honest.
                if minted.len() != 1 {
                    refused += 1;
                    println!(
                        "  -- no merge offered: {} of these came from MusicBrainz, and \
                         MusicBrainz saying two artists is evidence for two",
                        minted.len()
                    );
                    continue;
                }
                split += 1;
                let winner = minted.pop().expect("exactly one by the check above");
                // Heaviest loser first, so the biggest half of the split reads
                // directly under the winner it is going into.
                named.sort_by_key(|id| std::cmp::Reverse(weight.get(id).copied().unwrap_or(0)));
                for loser in named {
                    println!(
                        "  {}",
                        serde_json::to_string(&serde_json::json!({
                            "fact": "artists_merged",
                            "loser": loser.to_string(),
                            "winner": winner.to_string(),
                        }))?
                    );
                }
            }
            eprintln!(
                "\n{split} artist(s) held twice, {refused} name(s) left alone. The line under \
                 each is the merge that would join them, and the MusicBrainz id is the winner \
                 because it is the rung a second device would also derive."
            );
        }
        gmuse::cli::Shelf::Tree { levels, tracks } => {
            // **Parse at the boundary** (§12): the spec becomes `Levels`
            // before anything groups by it, through the *same* parser the `:`
            // line uses. A second reader of `genre@1` is a second place for
            // the depth syntax to drift, and #79's whole point is that the
            // compound string stops being load-bearing.
            let spec =
                gmuse::cmd::parse::levels_spec(levels).map_err(|e| format!("--levels: {e}"))?;
            let Some(pair) = spec.pair() else {
                // **`flat` is a level list of none**, and a tree of no levels
                // is one row per track — which `shelf query` already prints.
                // Two ways to answer one question is the shape this project
                // removes (#113), so this says where the answer lives rather
                // than growing a second printer for it.
                return Err(
                    "--levels flat is one row per track; that is `gmuse shelf query`".into(),
                );
            };

            // **A read, so the snapshot and never a scan** — #220. Walking the
            // disk here answered with what the tags say over what the library
            // recorded, which is the one thing the library exists to settle.
            let projection =
                gmuse::librarian::reading(&loaded.data_dir, device, gmuse::history::now_ms());
            let librarian = gmuse::model::seam::Librarian::from_catalogue(projection.catalogue());

            // Every row the library holds. The browser groups its *members* —
            // whatever the active filter left — and a filter is a separate
            // sentence from a level, so this asks the levels question against
            // the whole shelf and says how many rows that was.
            let members: std::collections::HashSet<_> = {
                use gmuse::model::rows::Listing as _;
                librarian.rows().into_iter().collect()
            };

            // `true` because that is what the UI starts with — the point is to
            // read what the browser would draw, not a second arrangement of it.
            let tree = gmuse::model::tree::Tree::grouped(&librarian, pair, true, &members);

            let (outer, inner) = pair;
            println!("# levels: {},{}", outer.written(), inner.written());
            let mut headings = 0usize;
            let mut albums = 0usize;
            let mut rows = 0usize;
            for (key, under) in tree.artists() {
                headings += 1;
                println!("{}", tree.artist_name(key));
                for album in under.values() {
                    albums += 1;
                    let year = album
                        .first_released
                        .map_or_else(String::new, |d| format!(" ({})", d.year));
                    println!("  {}{year}  [{} track(s)]", album.title, album.tracks.len());
                    rows += album.tracks.len();
                    if *tracks {
                        for listing in &album.tracks {
                            let Some(row) = librarian.row_of(listing.recording, listing.copy)
                            else {
                                continue;
                            };
                            // The **row** is the appearance (#155), so the
                            // slot comes off it and not off the recording: a
                            // song on an album and on a compilation is two
                            // rows with two positions, not one with a guess.
                            let slot = match listing.row {
                                gmuse::model::rows::CopyId::On(track) => Some(track),
                                gmuse::model::rows::CopyId::Loose(_) => None,
                            };
                            let (disc, no) = librarian.place_of(slot);
                            // **`u16::MAX` is `place_of`'s "no position"**,
                            // picked so an unnumbered track sorts last rather
                            // than first — a gap in what is known is not a
                            // position. Printing the sentinel would read as a
                            // claim the tags never made.
                            let place = |n: u16| {
                                if n == u16::MAX {
                                    "--".to_string()
                                } else {
                                    format!("{n:02}")
                                }
                            };
                            println!("    {}/{}  {}", place(disc), place(no), row.title);
                        }
                    }
                }
            }
            eprintln!("{headings} heading(s), {albums} album(s), {rows} row(s)");
        }
        gmuse::cli::Shelf::Unfiled => {
            let (mut log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            let report = gmuse::operations::populate::populate(
                &mut log,
                roots,
                gmuse::history::now_ms(),
                gmuse::operations::populate::Admits::Identified,
                &mut gmuse::model::ingest::Reads::default(),
            );
            let mut lines = 0usize;
            for (folder, held) in &report.unfiled {
                for recording in held {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "fact": "collected_into",
                            "recording": recording.to_string(),
                            "collection": folder,
                            "ordered": false,
                        }))?
                    );
                    lines += 1;
                }
            }
            // On stderr, so a redirect to a file gets only the proposals.
            eprintln!(
                "{lines} proposal(s) across {} folder(s) — review, then `gmuse shelf apply --write <file>`",
                report.unfiled.len()
            );
        }
        gmuse::cli::Shelf::Import {
            paths,
            write,
            into,
            unidentified,
        } => {
            let (mut log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            // **Named paths, or whatever a scan found that nothing knows.**
            // The second is the common case and the reason this can be run
            // with no arguments: the scan already said which files it did not
            // recognise, and importing them is the answer to that offer.
            let targets: Vec<std::path::PathBuf> = if paths.is_empty() {
                let seen = gmuse::operations::populate::reconcile(
                    &mut log,
                    roots,
                    gmuse::history::now_ms(),
                    &mut gmuse::model::ingest::Reads::default(),
                );
                // **A duplicate is a filesystem accident, and must not be
                // silent** — `Report::duplicates` has said so since it was
                // written, and nothing printed it. Two paths holding one set
                // of bytes are one row, so the second buys no capability and
                // the only way it comes to light is here.
                for (extra, kept) in &seen.duplicates {
                    println!("{}\t{}", extra.display(), kept.display());
                }
                if !seen.duplicates.is_empty() {
                    eprintln!(
                        "{} duplicate path(s): the same audio held twice, first column is the spare",
                        seen.duplicates.len()
                    );
                }
                seen.unknown
            } else {
                paths.clone()
            };
            if targets.is_empty() {
                eprintln!("nothing to import: every audio file under the roots is already known");
                // **The walk still learned where things are.** Nothing else
                // persists holdings, so returning here threw away the one
                // record that says a file moved — and files move, by hand and
                // by `render`.
                if *write {
                    let n = land(&mut log, &loaded.data_dir)?;
                    if n > 0 {
                        eprintln!("{n} record(s) written: the holdings moved");
                        drop_cache(cache_path.as_deref());
                    }
                }
                return Ok(());
            }

            let report = gmuse::operations::populate::populate(
                &mut log,
                &targets,
                gmuse::history::now_ms(),
                if *unidentified {
                    gmuse::operations::populate::Admits::Anything
                } else {
                    gmuse::operations::populate::Admits::Identified
                },
                &mut gmuse::model::ingest::Reads::default(),
            );
            for (path, recording) in &report.identified {
                println!("{recording}\t{}", path.display());
            }
            for (path, why) in &report.unreadable {
                eprintln!("unreadable: {} — {why}", path.display());
            }
            // **Refused, not resolved** (#148). A rule for picking a winner is
            // a guess that is wrong sometimes and invisibly so, and what it
            // leaves behind is a release group nothing points at.
            for (path, why) in &report.contradicts {
                eprintln!("contradicts the library: {}\n    {why}", path.display());
            }
            // **Kept and reported, not refused** (#62). A re-encode is not a
            // contradiction — one recording with two encodings is the shape
            // `replace` settled on, and history hangs off the recording so it
            // crosses whichever copy is kept. What cannot be inferred is
            // whether this is an upgrade, a duplicate, or a second format
            // wanted on purpose, and that is a question for a person.
            // **Nobody credited to this album is on it** — the check that was
            // missing when a warez site's name became an artist. Reported, not
            // refused: a netlabel in `albumartist` is how Bandcamp writes one,
            // and this library holds `Through My Speakers` legitimately.
            for (credited, album, someone) in &report.uncorroborated {
                eprintln!(
                    "credited to nobody on it: {album:?} is credited to {credited:?}\n                         but its tracks are by {someone:?} — a label, a site stamp, or a mistake?"
                );
            }
            for (path, recording, had) in &report.joins {
                eprintln!(
                    "joins a recording already held as {had} other encoding(s): {}\n    {recording} — an upgrade, a duplicate, or a second format?",
                    path.display()
                );
            }
            already_held(&report, &loaded.data_dir, device);
            // **Refused for want of an identity** — #160. Printed to stdout as
            // `artist \t title \t path`, because the refusal has to be
            // *answerable*: those are the two fields a recording search needs,
            // and an album-shaped lookup is what failed on this material in the
            // first place.
            for (path, artist, title) in &report.unidentified {
                println!("{artist}\t{title}\t{}", path.display());
            }
            eprintln!(
                "{} file(s), {} confirmed, {} resting on their bytes, {} refused for want of an identity, {} refused as contradictions, {} joining a recording already held, {} new fact(s)",
                report.ingested,
                report.confirmed,
                report.provisional,
                report.unidentified.len(),
                report.contradicts.len(),
                report.joins.len(),
                report.written
            );
            if !report.unidentified.is_empty() {
                eprintln!(
                    "\nNothing named those {}, so no performance was minted for them — \
                     `library-api.md` §7.\n\
                     Their bytes are held and what their tags claim is recorded, so they are on\n\
                     the work list: `gmuse shelf query --unidentified` hands each one an encoding\n\
                     and a locator, and an `identified` proposal against it catalogues it from\n\
                     that claim.\n\
                     The columns above are what a MusicBrainz *recording* search wants; the album\n\
                     fields on this material are the least reliable part of it.\n\
                     Or take them as they are with `--unidentified`, which rests them on their\n\
                     bytes — right for a white label, wrong for everything else.",
                    report.unidentified.len()
                );
            }
            if *write {
                // **Publish the holdings first, then place** (#132, #145).
                // The planner asks where a copy *is* before saying where it
                // belongs, and a file taken in a moment ago is not held
                // anywhere until a walk says so.
                //
                // **Over the roots *and* what was named**, because holdings
                // are published whole: walking the roots alone says the device
                // holds nothing outside them, which is false the moment a
                // file is imported from a staging folder — and the planner
                // then has no `from` for it, so it is silently counted
                // `elsewhere` and never placed. Importing 592 files from
                // `~/DanceMusic-staging` catalogued every one of them and
                // moved five.
                let mut walk: Vec<std::path::PathBuf> = roots.to_vec();
                walk.extend(targets.iter().cloned());
                gmuse::operations::populate::reconcile(
                    &mut log,
                    &walk,
                    gmuse::history::now_ms(),
                    &mut gmuse::model::ingest::Reads::default(),
                );

                // Import is strict, so anything that got this far is
                // describable — every row has a credit and a title, because
                // ingest is total — and the renderer cannot fail for want of
                // metadata. What it can refuse is a *destination*: a name too
                // long, a collision. Such a file is catalogued and left where
                // it is, because the library's contents must not depend on
                // path lengths.
                let root = roots.first().cloned().unwrap_or_default();
                let mut placed = gmuse::model::seam::Librarian::from_catalogue(&log.catalogue());
                placed.here(device);
                let taken: Vec<_> = report
                    .identified
                    .keys()
                    .filter_map(|at| gmuse::model::ingest::read(at).ok())
                    .map(|s| gmuse::model::id::Id::of_audio(&s.audio))
                    .collect();
                let plan = gmuse::operations::storage::plan_of(&placed, &root, &taken);
                let done = gmuse::operations::storage::carry_out(&plan, &root, true);
                for (at, why) in &done.failed {
                    eprintln!("could not place {}: {why}", at.display());
                }
                for (copy, why) in &plan.refused {
                    eprintln!("left where it is: {copy} — {why}");
                }
                eprintln!("{} file(s) placed, {} left alone", done.moved, plan.settled);
                // **Said separately, because it is a rename nobody asked for** —
                // #239. "1 file(s) placed" counted audio only, so a cover — or, when
                // this carried anything at all, a stranger — followed in silence.
                if done.assets > 0 || done.discarded > 0 {
                    eprintln!(
                        "{} release file(s) followed the audio, {} the system left removed",
                        done.assets, done.discarded
                    );
                }

                // **And again, because the bytes moved.** Identity is the
                // audio, so a moved file is the same encoding at a new place,
                // and the holdings fact is what carries that.
                //
                // **Over the same walk**, because a file refused a
                // destination is still catalogued and still openable — §10 of
                // `storage-layout.md` says so outright, "catalogued and left
                // where it is". Walking the roots alone drops its holding and
                // turns a playable row into a greyed one.
                if done.moved > 0 {
                    gmuse::operations::populate::reconcile(
                        &mut log,
                        &walk,
                        gmuse::history::now_ms(),
                        &mut gmuse::model::ingest::Reads::default(),
                    );
                }
                // **Filed as it is brought in** (#94). The collection is a
                // judgement a person made, and this is the moment they made
                // it — deriving it afterwards from the folder is the
                // inference the collection model exists to replace.
                if let Some(name) = into {
                    let now = gmuse::history::now_ms();
                    let collection =
                        gmuse::model::id::Id::<gmuse::model::entity::Collection>::of_name(name);
                    log.append_fact(
                        now,
                        gmuse::facts::Fact::CollectionNamed {
                            collection,
                            name: name.trim().to_string(),
                            // A set somebody curated, not a running order —
                            // the same answer `unfiled` gives.
                            ordered: false,
                        },
                    );
                    for recording in report.identified.values() {
                        log.append_fact(
                            now,
                            gmuse::facts::Fact::CollectionEntryAdded {
                                collection,
                                recording: *recording,
                                ord: gmuse::model::ord::OrdKey::first(),
                            },
                        );
                    }
                    eprintln!("{} filed into {name}", report.identified.len());
                }
                let n = land(&mut log, &loaded.data_dir)?;
                eprintln!("{n} record(s) written");
                // **Only when something actually changed.** A re-run that
                // moves nothing and writes nothing has not made the cache
                // wrong, and discarding it would charge the next launch a
                // full rescan for having asked a question.
                if done.moved > 0 || n > 0 {
                    drop_cache(cache_path.as_deref());
                }
            } else {
                eprintln!("dry run — nothing written. Add --write.");
            }
        }
        gmuse::cli::Shelf::Render { write } => {
            let (mut library_log, _) = gmuse::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                gmuse::history::now_ms(),
            );
            let mut library =
                gmuse::model::seam::Librarian::from_catalogue(&library_log.catalogue());
            library.here(device);
            let root = roots.first().cloned().unwrap_or_default();
            let plan = gmuse::operations::storage::plan(&library, &root);

            for (_, from, to) in &plan.moves {
                println!("{}\t{}", from.display(), root.join(to).display());
            }
            for (copy, why) in &plan.refused {
                eprintln!("refused: {copy} — {why}");
            }
            let done = gmuse::operations::storage::carry_out(&plan, &root, *write);
            for (at, why) in &done.failed {
                eprintln!("could not move {}: {why}", at.display());
            }
            eprintln!(
                "{} settled, {} {}, {} refused, {} blocked, {} held elsewhere",
                plan.settled,
                done.moved,
                if *write { "moved" } else { "would move" },
                plan.refused.len(),
                done.failed.len(),
                plan.elsewhere
            );
            // As above (#239): a file that moved without being counted is one
            // nobody can check.
            if done.assets > 0 || done.discarded > 0 {
                eprintln!(
                    "{} release file(s) followed the audio, {} the system left removed",
                    done.assets, done.discarded
                );
            }
            if *write {
                // **The locators follow the bytes.** Identity is the audio, so
                // a moved file is the same encoding at a new place — and the
                // holdings fact is what carries that.
                gmuse::operations::populate::reconcile(
                    &mut library_log,
                    roots,
                    gmuse::history::now_ms(),
                    &mut gmuse::model::ingest::Reads::default(),
                );
                let n = land(&mut library_log, &loaded.data_dir)?;
                eprintln!("{} folder(s) pruned, {n} record(s) written", done.emptied);
                if done.moved > 0 || n > 0 {
                    drop_cache(cache_path.as_deref());
                }
            } else {
                eprintln!("dry run — nothing moved. Add --write.");
            }
        }
        gmuse::cli::Shelf::Query {
            unidentified,
            every_copy,
            filter,
        } => {
            let only = if *unidentified {
                gmuse::query::Only::Unidentified
            } else {
                gmuse::query::Only::Everything
            };
            let copies = if *every_copy {
                gmuse::query::Copies::Every
            } else {
                gmuse::query::Copies::Shown
            };
            // **Parse at the boundary** (§12), through the parser the `:` line
            // uses. A second reader of the filter language is a second place
            // for it to drift.
            let filter = filter
                .as_deref()
                .map(gmuse::query::filter::Expr::parse)
                .transpose()
                .map_err(|e| format!("--filter: {e}"))?;
            let projection =
                gmuse::librarian::reading(&loaded.data_dir, device, gmuse::history::now_ms());
            let library = gmuse::model::seam::Librarian::from_catalogue(projection.catalogue());
            for row in gmuse::query::rows(
                projection.catalogue(),
                &library,
                device,
                gmuse::history::now_ms(),
                only,
                copies,
                filter.as_ref(),
            ) {
                println!("{}", serde_json::to_string(&row)?);
            }
        }
        gmuse::cli::Shelf::Apply { file, write } => {
            // **Claimed only for a real write.** A dry run reads the proposal and
            // says what it would do, and a dry run must not cost a lock.
            let mut outcome = gmuse::operations::apply::run(
                file,
                &loaded.data_dir,
                roots,
                device,
                gmuse::history::now_ms(),
                *write,
            )?;
            println!("{outcome}");
            // **Landed here, like every other operation's facts** (#218). `run` used
            // to commit and now hands back its log, because where the facts go
            // depends on whether an engine holds it and only this side knows.
            if *write {
                let n = land(&mut outcome.log, &loaded.data_dir)?;
                eprintln!("{n} record(s) landed");
            }
            for (line, text) in &outcome.report.malformed {
                println!(
                    "  line {line}: {}",
                    text.chars().take(72).collect::<String>()
                );
            }
            // Named rather than counted: a stale proposal wants re-querying,
            // and a tool cannot re-query what it has not been told about.
            for subject in outcome.report.unknown.iter().take(20) {
                println!("  not held: {subject:?}");
            }
            if outcome.report.unknown.len() > 20 {
                println!("  … and {} more", outcome.report.unknown.len() - 20);
            }
            // **A refusal is a reason and has to be readable.** It was neither
            // counted nor printed: 57 lines the vocabulary refused vanished
            // from a file of 340 and the summary said `283 proposals`, which
            // reads as a file with 283 lines in it. Finding out what happened
            // meant applying the lines by hand, one kind at a time.
            for (line, why) in outcome.report.refused.iter().take(20) {
                println!("  line {line}: {why}");
            }
            if outcome.report.refused.len() > 20 {
                println!("  … and {} more refused", outcome.report.refused.len() - 20);
            }
        }
    }
    Ok(())
}

/// Draw the views until the user quits.
///
/// The terminal is restored by the panic hook `ratatui::init` installs, so a
/// panic mid-render leaves a usable terminal rather than a wedged one.
/// Ask the terminal to report key release — #228, D-20.
///
/// **Only `REPORT_EVENT_TYPES`.** It is the one flag that turns a release into
/// an event; the others change how keys are *encoded*, which would rewrite what
/// every existing binding matches. A held key also starts arriving as
/// `Repeat` rather than as repeated `Press`, which `is_press()` already drops —
/// so D-7's "a toggle must not flutter under key repeat" is stronger here than
/// it can be without the protocol, where a repeat is indistinguishable from a
/// press.
///
/// Paired with [`hold_off`] around every `ratatui::restore`, because restoring
/// the screen hands the flags back with it.
fn hold_on(capable: bool) {
    if !capable {
        return;
    }
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::PushKeyboardEnhancementFlags(
            ratatui::crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    );
}

/// Give the enhancement flags back — #228. See [`hold_on`].
fn hold_off(capable: bool) {
    if !capable {
        return;
    }
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::PopKeyboardEnhancementFlags
    );
}

fn run_tui(
    app: &mut App,
    smart_artist_sort: bool,
    loaded: gmuse::config::Loaded,
    log_path: Option<PathBuf>,
    resume: bool,
    migrate_data: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // History is loaded before the library, not after: a catalog may be defined
    // by `play_count` or `days_since_added`, and building the projection
    // without it would silently give the wrong membership until something
    // happened to trigger a rebuild.
    let history_path = gmuse::history::path_in(&loaded.data_dir);
    // Before the first read, so an upgrade finds the ratings rather than an
    // empty file that looks exactly like having lost them.
    if let Some(from) = gmuse::history::migrate(&history_path) {
        println!(
            "history: moved {} -> {}",
            from.display(),
            history_path.display()
        );
    }
    // Did `data_dir` move out from under the data? Checked before the first
    // read, so an empty history is explained rather than merely reported.
    let state_dir = gmuse::config::default_state_dir();
    match gmuse::datadir::check(
        gmuse::datadir::remembered(&state_dir).as_deref(),
        &loaded.data_dir,
        gmuse::datadir::has_data,
    ) {
        gmuse::datadir::Check::Fine => gmuse::datadir::remember(&state_dir, &loaded.data_dir),
        gmuse::datadir::Check::Stranded { from } => {
            if migrate_data {
                match gmuse::datadir::move_data(&from, &loaded.data_dir) {
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
                            gmuse::datadir::remember(&state_dir, &loaded.data_dir);
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

    // Every log in the directory — this device's and any that arrived by sync.
    //
    // **No `moved` bookkeeping and no `seen` sweep.** Both existed to keep a
    // path-keyed history attached to files that move, and neither has anything
    // to do now that a fact names a recording (#81). A rename is invisible to
    // the log, which is the point.
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let now = gmuse::history::now();
    let now_ms = gmuse::history::now_ms();
    // **Parsed only when this process owns the log** — #261.
    //
    // A view attached to an engine follows the projection snapshot instead, which is 5.7 MB of
    // postcard against 87 MB of JSONL — and the 83.5 MB of records it parsed were used by
    // nothing it does: production asks its desk for `current`, `advance`, `settled`, `reread`
    // and `history`, and only the last wants a record at all.
    let attached = app.player.attached();
    let (log, damage) = match attached {
        false => gmuse::model::instance::Instance::open(&loaded.data_dir, device, now_ms),
        true => (gmuse::model::instance::Instance::new(device), Vec::new()),
    };
    if !log.records().is_empty() {
        println!("history: {} records", log.records().len());
    }
    for torn in &damage {
        eprintln!(
            "history: unreadable line {} in {}",
            torn.line,
            torn.path.display()
        );
    }
    for drift in &log.drifts {
        eprintln!("history: clock drift, {drift:?}");
    }
    // **Said, because nothing else says it any more** — #238. A shared stamp used to
    // announce itself by a fact going missing; now the order breaks the tie by the
    // fact, so this line is the only sign that a device id is being claimed twice.
    if log.collisions > 0 {
        eprintln!(
            "history: {} record(s) share a stamp with another — two processes or two \
             machines are using one device id",
            log.collisions
        );
    }
    let first_run = log.records().is_empty();

    // One scan, two shapes. The store is what the player still reads through;
    // the librarian is what it is moving to (#77), and building both from the
    // same pass is what keeps them from disagreeing.
    let assembling = std::time::Instant::now();
    // The log crosses here and does not come back: from this point the only
    // thing that folds is the librarian thread (I16), and this side holds
    // whatever it last published. `Desk::open` folds once synchronously, so
    // nothing below sees an empty library.
    //
    // **Attached, it is a reading desk** — #219, I15, §4.4. I15 lets a view fold
    // the log directly and that is what keeps browsing off the socket; what it
    // does not let a view do is *add* to it, because `durable::log_path` is one
    // file per device and the device comes from `machine_id`. Two processes on
    // one machine would be two appenders claiming to be one device, which breaks
    // what `Stamp.device` is for.
    //
    // So the difference is a capability rather than a flag: a reading desk has
    // no `Appender`, and the three places that used to ask `if attached` — the
    // scan below, `:rescan`, and `ui::record` — now ask for one and get `None`.
    // **The library, when it is somewhere else** — #261, §4.2. Opened before the desk, because
    // whether this process derives a `Librarian` at all depends on whether anything will read
    // it: with a reader, nothing does, and that is 26.5 MB not folded.
    let reader: Option<gmuse::ui::holding_remote::Remote> = attached
        .then(|| {
            gmuse::engine::attach::attach("gmuse-tui-reads", env!("CARGO_PKG_VERSION"))
                .inspect_err(|e| eprintln!("library: cannot read through the engine: {e}"))
                .ok()
                .map(|c| gmuse::ui::holding_remote::Remote::open(std::sync::Arc::new(c)))
        })
        .flatten();
    let desk = if attached && reader.is_some() {
        // **Folds nothing at all.** Rows come over the socket and so does `listened`, so there
        // is nothing left in the log for this process to read — §4.2's rule followed through.
        gmuse::librarian::Desk::elsewhere(device)
    } else if attached {
        gmuse::librarian::Desk::read_only(
            app.store.clone(),
            device,
            now_ms,
            loaded.data_dir.clone(),
        )
    } else {
        gmuse::librarian::Desk::open(
            log,
            app.store.clone(),
            device,
            now_ms,
            Some(loaded.data_dir.clone()),
        )
    };
    // **Reconciled at start, and not waited on** — the 2026-09-10 decision
    // that a stale cache is the worse failure, now that it costs nothing to
    // act on. The library is already usable: holdings come from the cached
    // store and the log, so this closes drift rather than supplying the rows.
    //
    // Warm it is ~0.3s, because the read cache settles 7,266 of 7,267 files
    // with a `stat`. Cold it re-reads everything, which is why it is a job.
    //
    // **And only with something to append with.** A scan appends, so a reading
    // desk has no way to start one — the engine reconciled the same roots when it
    // started, and `:rescan` goes over the wire.
    if !app.roots.is_empty()
        && let Some(appender) = desk.appender()
    {
        appender.scan(app.roots.clone(), app.cache_path.clone());
    }
    let library_view = desk.current();
    // **Counted through whichever library this view reads**, which is the engine's when there
    // is one — `library_view.library` is empty then, by construction.
    let known = gmuse::model::rows::Listing::rows(reading(&reader, &library_view)).len();
    println!(
        "librarian: {known} recordings in {:.1}s",
        assembling.elapsed().as_secs_f64()
    );

    // **A scan no longer catalogues what it walks past** (#140), so files on
    // disk that nobody imported are not rows — and an empty library with a
    // full disk needs saying out loud rather than being left to look like a
    // bug. `import` is the declaration; this is where it is offered.
    //
    // **Counted in encodings, not recordings.** Two copies of one track are
    // two files and one recording, so subtracting the second from the first
    // reported 175 strangers on a library that had none.
    let strangers = app
        .store
        .iter()
        .filter(|h| {
            !library_view
                .library
                .holds_encoding(gmuse::model::id::Id::of_audio(&h.audio))
        })
        .count();
    if strangers > 0 {
        println!(
            "library: {strangers} file(s) on disk are not in the library yet — \n\
             `gmuse shelf import` to see them, `--write` to take them in"
        );
    }

    // **Membership is asked of the same seam the rows come from** (#86), so
    // the two cannot disagree about what exists — which is what made the
    // `file_by_tag` fallback reachable and is why it is gone.
    //
    // One fold over one stack (#94, #119): a view is the only thing that says
    // what is in scope, and an empty one is the whole library.
    let composed = app
        .view
        .as_deref()
        .and_then(|src| gmuse::view::Stack::parse(src).ok())
        .unwrap_or_default();
    // From the same fold the librarian published, so this and the view it is
    // resolved against cannot disagree about what the log says.
    let listened = &*library_view.listened;
    let (members, _, _) = composed.members(
        reading(&reader, &library_view),
        listened,
        gmuse::query::filter::Env {
            listened: Some(listened),
            named: None,
            now,
        },
    );

    // **Grouped by what the model says, not by what the tags carry** (#84).
    // The librarian has to exist first, which is why this sits below rather
    // than beside the membership it is built from.
    // **Only a detached view groups its own shelf.** With a reader, `ui::holding` picks the
    // remote on the first refresh and this one would be a shelf over an empty library.
    let library: Box<dyn gmuse::browse::Holding> = match &reader {
        Some(r) => Box::new(r.clone()),
        None => Box::new(Shelf::project(
            &library_view.library,
            members,
            smart_artist_sort,
        )),
    };

    let mut library_view = library_view;
    /// Where the library is read from: the engine, or this process's own fold.
    fn reading<'a>(
        reader: &'a Option<gmuse::ui::holding_remote::Remote>,
        lv: &'a gmuse::librarian::Answer,
    ) -> &'a dyn gmuse::model::rows::Listing {
        match reader {
            Some(r) => r,
            None => &lv.library,
        }
    }
    let ctx = gmuse::ui::Context {
        library: reading(&reader, &library_view),
        player: Some(app.player.transport()),
        gapless: app.player.gapless(),
    };
    let mut state = gmuse::ui::Ui::new(library);
    // **This counts plays only when the device is ours** — #257. Attached, the engine does it,
    // and both doing it would count every track twice.
    state.plays_here = !app.player.attached();
    // **The library connection, separate from the transport's** — #236.
    //
    // A `Client` has one `Receiver<Event>`, so two readers of one connection
    // would steal each other's events — `Remote` already has a thread on
    // `playback`. Two subscriptions get two sockets, which is what the engine is
    // built to serve and what §7.10 makes cheap (a disconnect is a non-event).
    //
    // The engine is certainly up by now: `Playback::Attached` only exists because
    // `attach` connected to one.
    // **Set before the subscription, not inside it** — #261. The desk above already skipped
    // deriving a `Librarian` because there is a reader, so a view that lost its *events* and
    // kept no reader would draw an empty library rather than a stale one.
    state.reader = reader.clone();
    if app.player.attached() {
        match gmuse::engine::attach::attach("gmuse-tui-library", env!("CARGO_PKG_VERSION")) {
            Ok(client) => {
                let client = std::sync::Arc::new(client);
                if let Err(e) =
                    client.subscribe(&[gmuse_proto::Topic::Library, gmuse_proto::Topic::Queue])
                {
                    eprintln!("library: cannot follow the engine's changes: {e}");
                } else {
                    state.engine = Some(client);
                }
            }
            // **Not fatal, and said.** Without it the view still reads and still
            // plays; what it loses is writing and noticing. Refusing to start
            // would be worse than starting degraded, and silence would be worse
            // than both.
            Err(e) => eprintln!("library: cannot reach the engine to write: {e}"),
        }
    }
    state.playlists = loaded.playlists.clone();

    // Opened before the terminal is taken over, so a failure is visible.
    if let Some(path) = log_path {
        match gmuse::audit::Audit::open(&path) {
            Ok(mut audit) => {
                audit.line(
                    "device",
                    &format!(
                        "{} Hz output{}",
                        app.player.device_rate(),
                        // Said, because the two lines mean different things: one
                        // is this process's device and the other is a report of
                        // somebody else's.
                        if app.player.attached() {
                            " — the engine's, attached"
                        } else {
                            ""
                        }
                    ),
                );
                audit.line(
                    "library",
                    &format!(
                        "{} tracks, view: {}",
                        app.store.len(),
                        app.view.as_deref().unwrap_or("all")
                    ),
                );
                println!("logging to {}", path.display());
                state.audit = audit;
            }
            Err(e) => eprintln!("log: cannot open {}: {e}", path.display()),
        }
    }

    state.set_leader(loaded.leader.unwrap_or_else(gmuse::config::default_leader));
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
    let fixed: std::collections::HashSet<gmuse::cmd::SettingKey> = loaded
        .commands
        .iter()
        .filter_map(|c| match c {
            gmuse::cmd::Command::Set { key, .. } => Some(*key),
            _ => None,
        })
        .collect();

    // The config's bindings and settings are commands, so they run through the
    // same execute() a keypress does — there is no second configuration path
    // to keep in step.
    for cmd in loaded.commands {
        gmuse::ui::execute(&mut state, &ctx, cmd);
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
    // **Say which one, and keep saying it** — #227, acceptance criterion 2.
    //
    // This counted. A count tells you something is wrong and not what, and the
    // detail went to `eprintln!` — which runs before `ratatui::init` takes the
    // screen, so it is legible only after you quit. In session you got
    // "3 problem(s) in the config" for four seconds, during the same startup
    // that fires several other notices.
    //
    // So: the first offender by name, the rest counted, and it does not expire.
    // Every one of them also goes to the audit, which is what the block above
    // does with unreadable tags and what this one should always have done.
    //
    // It still starts. A typo in one binding disables one key; refusing to
    // launch over it would cost the whole player, and the failure it prevents
    // is already visible the moment you press that key.
    if !loaded.errors.is_empty() {
        let first = &loaded.errors[0];
        state.warn(match loaded.errors.len() - 1 {
            0 => format!("config: {first}"),
            more => format!("config: {first} (+{more} more)"),
        });
        for e in &loaded.errors {
            state.audit.line("config", &e.to_string());
            eprintln!("config: {e}");
        }
    }

    // An acquisition date is a *library* fact now, so it arrives with an
    // import rather than being invented by whichever machine noticed the file
    // first. Nothing is stamped here.
    {
        // From the published fold; the log is on the far side of the channel.
        let held = desk.current();
        let records = held.library.record_count;
        let known = held.listened.ratings.len() + held.listened.plays.len();
        drop(held);
        state.desk = desk.into_enquiries();
        gmuse::ui::refresh_listened(&mut state);
        state.data_dir = Some(loaded.data_dir.clone());
        state.audit.line(
            "history",
            &if first_run {
                "no history yet — ratings and plays start here".to_string()
            } else {
                format!("{records} records, {known} recordings with history")
            },
        );
    }

    // Where we were last time, restored paused. After the config, so `fixed`
    // means something; before the settings summary, so the summary reports
    // what is actually in force.
    if resume {
        let path = gmuse::session::default_path();
        if let Some(saved) = gmuse::session::load(&path) {
            gmuse::ui::restore_session(&mut state, &ctx, &saved, &fixed);
            state.audit.line(
                "session",
                &format!(
                    "restored {} queued, {}",
                    saved.queue.len(),
                    saved
                        .current
                        .as_ref()
                        .and_then(|p| {
                            gmuse::model::rows::Listing::row(
                                reading(&reader, &library_view),
                                p.recording,
                            )
                        })
                        .map(|row| {
                            format!(
                                "at {:.1}s in {} — {}",
                                saved.position_secs, row.file_under, row.title
                            )
                        })
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
            "replaygain={} shuffle={} scope={}",
            state.replaygain.label(),
            state.playback.shuffle,
            state.playback.scope.label(),
        ),
    );

    // **The loop hands back a reason, and this is what acts on it** (#120).
    //
    // The librarian is owned here and lent to the loop, so nothing inside
    // could ever rebuild it — the borrow outlives every command. Now the loop
    // returns, the borrow ends, the fold is redone, and it runs again. The
    // state the UI holds is untouched: only what it *reads through* is
    // replaced.
    // **Ask once, before the screen is taken** — #228, D-20.
    //
    // `Release` never arrives without the kitty keyboard protocol, so a
    // momentary verb has to know which world it is in. kitty, WezTerm, Ghostty
    // and foot answer yes; Terminal.app does not, and there a held key latches
    // instead — which the status line says, so the difference is visible rather
    // than mysterious.
    //
    // A probe that errors is a no: the fallback works everywhere, and guessing
    // yes would leave a verb that silently does nothing.
    state.hold_capable =
        ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);

    let mut terminal = ratatui::init();
    hold_on(state.hold_capable);
    let result = loop {
        let stop = {
            let ctx = gmuse::ui::Context {
                library: reading(&reader, &library_view),
                player: Some(app.player.transport()),
                gapless: app.player.gapless(),
            };
            match gmuse::ui::run(&mut terminal, &mut state, &ctx) {
                Ok(stop) => stop,
                Err(e) => break Err(e),
            }
        };
        // Every borrow of `library_view` has ended here, which is the whole
        // reason the loop returns rather than acting in place.
        match stop {
            gmuse::ui::Stop::Quit => break Ok(()),
            gmuse::ui::Stop::Rescan => {
                // **Asked of whoever can append** — I15, §4.4. A scan appends,
                // so a reading desk has no way to start one and the engine that
                // owns the log owns the reconcile too. This view then learns what
                // it found through `library`, like any other client.
                //
                // Neither branch waits: a cold pass re-reads every file and this
                // thread is the one drawing. A local scan reports on `desk.scans`,
                // which the draw loop drains.
                let said = if let Some(appender) = state.desk.appender() {
                    appender.scan(app.roots.clone(), app.cache_path.clone());
                    "rescan: started".to_string()
                } else {
                    match &app.player {
                        Playback::Attached(remote) => {
                            match remote.client().notify(
                                gmuse_proto::request::LibraryOp::Scan.method(),
                                Some(serde_json::json!({})),
                            ) {
                                Ok(()) => "rescan: asked the engine".to_string(),
                                Err(e) => format!("rescan: the engine did not take it — {e}"),
                            }
                        }
                        // Unreachable, and said rather than assumed: the two
                        // facts are set in different places, so "a local player
                        // has an appending desk" is worth stating where it would
                        // be noticed if it stopped being true.
                        Playback::Local(_) => {
                            "rescan: nothing to scan with and no engine to ask".to_string()
                        }
                    }
                };
                state.notify(said);
                library_view = state.desk.current();
            }
            gmuse::ui::Stop::Refold => {
                state.desk.advance();
                state.desk.settled();
                library_view = state.desk.current();
            }
            // **Outside the draw loop, which is the whole point of the trip
            // out here** (#99): the editor wants the terminal, so the screen
            // is given back before it runs and taken again after.
            gmuse::ui::Stop::Edit => {
                hold_off(state.hold_capable);
                ratatui::restore();
                let said = match gmuse::edit::edit(&state.composed, state.data_dir.as_deref()) {
                    Ok(back) => {
                        let label = back.label();
                        state.composed = back;
                        if label.is_empty() {
                            "view: everything".to_string()
                        } else {
                            format!("view: {label}")
                        }
                    }
                    // **A parse error names its line and nothing is applied.**
                    // Half-applying an edited document would leave a view
                    // nobody wrote, which is worse than the edit not landing.
                    Err(e) => format!("{e}"),
                };
                state.audit.line("edit", &said);
                state.notify(said);
                terminal = ratatui::init();
                // **And again**, because `restore` gave the flags back with the
                // screen. A round trip through the editor that forgot this would
                // leave hold-to-preview working until the first `:edit`.
                hold_on(state.hold_capable);
            }
        }
        // The shelf is a projection of the librarian, so it goes too — through
        // the same door a `:view` change uses, or refiltering would file
        // things a second way (#84).
        let ctx = gmuse::ui::Context {
            library: reading(&reader, &library_view),
            player: Some(app.player.transport()),
            gapless: app.player.gapless(),
        };
        gmuse::ui::refresh_library(&mut state, &ctx);
    };
    hold_off(state.hold_capable);
    ratatui::restore();
    // Written after the terminal is restored, so a failure is visible.
    // A fresh borrow of whatever the last refold produced, not the one the
    // setup above took: that one is long gone, and it named an older fold.
    let ctx = gmuse::ui::Context {
        library: reading(&reader, &library_view),
        player: Some(app.player.transport()),
        gapless: app.player.gapless(),
    };
    gmuse::ui::save_session(&mut state, &ctx);
    // Written after the terminal is restored, so the summary is visible.
    if state.audit.is_enabled() {
        // **Through the seam, which reports 0 when attached.** An attached view
        // has no ring to starve; the engine's underruns are the engine's to
        // report, and §7.8 puts them on `meters` rather than `playback`.
        let underruns = app.player.transport().underruns();
        state
            .audit
            .line("session", &format!("ended, {underruns} underruns in total"));
    }
    result?;
    Ok(())
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

/// What one holding turned out to be, when the bytes were read again.
#[derive(Debug, PartialEq)]
enum Holds {
    /// The file hashes to the id the library recorded for it.
    Agrees,
    /// It hashes to something else — the bytes were replaced without the
    /// library being told, and every id derived from the old ones now names
    /// audio that is not there.
    Changed(gmuse::model::id::Id<gmuse::model::entity::Encoding>),
    /// It is there and will not decode.
    Unreadable(String),
    /// The holding names a path that no longer exists.
    Gone,
}

/// **Read the bytes again and compare** — `shelf verify`.
///
/// Identity here is the audio, so one claim per copy carries the library's
/// whole account of itself: these bytes produce this `AudioId`. Nothing
/// checked it until this existed.
fn still_holds(at: &str, said: gmuse::model::id::Id<gmuse::model::entity::Encoding>) -> Holds {
    let path = std::path::Path::new(at);
    if !path.exists() {
        return Holds::Gone;
    }
    match gmuse::model::ingest::read(path) {
        Ok(scanned) => {
            let now =
                gmuse::model::id::Id::<gmuse::model::entity::Encoding>::of_audio(&scanned.audio);
            if now == said {
                Holds::Agrees
            } else {
                Holds::Changed(now)
            }
        }
        Err(e) => Holds::Unreadable(e.to_string()),
    }
}

/// **Where each copy is still held after this retirement** — the guard
/// `shelf remove` refuses on.
///
/// Two paths can be one encoding, because identity is the audio: `Early James`
/// was retired by moving one of two folders holding identical rips, ten of its
/// twelve files were the same encoding as a file still in the library, and
/// retiring "the departed copy" retired the live one too. The library went from
/// 6,648 settled to 6,638 before this existed.
///
/// **And `elsewhere` has to mean somewhere that is not going.** The first
/// version compared against every holding outside `.departed`, which includes
/// the files being retired — so each one reported itself as the reason it could
/// not leave, and `remove` retired nothing at all. It could only tombstone a
/// file somebody had already moved into `.departed` by hand, which is the one
/// case the path filter happened to exclude. Retiring 32 files reported
/// `0 to retire`, naming each file's own path as the copy that survived it.
fn surviving_holders<'a>(
    cat: &'a gmuse::projection::catalogue::Catalogue,
    departed: &std::path::Path,
    going: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeMap<gmuse::model::id::Id<gmuse::model::entity::Encoding>, Vec<&'a str>>
{
    let mut out: std::collections::BTreeMap<_, Vec<&str>> = Default::default();
    for asserted in cat.holdings.values() {
        for (id, at) in &asserted.held {
            if !std::path::Path::new(at).starts_with(departed) && !going.contains(at) {
                out.entry(*id).or_default().push(at.as_str());
            }
        }
    }
    out
}

/// Run the engine until interrupted — #214.
///
/// **What this is not, yet.** It binds, handshakes and answers `hello`, and
/// every other method is a `-32601`. The methods arrive with #215–#218, and
/// standing a door up before there is anything behind it is deliberate: §15
/// wants the protocol operable from a shell before the TUI depends on it, and
/// the handshake is what a client has to get right first.
fn run_engine(
    foreground: bool,
    null_audio: bool,
    // **What device to open, and to reopen** — #241. The engine hardcoded
    // `PlayerConfig::default`-shaped values, so `--device` could not reach it at all
    // and "honour the intent that opened the first stream" had no intent to honour.
    cfg: gmuse::audio::PlayerConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    // **Detaching is not built.** Said out loud rather than accepting the flag
    // and ignoring it, which would leave a view's auto-start (§4.2) silently
    // holding a foreground process.
    if !foreground {
        eprintln!("gmuse engine: only --foreground is implemented; running in the foreground");
    }

    let path = gmuse::engine::socket::default_path();

    // **What is behind the door today.** `hello` is served by the engine
    // itself; these are the three methods whose answers exist without a player
    // attached, and `cmd.exec_line` is the interesting one — it is the second
    // half of I4's rule, routing to the *same* parser the `:` line uses so
    // there is exactly one textual command language (§7.12).
    struct Bare {
        stopping: Arc<std::sync::atomic::AtomicBool>,
        /// **The process's single appender** (I15). Every write goes through
        /// here and there is no other door: `Instance::flush` is crate-private
        /// to `gmuse-library`, so the binary cannot reach the log except by
        /// asking the desk.
        desk: Arc<gmuse::librarian::Desk>,
        /// What to reconcile on a scan.
        roots: Vec<std::path::PathBuf>,
        /// **The last few trees built, so paging does not rebuild one** — #261.
        ///
        /// Paging the real library's tree cost **0.40 s a page** before this, because every
        /// page rebuilt every node. It is now about a millisecond — `wiring::kept` has the
        /// measurement and the reasoning, including why the key is the whole question.
        trees: gmuse::wiring::kept::Trees,
        /// **Which fold a query's page came from** — §7.11's `library_version`, #261.
        ///
        /// The same counter the announcer bumps and the audio engine watches, so a client
        /// paging through a query and a client following the `library` topic are talking about
        /// the same numbers.
        moved: Arc<std::sync::atomic::AtomicU64>,
        /// **The player, behind a channel** — #230. `PlayerHandle` is `!Send`
        /// and `Answers` is `Send + Sync`, so the player cannot live here; what
        /// lives here is the way to ask it for something.
        control: Arc<gmuse::engine::control::Control>,
        /// **Set after `serve` returns**, because the engine owns the answerer
        /// and the answerer needs the engine to publish through. A `OnceLock`
        /// rather than a channel back: written once at startup, read from
        /// connection threads.
        engine: Arc<std::sync::OnceLock<std::sync::Weak<gmuse::engine::Engine>>>,
    }
    impl Bare {
        /// §8.4's named write operations, routed to the one appender.
        fn library(
            &self,
            operation: gmuse_proto::request::LibraryOp,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse_proto::request::LibraryOp;
            use gmuse_proto::{RpcError, error::code};

            match operation {
                // **Acknowledges and reports through events** — §8.4's
                // long-running case. Waiting for a scan here would hold a
                // connection open for thirty to seventy seconds and tell the
                // client nothing while it did; the `library` event that follows
                // says what it found, which is strictly better than today's
                // blocking scan with nowhere to show progress.
                LibraryOp::Scan => {
                    if self.roots.is_empty() {
                        return Err(RpcError::new(
                            code::NOT_READY,
                            "no roots are configured, so there is nothing to reconcile",
                        ));
                    }
                    // **The engine's desk is the appending one**, opened that
                    // way ~300 lines below: §4.4 makes it the process's single
                    // appender, which is the same sentence that makes a view's
                    // desk a reading one.
                    self.appender().scan(self.roots.clone(), None);
                    Ok(serde_json::json!({
                        "accepted": true,
                        "roots": self.roots.len(),
                    }))
                }

                // **The facts a view emits directly** (§8.4). A rating is the
                // one this build can express end to end, which is what makes
                // "a fact appended by one client reaches another" testable at
                // all — the rest need their arguments plumbed and are refused
                // by name rather than pretended at.
                LibraryOp::Edit => self.edit(params),

                // **The tool door** — §8.4's "a file of proposed facts", as a call.
                //
                // Wider than `edit` on purpose, and the difference is what each is
                // *for* rather than what each is allowed. `edit` carries what a view
                // emits and `Fact::is_an_edit` refuses the rest, which catches a view
                // claiming a measurement it never took. This carries what an
                // operation computed from files on disk — an import's reading of
                // tags, a scan's holdings, a `remove`'s retirement — and no
                // predicate can check that a caller really read them.
                //
                // So this is a **trust** boundary and it is worth saying which kind:
                // the socket lives in the user's own state directory, and anything
                // that can reach it can already read and write the files it is
                // talking about. The gate on `edit` catches mistakes, not attackers,
                // and this door is for callers whose whole job is the thing the gate
                // would refuse.
                LibraryOp::Apply => self.apply(params),

                // **Compacting is the appender's, because it rewrites the log.**
                // Dropping superseded holdings assertions is not an append at all, so
                // no other process can do it while the engine holds the lock (#218).
                LibraryOp::Compact => {
                    self.appender().compact();
                    Ok(serde_json::json!({ "accepted": true }))
                }

                other => Err(RpcError::new(
                    code::NOT_READY,
                    format!("library.{} has no arguments plumbed yet", other.as_str()),
                )
                .with_data(serde_json::json!({
                    "reason": "arguments not plumbed",
                    "operation": other.as_str(),
                }))),
            }
        }

        /// **Ask the librarian, which owns the programme** — #257.
        ///
        /// It used to ask the control thread, because the queue lived there with a
        /// materialised pool behind it. Under the triangle the queue is a *librarian*
        /// question: a UI asks the actor that is the authority on the answer, and what plays
        /// next is not something the audio engine decides.
        ///
        /// **Which also takes the control thread off this path entirely.** A socket worker
        /// blocking on the librarian blocks itself; blocking on the control thread put a
        /// client's round trip in front of transport.
        ///
        /// A mutation is followed by `Ask::Resync`, because the engine's window was filled
        /// from the programme as it was — without it a `queue.set` would not be audible until
        /// the window ran dry two tracks later.
        fn programme(
            &self,
            ask: gmuse::librarian::programme::Ask,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse::librarian::programme::Reply;
            use gmuse_proto::{RpcError, error::code};

            let mutating = ask.mutating();
            let Some(reply) = self.desk.programme(ask) else {
                return Err(RpcError::new(
                    code::INTERNAL,
                    "the librarian is not running",
                ));
            };

            if mutating {
                // Best effort: a control thread that has stopped ticking is a separate
                // failure, and the mutation landed either way.
                self.control.ask(gmuse::engine::control::Ask::Resync);
            }

            match reply {
                Reply::Settled { standing } | Reply::Window { standing, .. } => {
                    Ok(serde_json::json!({ "version": standing.version }))
                }
                // **The rarer case than the design assumed**, because a reshuffle is a
                // reorder and keeps every token. This is a genuine replacement.
                Reply::Stale { standing } => Err(RpcError::stale_version(standing.version)),
                Reply::Look {
                    standing,
                    entries,
                    at,
                    next,
                    anchored,
                } => {
                    use gmuse_library::programme::Mode;
                    // **Recordings on the wire, rows underneath** — I17: only user-facing
                    // grains cross, and a recording is what a person chooses.
                    // **Tokens and marks beside the names, one filter for all three** — #271.
                    // A page is what a view edits from, and an edit that names a token
                    // touches that entry wherever the programme has moved it since; one
                    // that named an index would touch whatever is there now.
                    let shown: Vec<(String, u64, bool)> = entries
                        .iter()
                        .filter_map(|e| {
                            self.recording_of(e.row)
                                .map(|r| (r.to_string(), e.token, e.by_hand))
                        })
                        .collect();
                    let named: Vec<&String> = shown.iter().map(|(n, _, _)| n).collect();
                    let tokens: Vec<u64> = shown.iter().map(|(_, t, _)| *t).collect();
                    let by_hand: Vec<bool> = shown.iter().map(|(_, _, h)| *h).collect();
                    Ok(serde_json::json!({
                        "version": standing.version,
                        "entries": named,
                        "tokens": tokens,
                        "by_hand": by_hand,
                        "total": standing.total,
                        // **Where this page sits**, so a view can say where a person is in a
                        // programme rather than leaving them to guess.
                        "at": at,
                        "next": next,
                        // **Whether the first entry is the one sounding.** The default anchor
                        // is what is playing, and a token the programme no longer holds pages
                        // from the head instead — so `at: 0` alone cannot tell a view whether
                        // to draw the first row as "now" or as "next". A caller that passed
                        // its own `from` reads this as "the page starts where I asked".
                        "anchored": anchored,
                        // **The same four fields the `queue` event carries** (#233), so a view
                        // that just attached can draw the queue correctly without waiting for
                        // one of them to change.
                        "scope": standing.scope.label(),
                        "shuffle": standing.shuffled,
                        "repeat": standing.mode == Mode::RepeatAll,
                        "repeat_current": standing.mode == Mode::RepeatCurrent,
                    }))
                }
            }
        }

        /// **The half of the desk that appends**, which this one always has.
        ///
        /// `Desk::appender` is an `Option` because a *view* holds a reading desk
        /// (#219, I15); the engine's is opened appending, so the absent case is a
        /// construction error rather than a state to handle. Named once here so
        /// the reason lives in one place instead of at each call site.
        fn appender(&self) -> gmuse::librarian::Appender<'_> {
            self.desk
                .appender()
                .expect("§4.4: the engine's desk is the process's appender")
        }

        /// The row that draws a recording, for the device this engine is on.
        fn row_of(
            &self,
            recording: gmuse::model::id::Id<gmuse::model::entity::Recording>,
        ) -> Option<gmuse::model::rows::CopyId> {
            // **A scan, and that is the right answer for one row.** #123's
            // entry is the precedent: "building a map of 13,000 credits to ask
            // about one album is worse than walking them". Enqueuing is a
            // handful of ids at human speed, so walking the rows costs less
            // than an index the librarian would have to maintain for it.
            use gmuse::model::rows::Listing;
            let answer = self.desk.current();
            let rows: &dyn Listing = &answer.library;
            rows.rows()
                .into_iter()
                .find(|id| rows.recording_of(*id) == Some(recording))
        }

        /// And back the other way, for reading the queue out.
        /// **What is playing, as one reply** — #243, #248.
        ///
        /// The `playback` topic carries discontinuities and §7.8 forbids a steady
        /// position stream, so a client arriving between two of them learns nothing
        /// until the next one. A status bar cannot subscribe at all, and a view that
        /// attached mid-album drew "stopped" over audible music.
        ///
        /// **The optional `format` renders engine-side, through the same template
        /// language the config and the `:` line use.** That is the 2026-09-01
        /// decision unchanged: this library's `now_playing` puts the title **last**,
        /// so a bar truncating a pre-rendered line throws away the field it wanted.
        /// A caller that wants fields ignores `format` and gets them.
        /// **A page of rows matching a filter** — §7.11, #261.
        ///
        /// Answered here rather than by the librarian thread, and that is the point: a read
        /// needs an atomic load, not a turn in a queue. Appends are what have to be ordered.
        ///
        /// `library_version` says which fold the page came from, so a client paging through can
        /// tell that the library moved under it. The page itself is internally consistent —
        /// one `Arc<Answer>` for the whole of it — and a change *during* one page is invisible,
        /// which is the honest limit of a cursor that is an index.
        fn found(
            &self,
            filter: Option<String>,
            limit: Option<usize>,
            cursor: Option<usize>,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse_proto::RpcError;

            let filter = match filter.as_deref().map(gmuse::query::filter::Expr::parse) {
                Some(Ok(expr)) => Some(expr),
                Some(Err(e)) => return Err(RpcError::invalid_params("filter", e.to_string())),
                None => None,
            };

            // **One atomic load of a shared fold**, and the version beside it so a client can
            // see the library move.
            let answer = self.desk.current();
            let version = self.moved.load(std::sync::atomic::Ordering::Acquire);

            // **The picker path, not the tool path** — #261. `query::rows` produces a locator
            // per recording, which is 208 ms of a 298 ms query on the real library and is not
            // in the reply. `found` resolves one only when the filter reads it.
            let all = gmuse::query::found::found(
                &answer.library,
                &*answer.listened,
                gmuse::history::now_ms(),
                filter.as_ref(),
            );

            // **A screenful, not a library.** A picker draws about forty rows; the cursor is an
            // index into *this* version's result, which the reply names.
            let at = cursor.unwrap_or(0).min(all.len());
            let take = limit.unwrap_or(50).min(1_000);
            let page: Vec<serde_json::Value> = all
                .iter()
                .skip(at)
                .take(take)
                .map(|row| {
                    serde_json::json!({
                        "recording": row.recording.map(|r| r.to_string()),
                        "title": row.title,
                        "artist": row.artist,
                        "file_under": row.file_under,
                        "album": row.album,
                        "length_ms": row.length_ms,
                        // **Where the filter matched** — #261's last done-when. Without this a
                        // picker needs the matcher to underline, and then it needs the rows.
                        "matched": row.matched,
                    })
                })
                .collect();
            let next = at + page.len();

            Ok(serde_json::json!({
                "rows": page,
                "at": at,
                "total": all.len(),
                "next": (next < all.len()).then_some(next),
                "library_version": version,
            }))
        }

        /// **A page of the browse tree** — §7.11, #261.
        ///
        /// Grouped here because grouping is a library question. The *folding* is not, so the
        /// whole shape comes back and a client draws what it has open — which is also what
        /// `tree-view-design.md` says a search reads.
        /// Typed at the edge (§12): the wire says strings, and a sort key is already
        /// normalised, so what a reply gave back goes straight in again.
        fn folds_of(
            folds: Option<gmuse_proto::request::Folds>,
        ) -> Option<gmuse::fold::Expansion<gmuse::shelf::SortKey>> {
            folds.map(|f| {
                let key = |t: &String| gmuse::shelf::SortKey::already(t);
                gmuse::fold::Expansion::from_parts(
                    f.level,
                    f.eager,
                    f.opened.iter().map(|p| p.iter().map(key).collect()),
                    f.closed.iter().map(|p| p.iter().map(key).collect()),
                )
            })
        }

        fn anchor_of(a: gmuse_proto::request::Anchor) -> gmuse::browse::Mark {
            (
                a.path
                    .iter()
                    .map(|t| gmuse::shelf::SortKey::already(t))
                    .collect(),
                a.id,
            )
        }

        /// **The grouped shelf for this question, built once and kept** — #261.
        ///
        /// Shared by `query.tree` and `query.reveal` because they ask the same question of the
        /// library and differ only in what they do with the answer — and because two copies of
        /// this would be two caches, which is two answers to one question.
        fn shelf_for(
            &self,
            view: Option<String>,
            levels: Option<Vec<String>>,
        ) -> Result<(std::sync::Arc<gmuse::wiring::kept::Grouped>, u64), gmuse_proto::RpcError>
        {
            use gmuse_proto::RpcError;

            let raw = view.clone();
            // **A view, not a filter** — #261. A term may name a collection or another saved
            // view, and both are resolved from the log, which is here. A bare filter parses as
            // a view of one term, so this accepts everything the `filter` it replaced did.
            let view = match view.as_deref().map(gmuse::view::Stack::parse) {
                Some(Ok(stack)) => Some(stack),
                Some(Err(e)) => return Err(RpcError::invalid_params("view", e.to_string())),
                None => None,
            };

            // **Named fields, not a hardcoded pair** — `[artist, album]` is the tree as it has
            // always been, said out loud rather than assumed (`tree-view-design.md` §6).
            let levels = match levels {
                None => gmuse::shelf::Levels::default(),
                Some(names) if names.is_empty() => gmuse::shelf::Levels::Flat,
                Some(names) if names.len() == 2 => {
                    let mut got = Vec::new();
                    for name in &names {
                        match gmuse::query::format::Field::parse(name) {
                            Some(field) => got.push(gmuse::shelf::Level::of(field)),
                            None => {
                                return Err(RpcError::invalid_params(
                                    "levels",
                                    format!("{name:?} is not a field to group by"),
                                ));
                            }
                        }
                    }
                    gmuse::shelf::Levels::By {
                        outer: got[0],
                        inner: got[1],
                    }
                }
                Some(names) => {
                    return Err(RpcError::invalid_params(
                        "levels",
                        format!("two fields or none, not {}", names.len()),
                    ));
                }
            };

            let version = self.moved.load(std::sync::atomic::Ordering::Acquire);
            let want = gmuse::wiring::kept::Asked {
                version,
                levels,
                view: view.as_ref().map(|_| raw.clone().unwrap_or_default()),
            };
            if let Some(ready) = self.trees.get(&want) {
                return Ok((ready, version));
            }

            let answer = self.desk.current();
            // **Which rows are in it**, which is the view's job and not the tree's.
            //
            // **Resolved with the history, which the engine has and the old filter path did
            // not.** `query::found` passed `listened: None`, so `rating>=4` and
            // `days_since_played` evaluated against nothing. `answer.listened` is both the
            // `Listened` a filter reads and the `Definitions` a `saved=` term names.
            let (members, widened, problems) = match &view {
                None => {
                    use gmuse::model::rows::Listing;
                    (
                        answer.library.rows().into_iter().collect(),
                        Vec::new(),
                        Vec::new(),
                    )
                }
                Some(stack) => {
                    let env = gmuse::query::filter::Env {
                        listened: Some(&*answer.listened),
                        named: None,
                        now: gmuse::history::now(),
                    };
                    stack.members(&answer.library, &*answer.listened, env)
                }
            };
            let built = std::sync::Arc::new(gmuse::wiring::kept::Grouped {
                shelf: gmuse::shelf::Shelf::grouped(&answer.library, members, levels, true),
                problems: problems.iter().map(ToString::to_string).collect(),
                widened,
            });
            self.trees.keep(want, std::sync::Arc::clone(&built));
            Ok((built, version))
        }

        /// **The read seam, answered over the wire** — #261.
        ///
        /// Six methods for eleven questions, grouped by what a caller is asking *about*: one
        /// row, one recording, one thing in the panel, a name, a facet, a collection. They are
        /// separate calls rather than one `library.read` with a tag because a reply shape a
        /// client has to switch on is a reply shape nothing can document.
        fn reading(
            &self,
            request: gmuse_proto::request::Request,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse::model::rows::Listing;
            use gmuse_proto::RpcError;
            use gmuse_proto::request::Request;
            let answer = self.desk.current();
            let lib = &answer.library;
            let bad = |what: &'static str| RpcError::invalid_params(what, "not an id");

            Ok(match request {
                Request::QueryRow { ids } => serde_json::json!({
                    "rows": ids
                        .iter()
                        .map(|id| {
                            serde_json::json!({
                                "id": id,
                                "row": lib.listed(*id),
                                "locator": lib.playable(*id).map(|p| p.locator),
                                "recording": lib.recording_of(*id),
                            })
                        })
                        .collect::<Vec<_>>(),
                }),
                // **The pairing a caller holding recordings needs** — #261. A recording is
                // what crosses the wire; a pane draws copies.
                Request::QueryHoldings { recordings } => {
                    let want: std::collections::HashSet<
                        gmuse::model::id::Id<gmuse::model::entity::Recording>,
                    > = recordings.iter().filter_map(|r| r.parse().ok()).collect();
                    // Walked once and filtered, rather than asked per recording: the library
                    // is keyed by copy and the inverse is this walk either way.
                    let held: Vec<_> = lib
                        .rows()
                        .into_iter()
                        .filter_map(|id| Some((lib.recording_of(id)?, id)))
                        .filter(|(recording, _)| want.contains(recording))
                        .map(|(recording, id)| serde_json::json!([recording, id]))
                        .collect();
                    serde_json::json!({ "held": held })
                }
                Request::QueryRecording { recording, here } => {
                    let recording = recording.parse().map_err(|_| bad("recording"))?;
                    let here: Vec<_> = here.iter().filter_map(|d| d.parse().ok()).collect();
                    serde_json::json!({
                        "row": lib.row(recording),
                        "chosen": lib.chosen(recording, &here, &[]),
                    })
                }
                Request::QueryAbout { focus } => {
                    let focus = serde_json::from_value(focus)
                        .map_err(|e| RpcError::invalid_params("focus", e.to_string()))?;
                    // **The history comes from here, not from the client.** A claim's basis is
                    // the library's to state (`info-panel.md`), and a panel that was handed one
                    // could be handed a different one.
                    let happened = self.desk.history(focus);
                    serde_json::json!({
                        "about": lib.about(focus, &happened),
                        "label": lib.label_of(focus),
                    })
                }
                Request::QueryName { kind, name } => {
                    let kind = gmuse::model::about::Kind::of_word(&kind)
                        .ok_or_else(|| RpcError::invalid_params("kind", "not a kind"))?;
                    serde_json::json!({ "found": lib.find(kind, &name) })
                }
                Request::QueryPlaces { facet } => {
                    let facet = facet.parse().map_err(|_| bad("facet"))?;
                    serde_json::json!({ "places": lib.places(facet) })
                }
                Request::QueryListened => serde_json::json!({ "heard": &*answer.listened }),
                Request::QueryEntries { collection } => {
                    let collection = collection.parse().map_err(|_| bad("collection"))?;
                    serde_json::json!({ "entries": lib.entries_of(collection) })
                }
                Request::QueryCovers { focus } => {
                    let focus = serde_json::from_value(focus)
                        .map_err(|e| RpcError::invalid_params("focus", e.to_string()))?;
                    serde_json::json!({ "recordings": lib.covers(focus) })
                }
                other => {
                    return Err(RpcError::new(
                        gmuse_proto::error::code::METHOD_NOT_FOUND,
                        format!("{} is not a read", other.method()),
                    ));
                }
            })
        }

        /// **What sits under one line** — #261.
        fn under(
            &self,
            filter: Option<String>,
            levels: Option<Vec<String>>,
            path: Vec<String>,
            id: Option<gmuse::model::rows::CopyId>,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse::browse::Browsing;
            let (grouped, version) = self.shelf_for(filter, levels)?;
            let shelf = &grouped.shelf;
            let path: Vec<_> = path
                .iter()
                .map(|t| gmuse::shelf::SortKey::already(t))
                .collect();

            // **Found by mark, not by seat.** A seat does not name a track: a track sits at its
            // record's path and so does the record's own line, so matching on the seat alone
            // hands back the record — which is `x` on a track playing the whole album. The id
            // is the half that tells them apart, and a container has none.
            let want = (path.clone(), id);
            let everything =
                gmuse::browse::flatten(shelf, &gmuse::fold::Expansion::default(), true);
            let tracks = match everything.iter().find(|n| n.mark() == want) {
                Some(node) => shelf.tracks_under(node),
                // An empty path, or a mark nothing sits at any more, is the whole library.
                None => shelf.sorted().to_vec(),
            };
            Ok(serde_json::json!({
                "tracks": tracks,
                "openable": shelf.openable_under(&path),
                "library_version": version,
            }))
        }

        /// **Open the way to a line and say where it lands** — #261.
        #[allow(clippy::too_many_arguments)]
        fn reveal(
            &self,
            filter: Option<String>,
            levels: Option<Vec<String>>,
            folds: Option<gmuse_proto::request::Folds>,
            pattern: Option<String>,
            id: Option<gmuse::model::rows::CopyId>,
            from: Option<usize>,
            backwards: Option<bool>,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            let folds = Self::folds_of(folds).unwrap_or_default();
            let (grouped, version) = self.shelf_for(filter, levels)?;
            let shelf = &grouped.shelf;
            let want = match (pattern, id) {
                (Some(needle), None) => gmuse::browse::Target::Pattern {
                    needle,
                    backwards: backwards.unwrap_or(false),
                },
                (None, Some(id)) => gmuse::browse::Target::Id(id),
                // The parser refuses both and neither, so this is unreachable rather than
                // lenient — and saying so beats a silent default that would search for "".
                _ => unreachable!("the request parser requires exactly one target"),
            };

            // **`null` rather than an error when nothing matches.** A search that found nothing
            // is an ordinary answer, and `/` says so on the status line rather than failing.
            let found = gmuse::browse::reveal(shelf, &folds, &want, from.unwrap_or(0));
            Ok(serde_json::json!({
                "open": found.as_ref().map(|f| f.open.clone()),
                "at": found.as_ref().map(|f| f.at),
                "library_version": version,
            }))
        }

        #[allow(clippy::too_many_arguments)]
        fn tree(
            &self,
            filter: Option<String>,
            levels: Option<Vec<String>>,
            limit: Option<usize>,
            cursor: Option<usize>,
            folds: Option<gmuse_proto::request::Folds>,
            anchor: Option<gmuse_proto::request::Anchor>,
            settle: Option<Vec<String>>,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            let folds = Self::folds_of(folds);
            let anchor = anchor.map(Self::anchor_of);
            let (grouped, version) = self.shelf_for(filter, levels)?;
            let shelf = &grouped.shelf;

            // **The flatten is the cheap half and it is the half the folds decide** — 1.8ms
            // against the shelf's 370ms, so it runs every time and the cache never has to know
            // which nodes are open. No folds means everything open, which is what a client
            // that keeps no fold state wants.
            let all = match &folds {
                Some(folds) => gmuse::browse::flatten(shelf, folds, false),
                None => gmuse::browse::flatten(shelf, &gmuse::fold::Expansion::default(), true),
            };
            let spine = gmuse::browse::spine_of(&all);

            // **An anchor beats a cursor**, because a toggle moves every row below it: the
            // client says which line it was on and the page comes back around wherever that
            // line now is. Falls through to the cursor when the anchor is gone — a row whose
            // fold just closed is not on screen to be anchored to.
            // **The anchor is the line, the settle is the nearest surviving ancestor.** A
            // toggle knows exactly which line it was on; a level change may have closed it, and
            // then the cursor goes where vim puts it — on the fold that swallowed it.
            let at = anchor
                .as_ref()
                .and_then(|want| all.iter().position(|n| &n.mark() == want))
                .or_else(|| {
                    let seat: Vec<_> = settle
                        .as_ref()?
                        .iter()
                        .map(|t| gmuse::shelf::SortKey::already(t))
                        .collect();
                    Some(gmuse::browse::Browsing::settle(
                        &*shelf,
                        folds.as_ref().unwrap_or(&Default::default()),
                        &seat,
                    ))
                })
                .unwrap_or_else(|| cursor.unwrap_or(0))
                .min(all.len());
            let take = limit.unwrap_or(100).min(5_000);
            let page: Vec<gmuse::browse::Drawn> = all
                .iter()
                .skip(at)
                .take(take)
                .map(gmuse::browse::Drawn::of)
                .collect();
            let next = at + page.len();

            Ok(serde_json::json!({
                "nodes": page,
                "at": at,
                "total": all.len(),
                "next": (next < all.len()).then_some(next),
                "spine": spine.to_hex(),
                // **What resolving the view had to say** — #261, #119. A saved view that has
                // gone selects no rows, and so does a filter that is merely too narrow; a pane
                // that cannot tell them apart shows an empty library either way. The librarian
                // resolves the view, so the librarian is what knows.
                "problems": grouped.problems,
                "widened": grouped.widened,
                // **The counts a status line draws** — #261. Free here, because the shelf that
                // answered the page knows them, and a round trip for three numbers a client
                // redraws every frame would not be.
                "rows": shelf.len(),
                "artists": shelf.artist_count(),
                "albums": shelf.album_count(),
                "library_version": version,
            }))
        }

        /// **Which programme entry is sounding**, for a page anchored where playback is.
        ///
        /// `None` when nothing is playing or the engine did not answer, which pages from the
        /// head — the right answer either way: a programme nobody has started shows its
        /// beginning.
        fn playing_entry(&self) -> Option<u64> {
            let (tx, rx) = crossbeam_channel::bounded(1);
            if !self
                .control
                .ask(gmuse::engine::control::Ask::Playing { reply: tx })
            {
                return None;
            }
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .ok()
                .and_then(|now| now.entry)
        }

        fn playing(
            &self,
            format: Option<String>,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse_proto::{RpcError, error::code};
            let (tx, rx) = crossbeam_channel::bounded(1);
            if !self
                .control
                .ask(gmuse::engine::control::Ask::Playing { reply: tx })
            {
                return Err(RpcError::new(code::INTERNAL, "the player is not running"));
            }
            let Ok(now) = rx.recv_timeout(std::time::Duration::from_secs(5)) else {
                return Err(RpcError::new(
                    code::INTERNAL,
                    "the control thread did not answer",
                ));
            };

            let mut body = serde_json::json!({
                "state": match now.state {
                    gmuse::audio::State::Playing => "playing",
                    gmuse::audio::State::Paused => "paused",
                    gmuse::audio::State::Stopped => "stopped",
                },
                "recording": now.recording,
                // **The programme entry**, so a client can ask for "up next" itself rather
                // than relying on `queue.get`'s default (#257). Opaque: hand it back.
                "entry": now.entry,
                "pos_frames": now.pos_frames,
                "stream_generation": now.generation,
                "sample_rate": now.sample_rate,
                "rate": now.rate,
            });

            // **Rendered only when asked, and a bad template is reported once.**
            //
            // A status bar with a typo in its format should say so rather than print
            // a blank field forever — which is `render_with`'s rule from the old
            // socket (§13, 2026-09-01), and the reason this is an error reply rather
            // than a null field.
            if let Some(template) = format {
                let rendered = match gmuse::query::format::Template::parse(&template) {
                    Ok(t) => self.render(&now.recording, &t),
                    Err(e) => {
                        return Err(RpcError::invalid_params("format", e.to_string()));
                    }
                };
                body["rendered"] = serde_json::json!(rendered);
            }
            Ok(body)
        }

        /// The current track through a template, or an empty string when there is no
        /// track.
        ///
        /// **Empty rather than absent when nothing plays**, because the one caller is
        /// a status bar and "" is what it wants to print. A word it then has to filter
        /// out would be a second thing to get wrong.
        fn render(
            &self,
            recording: &Option<String>,
            template: &gmuse::query::format::Template,
        ) -> String {
            use gmuse::model::rows::Listing;
            let Some(id) = recording
                .as_deref()
                .and_then(|r| r.parse::<uuid::Uuid>().ok())
                .map(gmuse::model::id::Id::from_uuid)
            else {
                return String::new();
            };
            let answer = self.desk.current();
            let rows: &dyn Listing = &answer.library;
            rows.rows()
                .into_iter()
                .find(|&row| rows.recording_of(row) == Some(id))
                .and_then(|row| rows.listed(row))
                .map(|listed| template.render(&listed.listed()))
                .unwrap_or_default()
        }

        fn recording_of(
            &self,
            id: gmuse::model::rows::CopyId,
        ) -> Option<gmuse::model::id::Id<gmuse::model::entity::Recording>> {
            use gmuse::model::rows::Listing;
            let answer = self.desk.current();
            let rows: &dyn Listing = &answer.library;
            rows.recording_of(id)
        }

        /// **Facts an operation computed from the disk** — #218, §8.4.
        ///
        /// The tool door. See the `LibraryOp::Apply` arm above for why its gate is
        /// wider than `edit`'s and what that does and does not buy.
        fn apply(
            &self,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse_proto::RpcError;

            /// The same bound `edit` uses: a `:collect` over a large selection is the
            /// biggest real batch, at two facts per recording.
            const MOST: usize = 20_000;

            #[derive(serde::Deserialize)]
            struct Apply {
                facts: Vec<gmuse::facts::Fact>,
            }
            let Ok(apply) = serde_json::from_value::<Apply>(params) else {
                return Err(RpcError::invalid_params(
                    "facts",
                    "library.apply takes {facts: [...]}, in the log's own encoding",
                ));
            };
            let facts = apply.facts;
            if facts.is_empty() {
                return Err(RpcError::invalid_params("facts", "nothing to append"));
            }
            if facts.len() > MOST {
                return Err(RpcError::invalid_params(
                    "facts",
                    format!("{} facts is more than {MOST} in one call", facts.len()),
                ));
            }

            let counted = facts.len();
            let now = gmuse::history::now_ms();
            // Stamped here, by the appender's clock, for the reason `edit` gives.
            self.appender().say(move |log| {
                for fact in facts {
                    log.append_fact(now, fact);
                }
            });
            Ok(serde_json::json!({ "accepted": true, "facts": counted }))
        }

        /// **Proposed facts, each checked before any is appended** — #236, §8.4.
        ///
        /// The method's own summary is "the facts a view emits directly", and this
        /// is that literally: a view sends the facts, the engine decides. It
        /// replaces a `{recording, stars?}` shape whose error message already said
        /// "in this build" — one rating-shaped call could not carry the ten
        /// gestures the TUI has, and a method per gesture would be ten methods to
        /// keep in step with `Fact`.
        ///
        /// **The wire form is the log form**, `{"RecordingRated": {…}}`, and that
        /// coupling is worth naming: renaming a fact is a protocol change. In
        /// exchange, a fact read out of `gmuse events` and a fact written here are
        /// the same text, which is what makes the pair debuggable by eye.
        ///
        /// **`Fact::is_an_edit` is the gate.** A client may not propose what it did
        /// not observe — a measurement, an import's reading of a file, one
        /// machine's holdings — and that function explains each refusal group.
        fn edit(
            &self,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse_proto::RpcError;

            /// A batch has to be bounded, and this is generous: `:collect` over a
            /// large selection is the biggest real one, and it writes two facts per
            /// recording.
            const MOST: usize = 20_000;

            #[derive(serde::Deserialize)]
            struct Edit {
                facts: Vec<gmuse::facts::Fact>,
            }

            let Ok(edit) = serde_json::from_value::<Edit>(params) else {
                return Err(RpcError::invalid_params(
                    "facts",
                    "library.edit takes {facts: [...]}, in the log's own encoding",
                ));
            };
            if edit.facts.is_empty() {
                return Err(RpcError::invalid_params("facts", "nothing to append"));
            }
            if edit.facts.len() > MOST {
                return Err(RpcError::invalid_params(
                    "facts",
                    format!("{} facts is more than {MOST} in one call", edit.facts.len()),
                ));
            }

            // **Every one, before any of them.** A half-applied batch would leave
            // a collection somebody named with none of its members — the same
            // failure `edit.rs` refuses for a mistyped view document, where
            // nothing is applied rather than half of it.
            for (nth, fact) in edit.facts.iter().enumerate() {
                if !fact.is_an_edit() {
                    return Err(RpcError::invalid_params(
                        "facts",
                        format!("fact {nth} is not one a view may propose"),
                    )
                    .with_data(serde_json::json!({
                        "at": nth,
                        "reason": "a view proposes what it observed; this one comes from an \
                                   import, a scan, a measurement or a curated vocabulary",
                    })));
                }
            }

            let counted = edit.facts.len();
            let facts = edit.facts;
            let now = gmuse::history::now_ms();

            // **Through the desk, which is the only door.** Nothing here touches
            // the log: the closure runs on the librarian thread (I16), and the
            // event a view sees comes from what that thread publishes.
            //
            // Stamped *here*, by the appender's clock, and a proposal carries no
            // stamp at all. A stamp says which device said something and in what
            // order; a client minting its own would put a second device id in one
            // machine's log, which is the contradiction #219 avoided by taking the
            // appender away from a view.
            self.appender().say(move |log| {
                for fact in facts {
                    log.append_fact(now, fact);
                }
            });

            Ok(serde_json::json!({ "accepted": true, "facts": counted }))
        }
    }

    impl gmuse::engine::Answers for Bare {
        fn stream_generation(&self) -> u32 {
            // **A report now, not a placeholder** (#212, #230). The control
            // thread stores what the render step is counting in; before #230
            // this was hardcoded zero because there was no stream.
            self.control
                .facts()
                .stream_generation
                .load(std::sync::atomic::Ordering::Acquire)
        }
        fn now_ns(&self) -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
                .unwrap_or(0)
        }
        fn call(
            &self,
            request: gmuse_proto::Call,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            // **What each method costs, when asked** — #270. `GMUSE_TRACE=1` prints one
            // line per call to stderr, which is what "measure inside the running player"
            // means for the socket: which methods a view actually calls after a change,
            // and what each took. Off, it is one env read per call.
            let traced = std::env::var_os("GMUSE_TRACE").is_some_and(|v| v != "0");
            let name = request.method();
            let started = traced.then(std::time::Instant::now);
            let out = self.answer(request);
            if let Some(t) = started {
                eprintln!("trace: {name:<20} {:>9.1?}", t.elapsed());
            }
            out
        }
    }

    impl Bare {
        fn answer(
            &self,
            request: gmuse_proto::Call,
        ) -> Result<serde_json::Value, gmuse_proto::RpcError> {
            use gmuse::engine::Answers;
            use gmuse_proto::{Call, RpcError, error::code};
            match request {
                // Served by the connection before this is reached: the
                // handshake and the subscription calls are its own state, not
                // something the engine answers about itself.
                Call::Hello(_) | Call::Subscribe { .. } | Call::Unsubscribe { .. } => Err(
                    RpcError::new(code::INTERNAL, "handled by the connection, not here"),
                ),

                Call::TimeNow => Ok(serde_json::json!({ "engine_now_ns": self.now_ns() })),

                // **Reachable, and refusing honestly.** §8.4's write surface is
                // named and typed; what is not built is the engine's librarian
                // to route it to, so every operation answers `NotReady` with
                // which one it was. That is the difference between "this
                // protocol does not have that" and "this build cannot do it
                // yet" — a client can tell them apart, and `-32601` would have
                // said the first when the second is true.
                Call::Library { operation, params } => self.library(operation, params),

                Call::EngineStatus => Ok(serde_json::json!({
                    "protocol": gmuse_proto::PROTOCOL,
                    "methods": gmuse_proto::method::ALL,
                    "pid": std::process::id(),
                    // **Two numbers, because they answer different questions** — #233.
                    //
                    // `clients` counts who is attached; `connections` counts sockets, and a
                    // view is three of them. `:qa` asks whether stopping the engine would
                    // surprise somebody, which is the first — the second reported alone said
                    // "two strangers are here" to a view sitting on its own.
                    //
                    // **Both include the one asking.** A caller subtracts itself; reporting
                    // "others" would need the engine to know which connection asked, which it
                    // has no reason to.
                    "clients": self
                        .engine
                        .get()
                        .and_then(|weak| weak.upgrade())
                        .map(|e| e.clients())
                        .unwrap_or(0),
                    "connections": self
                        .engine
                        .get()
                        .and_then(|weak| weak.upgrade())
                        .map(|e| e.connections())
                        .unwrap_or(0),
                })),

                Call::EngineShutdown => {
                    // **Say so before going.** §7.8 puts status on the `engine`
                    // topic, and a view that learns the engine stopped by its
                    // socket closing cannot tell that from a crash. The notice
                    // is queued before the flag is set, so the writers have it
                    // in hand before the shutdown drains them.
                    if let Some(engine) = self.engine.get().and_then(|weak| weak.upgrade()) {
                        engine.publish(&gmuse_proto::Event::new(
                            gmuse_proto::Topic::Engine,
                            serde_json::json!({ "stopping": true, "reason": "engine.shutdown" }),
                        ));
                    }
                    self.stopping
                        .store(true, std::sync::atomic::Ordering::Release);
                    Ok(serde_json::json!({ "stopping": true }))
                }

                // **The same parser the `:` line uses**, which is the whole
                // point of §7.12: two textual command languages is the standing
                // complaint against `--repl`, and routing here is what stops a
                // second one being written.
                Call::PlaybackGet { format } => self.playing(format),
                // **Is this filter valid** — §7.11. One parser, and this is how a client
                // reaches it without having one.
                Call::QueryParse { text } => match gmuse::query::filter::Expr::parse(&text) {
                    Ok(_) => Ok(serde_json::json!({ "ok": true })),
                    // **The message and no span**, because the parser does not track
                    // positions. §7.11 asks for one; adding it is a change to the parser
                    // rather than to this, and the message is what the `:` line shows today.
                    Err(e) => Err(RpcError::invalid_params("filter", e.to_string())),
                },

                // **Run a filter and hand back a page** — §7.11, #261.
                //
                // On *this* thread, which is the connection's own, from one atomic load of the
                // published fold. §7.11 says a query "runs on a query worker"; a connection
                // worker is one, and the effect is that a query queues behind neither the
                // appender nor another query.
                //
                // **Nothing here is a copy of the library.** `Desk::current()` clones an `Arc`;
                // what crosses the wire is the page.
                Call::QueryRun {
                    filter,
                    limit,
                    cursor,
                } => self.found(filter, limit, cursor),

                // **The browse tree, grouped by the librarian** — §7.11, #261.
                Call::QueryTree {
                    view,
                    levels,
                    limit,
                    cursor,
                    folds,
                    anchor,
                    settle,
                } => self.tree(view, levels, limit, cursor, folds, anchor, settle),

                // **Where a search lands, and what had to open for it to** — #261.
                Call::QueryReveal {
                    view,
                    levels,
                    folds,
                    pattern,
                    id,
                    from,
                    backwards,
                } => self.reveal(view, levels, folds, pattern, id, from, backwards),

                // **What `a`, `e`, `x` and `zR` act on** — #261.
                Call::QueryUnder {
                    view,
                    levels,
                    path,
                    id,
                } => self.under(view, levels, path, id),

                // **The read seam** — #261. One arm, because they differ only in what they ask
                // about and every one of them is answered off the same published fold.
                read @ (Call::QueryRow { .. }
                | Call::QueryHoldings { .. }
                | Call::QueryRecording { .. }
                | Call::QueryAbout { .. }
                | Call::QueryName { .. }
                | Call::QueryPlaces { .. }
                | Call::QueryEntries { .. }
                | Call::QueryCovers { .. }
                | Call::QueryListened) => self.reading(read),

                // **A page, anchored where playback is** — #257.
                //
                // The anchor comes from the engine and the page from the librarian, which is
                // the triangle doing exactly what it is for: *where am I* is a playback
                // question and *what is the order* is a library one. Neither actor can answer
                // both, and this is the one place that may ask each.
                Call::QueueGet { from, limit } => {
                    let from = from.or_else(|| self.playing_entry());
                    self.programme(gmuse::librarian::programme::Ask::Look {
                        from,
                        // **A screenful, not a library.** Large enough that a view rarely
                        // pages, small enough that the answer is never 7,271 rows.
                        limit: limit.unwrap_or(100).min(1_000),
                    })
                }
                Call::QueueEnqueue {
                    recordings,
                    base_version,
                    at,
                    after,
                } => {
                    // **Recordings on the wire, rows underneath** — I17: only
                    // user-facing grains cross, and a recording is what a person
                    // chooses. The librarian turns one into the row that draws
                    // it, which is the same resolution `player.load` does.
                    let mut ids = Vec::with_capacity(recordings.len());
                    for raw in &recordings {
                        let Ok(uuid) = raw.parse::<uuid::Uuid>() else {
                            return Err(RpcError::invalid_params(
                                "recordings",
                                format!("{raw:?} is not a recording id"),
                            ));
                        };
                        match self.row_of(gmuse::model::id::Id::from_uuid(uuid)) {
                            Some(id) => ids.push(id),
                            None => {
                                return Err(RpcError::new(
                                    code::NOT_FOUND,
                                    format!("the library has no row for {raw}"),
                                )
                                .with_data(serde_json::json!({ "ref": raw })));
                            }
                        }
                    }
                    // **Relative to what is sounding**, which only this side knows — the
                    // same anchor `queue.get` pages from. This was a push to the end,
                    // "because a programme is one list" (#257) — and under one list the
                    // end is after seven thousand rows. What `e` had always meant was
                    // "after what I queued, before the pool resumes", and the programme
                    // can say which entries those are now.
                    let at = match at {
                        gmuse_proto::request::Placement::Next => {
                            gmuse::librarian::programme::Placement::Next
                        }
                        gmuse_proto::request::Placement::Queued => {
                            gmuse::librarian::programme::Placement::Queued
                        }
                        gmuse_proto::request::Placement::End => {
                            gmuse::librarian::programme::Placement::End
                        }
                    };
                    self.programme(gmuse::librarian::programme::Ask::Enqueue {
                        rows: ids,
                        // A caller that named an entry places relative to it — `p` at the
                        // cursor; one that did not places relative to what is sounding.
                        anchor: after.or_else(|| self.playing_entry()),
                        at,
                        base: base_version,
                    })
                }
                // **The whole list, for a view that holds a mirror** — #233.
                // Resolved the same way `enqueue` resolves, because a recording
                // is the grain a person chooses (I17) and the row that draws it
                // is the librarian's business.
                Call::QueueSet {
                    recordings,
                    base_version,
                } => {
                    let mut ids = Vec::with_capacity(recordings.len());
                    for raw in &recordings {
                        let Ok(uuid) = raw.parse::<uuid::Uuid>() else {
                            return Err(RpcError::invalid_params(
                                "recordings",
                                format!("{raw:?} is not a recording id"),
                            ));
                        };
                        match self.row_of(gmuse::model::id::Id::from_uuid(uuid)) {
                            Some(id) => ids.push(id),
                            None => {
                                return Err(RpcError::new(
                                    code::NOT_FOUND,
                                    format!("the library has no row for {raw}"),
                                )
                                .with_data(serde_json::json!({ "ref": raw })));
                            }
                        }
                    }
                    self.programme(gmuse::librarian::programme::Ask::Play {
                        rows: ids,
                        window: 3,
                        base: Some(base_version),
                    })
                }
                Call::QueueRemove {
                    which,
                    base_version,
                } => match which {
                    gmuse_proto::request::Removal::At(index) => {
                        self.programme(gmuse::librarian::programme::Ask::RemoveAt {
                            index,
                            base: base_version,
                        })
                    }
                    gmuse_proto::request::Removal::Tokens(tokens) => {
                        self.programme(gmuse::librarian::programme::Ask::Remove { tokens })
                    }
                },
                Call::QueueClear {
                    base_version,
                    queued_only,
                } => self.programme(gmuse::librarian::programme::Ask::Clear {
                    base: base_version,
                    queued_only,
                }),
                Call::SetScope { mode } => {
                    let Some(mode) = gmuse::next::Scope::parse(&mode) else {
                        return Err(RpcError::invalid_params(
                            "mode",
                            format!("{mode:?} is not an aaa mode"),
                        ));
                    };
                    // **A scope change rebuilds the programme**, anchored at what is
                    // sounding — which only the engine's window knows, so the engine does
                    // it. Routed as an instruction rather than an ask for that reason.
                    if !self.control.ask(gmuse::engine::control::Ask::Do(
                        gmuse_engine::instruction::Instruction::Scope(
                            gmuse_engine::instruction::ScopeTo::Set(mode),
                        ),
                    )) {
                        return Err(RpcError::new(code::INTERNAL, "the player is not running"));
                    }
                    Ok(serde_json::json!({ "accepted": true }))
                }

                // **Three verbs, resolved where the state lives** — #225, D-8.
                //
                // The text language cannot carry these: its `play` starts the selection and
                // needs a cursor, its `pause` is the cmus toggle, and `toggle` names a
                // setting. #224's O-6 settled two enums rather than three, and this is the
                // wire's half.
                Call::PlayerTransport { verb } => {
                    use gmuse_proto::request::Transport;
                    let what = match verb {
                        Transport::Play => gmuse_engine::instruction::Instruction::Play,
                        Transport::Pause => gmuse_engine::instruction::Instruction::Pause,
                        Transport::Toggle => gmuse_engine::instruction::Instruction::Toggle,
                        Transport::Cue => gmuse_engine::instruction::Instruction::Cue,
                        Transport::CuePreview => gmuse_engine::instruction::Instruction::CuePreview,
                    };
                    if !self.control.ask(gmuse::engine::control::Ask::Do(what)) {
                        return Err(RpcError::new(code::INTERNAL, "the player is not running"));
                    }
                    Ok(serde_json::json!({ "accepted": true }))
                }

                // **Seconds in, frames at the far end** — #265. The view has no
                // device rate, so the conversion belongs where the rate is known;
                // what crosses is the number the person asked for.
                Call::PlayerSeek { to } => {
                    use gmuse_proto::request::Seek;
                    let what = gmuse_engine::instruction::Instruction::Seek(match to {
                        Seek::Absolute(secs) => gmuse::audio::SeekTo::Absolute(
                            std::time::Duration::from_secs_f64(secs.max(0.0)),
                        ),
                        Seek::Relative(secs) => gmuse::audio::SeekTo::Relative(secs),
                    });
                    if !self.control.ask(gmuse::engine::control::Ask::Do(what)) {
                        return Err(RpcError::new(code::INTERNAL, "the player is not running"));
                    }
                    Ok(serde_json::json!({ "accepted": true }))
                }

                Call::PlayerLoad { recording, view } => {
                    let Ok(uuid) = recording.parse::<uuid::Uuid>() else {
                        return Err(RpcError::invalid_params(
                            "recording",
                            format!("{recording:?} is not a recording id"),
                        ));
                    };
                    let recording = gmuse::model::id::Id::from_uuid(uuid);
                    // **The view, resolved here and handed down as rows.** The programme
                    // is built by the librarian from what it is given, and what it was
                    // given was the whole library — so a shuffle wandered out of the
                    // view a person pressed play in. This is the one place that holds
                    // both the parser and the fold, which is why the engine gets a set
                    // and never the text (`layers.md`).
                    let within = match view.as_deref().map(gmuse::view::Stack::parse) {
                        None => None,
                        Some(Err(e)) => {
                            return Err(RpcError::invalid_params("view", e.to_string()));
                        }
                        Some(Ok(stack)) => {
                            let answer = self.desk.current();
                            let env = gmuse::query::filter::Env {
                                listened: Some(&*answer.listened),
                                named: None,
                                now: gmuse::history::now(),
                            };
                            let (members, _, _) =
                                stack.members(&answer.library, &*answer.listened, env);
                            Some(std::sync::Arc::new(members))
                        }
                    };
                    self.control
                        .ask(gmuse::engine::control::Ask::Load { recording, within });
                    // **Accepted, not played** — §7.4's acknowledgement
                    // semantics. Whether it sounded arrives as a `playback`
                    // event, and a load of something this device does not hold
                    // produces no event, which is the honest report.
                    Ok(serde_json::json!({ "accepted": true }))
                }

                Call::ExecLine { line } => match gmuse::cmd::parse::command(&line) {
                    Ok(cmd) => {
                        // **The translation is the gate** — #213. `instruction_of`
                        // either produces something the control thread can carry
                        // out or says why it cannot, so there is no second list to
                        // agree with and the refusal carries the reason rather than
                        // a category.
                        let parsed = gmuse::cmd::parse::format(&cmd);
                        match gmuse::cmd::instruction::instruction_of(&cmd) {
                            Ok(instruction) => {
                                self.control
                                    .ask(gmuse::engine::control::Ask::Do(instruction));
                                Ok(serde_json::json!({ "accepted": true, "parsed": parsed }))
                            }
                            Err(why) => Err(RpcError::new(code::NOT_READY, why).with_data(
                                serde_json::json!({
                                    "reason": why,
                                    "parsed": parsed,
                                }),
                            )),
                        }
                    }
                    // **`span` is null and that is honest.** §7.13 shapes this
                    // as `{message, span}`; today's parser reports what was
                    // wrong and not where, so inventing an offset would be
                    // worse than admitting there is none.
                    Err(e) => Err(RpcError::new(code::COMMAND_PARSE, e.to_string())
                        .with_data(serde_json::json!({ "message": e.to_string(), "span": null }))),
                },
            }
        }
    }

    // **The librarian, before the socket.** §4.4 and I15: it owns the log and
    // the projection and is the process's single appender. `Desk::open` folds
    // once synchronously, so no client that connects can ever see an empty
    // library — which is why it happens here rather than on the first request.
    let mut loaded = gmuse::config::load(&gmuse::config::default_path());
    // **`GMUSE_DATA_DIR` points the engine at another library.**
    //
    // Without it the engine can only ever be run against the real one, which
    // makes it untestable and makes a first attempt at an integration test a
    // write into somebody's actual log. An env var rather than a flag because
    // the thing that needs it is a test harness spawning the binary, and it
    // matches how `GMUSE_SOCKET` already moves the socket for the same reason.
    if let Some(dir) = std::env::var_os("GMUSE_DATA_DIR") {
        loaded.data_dir = std::path::PathBuf::from(dir);
    }
    // **And `GMUSE_ROOTS` decides what it reconciles**, empty meaning nothing.
    //
    // Needed for the same reason and separately: the desk processes jobs in
    // order, so the scan queued at startup delays every write behind it. On
    // this library that is thirty to seventy seconds during which a rating
    // sent over the socket is accepted and does not land — which is what a
    // test pointed at a fresh data directory but the real roots looks like,
    // and it took a while to see because the acknowledgement is honest: the
    // operation *was* accepted, it is simply queued.
    if let Some(roots) = std::env::var_os("GMUSE_ROOTS") {
        let roots = roots.to_string_lossy().to_string();
        loaded.library = if roots.trim().is_empty() {
            Vec::new()
        } else {
            roots.split(':').map(std::path::PathBuf::from).collect()
        };
    }
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let now_ms = gmuse::history::now_ms();
    let roots = loaded.library.clone();
    // **From the snapshot, holding no records** — #269. The last engine left a
    // projection that says where it got to; this one resumes there and reads the
    // log files back only for what the projection does not hold. An 87 MB parse
    // at every start, and 83.5 MB held for the life of the process, both go.
    let desk = Arc::new(gmuse::librarian::Desk::resume(
        Default::default(),
        device,
        now_ms,
        loaded.data_dir.clone(),
    ));

    let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // **`Weak`, or nothing ever shuts down** — #245.
    //
    // Everything that holds this only wants to `publish`, and holding a strong
    // reference made a cycle: the listener thread owns `Arc<Bare>`, `Bare` owns this
    // slot, and the slot owned the `Engine` whose `Drop` is the only thing that stops
    // the listener. So the count could not reach zero, `Drop` never ran, and the
    // graceful shutdown it implements — set the flag, wake `accept`, join, unlink the
    // socket — was unreachable code. Measured at 3 strong references where 1 was
    // needed.
    //
    // `upgrade()` returning `None` is the honest answer to "publish this": the engine
    // is going, so there is nobody to publish to.
    let engine_slot: Arc<std::sync::OnceLock<std::sync::Weak<gmuse::engine::Engine>>> =
        Arc::default();

    // **Claim the engine before opening a device** — #219. The lock is what
    // decides whether this process is *the* engine, and it used to be taken
    // inside `serve`, after the player was already open: a second engine
    // claimed the sound card, printed the rate it had opened it at, and only
    // then found out it was not allowed to be an engine.
    let lock = gmuse::engine::claim(&path).unwrap_or_else(|e| die(e));

    // **The player, on its own thread, before the socket** — #230. It blocks
    // until the device is open so that `hello` never reports a placeholder
    // rate: a client connecting during startup would otherwise be told 0 Hz
    // and have no way to learn better.
    // **The engine reaches the librarian through channels, not a `Desk`** — #257. The relay
    // owns this side of them; the engine cannot name a librarian at all.
    mark("Desk::resume (the snapshot)");
    let relay = gmuse::wiring::relay::Relay::start(Arc::clone(&desk));
    let control = Arc::new(
        gmuse::engine::control::Control::start(
            null_audio,
            cfg,
            relay.librarian.clone(),
            roots.clone(),
            Arc::clone(&engine_slot),
        )
        .unwrap_or_else(|e| die(e)),
    );
    eprintln!(
        "gmuse engine: {} at {} Hz",
        if null_audio {
            "null renderer"
        } else {
            "audio device"
        },
        control
            .facts()
            .sample_rate
            .load(std::sync::atomic::Ordering::Acquire)
    );
    // **The rate is a report now**, from the stream the control thread opened
    // (#230). It was hardcoded 48 kHz with a comment admitting it, because the
    // handshake promises the rate frames are counted in and there was no stream
    // to ask.
    let identity = gmuse::engine::Identity::new(
        control
            .facts()
            .sample_rate
            .load(std::sync::atomic::Ordering::Acquire),
        Vec::new(),
    );
    let engine = gmuse::engine::serve_with(
        &path,
        identity,
        Arc::new(Bare {
            stopping: Arc::clone(&stopping),
            engine: Arc::clone(&engine_slot),
            desk: Arc::clone(&desk),
            control: Arc::clone(&control),
            roots: roots.clone(),
            moved: Arc::clone(&relay.moved),
            // Four: a picker alternates between a couple of shapes.
            trees: gmuse::wiring::kept::Trees::new(4),
        }),
        lock,
    )
    .unwrap_or_else(|e| die(e));
    let engine = Arc::new(engine);
    let _ = engine_slot.set(Arc::downgrade(&engine));

    // **What the librarian publishes becomes what the views are told.** The
    // announcer starts from the answer standing now, not from the first fold:
    // the startup fold is not news, and announcing it would tell every view to
    // re-query the moment it connected.
    // **One watcher, not two.** The announcer already compares the published answer to notice
    // a change; it bumps the counter the engine reads, so nothing else has to watch (#257).
    mark("the player is up");
    let _announcer = gmuse::wiring::announcer::Announcer::start(
        Arc::clone(&desk),
        Arc::downgrade(&engine),
        Arc::clone(&relay.moved),
    );

    // **The scan at engine start** — §8.9, and the 2026-09-10 decision that a
    // stale cache is the worse failure. Not waited on: the library is already
    // usable from the log, and this closes drift rather than supplying rows.
    // Its completion arrives as a `library` event like any other change.
    if !roots.is_empty() {
        desk.appender()
            .expect("the engine's desk appends")
            .scan(roots.clone(), None);
    }
    mark("listening");
    eprintln!("gmuse engine: listening on {}", engine.path().display());

    // **Say what is held, when asked for at build time** — #255.
    //
    // Every few seconds rather than once, because the interesting number is whether it
    // settles: a fold that churns and a fold that grows look the same in a single reading.
    #[cfg(feature = "measure-memory")]
    std::thread::Builder::new()
        .name("gmuse-memory".into())
        .spawn(|| {
            use gmuse_library::accounting::{live, mb, peak};
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                eprintln!(
                    "memory: {:.1} MB live, {:.1} MB peak",
                    mb(live()),
                    mb(peak())
                );
            }
        })
        .expect("the memory reporter starts");

    // Nothing to do on this thread but wait to be told to stop; the listener
    // owns the work. Polling rather than a condvar because the only waker is a
    // flag set from a connection thread and a second of latency on shutdown
    // costs nothing.
    while !stopping.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    eprintln!("gmuse engine: stopping");
    // Let the `stopping` notice reach the views that asked for it before the
    // process goes. Bounded: a view that stopped reading does not get to hold
    // the shutdown open.
    engine.drain(std::time::Duration::from_secs(2));
    drop(engine);
    Ok(())
}

/// Connect to a running engine, or say plainly that there is none.
fn connect() -> Result<gmuse_client::Client, String> {
    let path = gmuse::engine::socket::default_path();
    gmuse_client::Client::connect(&path, "gmuse-cli", env!("CARGO_PKG_VERSION")).map_err(|e| {
        match e {
            // **One is running and will not talk**, so "start one" is the wrong advice: it
            // would lose the lock to the wedged one and exit. The error already says what
            // to do.
            gmuse_client::ClientError::Unresponsive { .. } => e.to_string(),
            // **Not a stack trace and not a bare io error.** The overwhelmingly
            // likely cause is that no engine is running, and saying so with the
            // command that starts one is the whole difference between a usable CLI
            // and one you have to already understand.
            _ => format!("{e}\n\nStart one with: gmuse engine --foreground"),
        }
    })
}

/// Print a message and stop, the way a command-line tool does.
///
/// **Not `Err` back to `main`.** `main` returns `Box<dyn Error>`, which Rust
/// prints with `Debug` — so a refusal reached a terminal as
/// `Refused(RpcError { code: 1005, message: "unknown command \"nonsuch\"", … })`
/// when what the person needed was `unknown command "nonsuch" (1005)`. Found by
/// running it, which is the point of this phase.
fn die(message: impl std::fmt::Display) -> ! {
    eprintln!("gmuse: {message}");
    std::process::exit(1)
}

/// `gmuse ctl <command…>` — one command, printed, exit — §4.2.
fn run_ctl(words: &[String], json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if words.is_empty() {
        die("gmuse ctl needs a command, for example: gmuse ctl pause");
    }
    let client = connect().unwrap_or_else(|e| die(e));

    // **Either typed arguments or passed through** — §4.2, and the first
    // version only did the second half. `gmuse ctl engine.shutdown` went to the
    // `:` line's parser, which has never heard of it, and came back
    // `unknown command "engine.shutdown"` — so the protocol's own methods were
    // the one thing the CLI could not reach. Found by driving it from a shell,
    // which is what §15 says this phase is for.
    //
    // A method name is distinguishable from a command word by being one: the
    // `:` language has no dots in its verbs and every method does. No guessing
    // and no new syntax — the params are JSON, because inventing a shell-ish
    // key=value dialect here would be a second parameter language to keep in
    // step with the first.
    let outcome = if gmuse_proto::request::method::ALL.contains(&words[0].as_str()) {
        let params = words.get(1).map(|raw| {
            serde_json::from_str(raw).unwrap_or_else(|e| {
                die(format!(
                    "{raw:?} is not JSON: {e}\n\nUsage: gmuse ctl {} '{{…}}'",
                    words[0]
                ))
            })
        });
        client.call(&words[0], params)
    } else {
        // The `:` line's language, parsed engine-side by the same parser the
        // TUI uses (§7.12) — so the CLI has no command vocabulary of its own.
        client.call(
            gmuse_proto::request::method::EXEC_LINE,
            Some(serde_json::json!({ "line": words.join(" ") })),
        )
    };

    match outcome {
        Ok(result) => {
            if json {
                println!("{}", serde_json::to_string(&result)?);
            } else if !result.is_null() {
                println!("{}", render(&result));
            }
            Ok(())
        }
        Err(e) => {
            // **A refusal is still an exit status.** A pipeline that only read
            // stdout would otherwise treat "the engine said no" as success.
            if json && let Some(rpc) = e.rpc() {
                println!("{}", serde_json::to_string(rpc)?);
                std::process::exit(1);
            }
            die(e)
        }
    }
}

/// Render a result for a person rather than for jq.
fn render(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| format!("{k}: {}", render(v)))
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Array(items) => items.iter().map(render).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

/// `gmuse events [--topics …]` — subscribe and print NDJSON — §13.3.
fn run_events(topics: &[String], count: Option<usize>) -> Result<(), Box<dyn std::error::Error>> {
    let wanted: Vec<gmuse_proto::Topic> = if topics.is_empty() {
        gmuse_proto::Topic::ALL.to_vec()
    } else {
        topics
            .iter()
            .map(|name| {
                serde_json::from_value(serde_json::Value::String(name.clone())).map_err(|_| {
                    format!(
                        "{name:?} is not a topic — try one of: {}",
                        gmuse_proto::Topic::ALL
                            .iter()
                            .map(|t| t.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
            })
            .collect::<Result<_, _>>()
            .unwrap_or_else(|e: String| die(e))
    };

    let client = connect().unwrap_or_else(|e| die(e));
    let in_force = client.subscribe(&wanted).unwrap_or_else(|e| die(e));
    eprintln!(
        "subscribed: {}",
        in_force
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // **NDJSON on stdout, the subscription note on stderr** (§13.3). A jq or
    // fzf pipeline reads stdout, and a line of prose in it would be a parse
    // error in somebody's script.
    for (seen, event) in client.events().iter().enumerate() {
        println!("{}", serde_json::to_string(&event)?);
        use std::io::Write as _;
        std::io::stdout().flush()?;
        if count.is_some_and(|n| seen + 1 >= n) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A read never scans** — `access-layers.md` §3a calls this the absolute
    /// rule, and it was being broken by four subcommands.
    ///
    /// A scan is a *discovery*: it walks the disk, reads tags and mints facts.
    /// A read that does one answers with what a tagger last wrote instead of
    /// what the library recorded — `shelf tree` showed `Analogue Bubblebath 4`
    /// over the library's `Analogue Bubblebath EP4`, and counted a file on
    /// disk that had never been imported as though it were held.
    ///
    /// Asserted against the source because there is no type that could carry
    /// it: `populate` is an ordinary function and any arm can call one.
    /// `verify` is the exception and says so — reading the bytes back is the
    /// whole of what it does.
    #[test]
    fn no_read_subcommand_walks_the_disk() {
        let src = include_str!("main.rs");
        let body = match src.find("#[cfg(test)]") {
            Some(at) => &src[..at],
            None => src,
        };

        let mut offender = None;
        let mut arm = "";
        for line in body.lines() {
            if let Some(at) = line.find("gmuse::cli::Shelf::") {
                arm = line[at + "gmuse::cli::Shelf::".len()..]
                    .split(|c: char| !c.is_alphanumeric())
                    .next()
                    .unwrap_or("");
            }
            const READS: &[&str] = &[
                "Query",
                "Render",
                "Duplicates",
                "Copies",
                "Slots",
                "Artists",
                "Names",
                "Tree",
            ];
            if READS.contains(&arm) && line.contains("populate::populate") {
                offender = Some(arm);
            }
        }

        assert_eq!(
            offender, None,
            "a read subcommand walks the disk, so it answers with tags over the library"
        );
    }

    /// A tiny real WAV, so there is audio to hash.
    fn wav(path: &std::path::Path, frames: usize) {
        use std::io::Write;
        let bytes = 4 * frames;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&((36 + bytes) as u32).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&2u16.to_le_bytes()).unwrap();
        f.write_all(&44100u32.to_le_bytes()).unwrap();
        f.write_all(&176_400u32.to_le_bytes()).unwrap();
        f.write_all(&4u16.to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&(bytes as u32).to_le_bytes()).unwrap();
        f.write_all(&vec![0u8; bytes]).unwrap();
    }

    /// **The claim the whole library rests on, checked** — `shelf verify`.
    ///
    /// Identity is the audio, so one statement per copy carries everything:
    /// these bytes produce this id. A file replaced in place leaves the log
    /// describing audio that is not there, and every id derived from it names
    /// the wrong thing — silently, because nothing ever read the bytes again.
    #[test]
    fn verify_notices_bytes_that_were_replaced_in_place() {
        use gmuse::model::entity::Encoding;
        use gmuse::model::id::Id;

        let dir = std::env::temp_dir().join("gmuse-verify");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join("a.wav");
        wav(&at, 1000);

        let said =
            Id::<Encoding>::of_audio(&gmuse::model::ingest::read(&at).expect("readable").audio);
        let path = at.display().to_string();
        assert_eq!(still_holds(&path, said), Holds::Agrees);

        // The same path, different audio — a re-encode written over its
        // original, or a copy that came back wrong off a backup.
        wav(&at, 2000);
        let Holds::Changed(now) = still_holds(&path, said) else {
            panic!("a replaced file passed as unchanged")
        };
        assert_ne!(now, said);

        std::fs::remove_file(&at).unwrap();
        assert_eq!(still_holds(&path, said), Holds::Gone);
    }

    /// **Gone is not changed.** A holding whose file has been moved away says
    /// nothing about the bytes, and reporting it as a replacement would send
    /// somebody looking for a corruption that is not there.
    #[test]
    fn a_holding_with_no_file_is_its_own_answer() {
        use gmuse::model::entity::Encoding;
        use gmuse::model::id::Id;
        assert_eq!(
            still_holds("/nowhere/at/all.mp3", Id::<Encoding>::derived(b"x")),
            Holds::Gone
        );
    }

    /// **Two files of one recording is a shape the model supports and nothing
    /// could list** — `shelf copies`.
    ///
    /// `query` hands out one locator per recording, so the second copy is on
    /// no work list; `audit duplicates` asks which *recordings* are the same
    /// music and only ever sees that one locator; `verify` checks each file
    /// against its own hash and says nothing about siblings. Measured here:
    /// **216 recordings held twice**, 67 of them a FLAC beside its MP3.
    #[test]
    fn copies_of_one_recording_are_grouped_and_the_rest_are_not() {
        use gmuse::model::entity::{Encoding, Recording};
        use gmuse::model::id::Id;

        let song = Id::<Recording>::derived(b"closing in");
        let alone = Id::<Recording>::derived(b"a song held once");
        let flac = Id::<Encoding>::derived(b"the flac");
        let mp3 = Id::<Encoding>::derived(b"the mp3");
        let only = Id::<Encoding>::derived(b"the only copy");

        let bound: std::collections::BTreeMap<_, _> = [(flac, song), (mp3, song), (only, alone)]
            .into_iter()
            .collect();
        let held = [
            (flac, "/m/a.flac".to_string()),
            (mp3, "/m/a.mp3".to_string()),
            (only, "/m/b.mp3".to_string()),
        ];

        let mut by_recording: std::collections::BTreeMap<_, Vec<_>> = Default::default();
        for (encoding, path) in &held {
            if let Some(recording) = bound.get(encoding) {
                by_recording
                    .entry(*recording)
                    .or_insert_with(Vec::new)
                    .push(path);
            }
        }
        let doubled: Vec<_> = by_recording
            .iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(r, v)| (*r, v.len()))
            .collect();
        assert_eq!(
            doubled,
            vec![(song, 2)],
            "a recording held once was reported, or one held twice was not"
        );
    }

    /// **A file does not block its own retirement**, and thirty-two of them did.
    ///
    /// The guard asks whether these bytes survive this retirement. The files
    /// being retired are not an answer to that question — each is held at its
    /// own path — so comparing against every holding outside `.departed` made
    /// every file report itself as the reason it could not leave. Retiring one
    /// of two Luna copies reported `0 to retire`, naming each file's own path
    /// as the copy that outlived it.
    #[test]
    fn a_copy_being_retired_is_not_a_copy_that_survives_it() {
        use gmuse::model::entity::{Device, Encoding};
        use gmuse::model::id::Id;
        use gmuse::projection::catalogue::{Asserted, Catalogue};

        let going_path = "/m/Bodzin & Romboy/Luna/01 Triton.mp3";
        let bytes = Id::<Encoding>::derived(b"triton");
        let mut cat = Catalogue::default();
        cat.holdings.insert(
            Id::<Device>::derived(b"laptop"),
            Asserted {
                at: gmuse::model::clock::Stamp {
                    wall: 1,
                    counter: 0,
                    device: Id::<Device>::derived(b"laptop"),
                },
                held: [(bytes, going_path.to_string())].into_iter().collect(),
            },
        );
        let going = [going_path.to_string()].into_iter().collect();
        assert!(
            !surviving_holders(&cat, std::path::Path::new("/m/.departed"), &going)
                .contains_key(&bytes),
            "the file named itself as the reason it could not go"
        );
    }

    /// **And the case the guard exists for still fires.** `Early James` was
    /// retired by moving one of two folders holding identical rips: ten of its
    /// twelve files were the same encoding as a file still in the library, so
    /// retiring the departed copy retired the live one too — 6,648 settled to
    /// 6,638.
    ///
    /// A copy that is *not* going still holds those bytes, and still says so.
    #[test]
    fn a_live_copy_of_the_same_bytes_still_refuses_the_retirement() {
        use gmuse::model::entity::{Device, Encoding};
        use gmuse::model::id::Id;
        use gmuse::projection::catalogue::{Asserted, Catalogue};

        let going_path = "/m/Early James/copy two/01 Blue Pill.mp3";
        let staying = "/m/Early James/Strange Time/01 Blue Pill.mp3";
        let bytes = Id::<Encoding>::derived(b"one rip, two folders");
        let mut cat = Catalogue::default();
        cat.holdings.insert(
            Id::<Device>::derived(b"laptop"),
            Asserted {
                at: gmuse::model::clock::Stamp {
                    wall: 1,
                    counter: 0,
                    device: Id::<Device>::derived(b"laptop"),
                },
                held: [(bytes, staying.to_string())].into_iter().collect(),
            },
        );
        let going = [going_path.to_string()].into_iter().collect();
        assert_eq!(
            surviving_holders(&cat, std::path::Path::new("/m/.departed"), &going).get(&bytes),
            Some(&vec![staying]),
            "retiring one folder would have retired the live copy too"
        );
    }

    /// **A file already under `.departed` is not a survivor either**, which is
    /// what lets `remove` tombstone an album somebody retired by hand before
    /// this command existed.
    #[test]
    fn bytes_that_are_only_in_departed_do_not_hold_anything_back() {
        use gmuse::model::entity::{Device, Encoding};
        use gmuse::model::id::Id;
        use gmuse::projection::catalogue::{Asserted, Catalogue};

        let bytes = Id::<Encoding>::derived(b"moved by hand");
        let mut cat = Catalogue::default();
        cat.holdings.insert(
            Id::<Device>::derived(b"laptop"),
            Asserted {
                at: gmuse::model::clock::Stamp {
                    wall: 1,
                    counter: 0,
                    device: Id::<Device>::derived(b"laptop"),
                },
                held: [(bytes, "/m/.departed/old/01 Gone.mp3".to_string())]
                    .into_iter()
                    .collect(),
            },
        );
        assert!(
            surviving_holders(
                &cat,
                std::path::Path::new("/m/.departed"),
                &Default::default()
            )
            .is_empty()
        );
    }

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
