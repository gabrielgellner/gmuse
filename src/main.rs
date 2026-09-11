//! M2 — the audio API.
//!
//! `PlayerHandle` (in `audio`) is the whole interface: load, play, pause,
//! stop, seek, volume, position, and an event stream. This binary drives it
//! from stdin lines, standing in for the UI until M4.
//!
//! Verification target from DESIGN §9: seek is sample-accurate and the
//! position display does not jump on seek.

mod apply;
mod audio;
mod audio_id;
mod audit;
mod bridge;
mod cache;
mod cli;
mod cmd;
mod config;
mod datadir;
mod edit;
mod events;
mod filter;
#[cfg(test)]
mod fixture;
mod format;
mod guides;
mod history;
mod lock;
mod m3u;
mod model;
mod next;
mod playlist;
mod query;
mod remote;
mod scan;
mod session;
mod shelf;
mod smart;
mod sort;
mod storage;
mod text;
mod ui;
mod undo;
mod view;

use std::path::PathBuf;

use clap::Parser;

use audio::PlayerHandle;
use cli::Cli;
use scan::Holdings;
use shelf::Shelf;

/// Everything the prompt operates on.
struct App {
    player: PlayerHandle,
    store: Holdings,
    roots: Vec<PathBuf>,
    cache_path: Option<PathBuf>,
    /// The view in force, as its source line.
    view: Option<String>,
    /// Files recognised at a new path by a scan, when one ran.
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
    if let Some(cli::Command::Shelf(op)) = &args.command {
        let loaded = if args.no_config {
            // **`--no-config` still keeps the log where the log lives.**
            // `Loaded::default()` leaves `data_dir` empty, and an empty path
            // reads as nothing (`read_dir("")` fails) while writing perfectly
            // well relative to the working directory — so a library operation
            // silently started a fresh log in whatever folder it was run from
            // and wrote it there.
            config::Loaded {
                data_dir: config::default_data_dir(),
                ..config::Loaded::default()
            }
        } else {
            config::load(&args.config.clone().unwrap_or_else(config::default_path))
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
    let (store, cached_roots, cached_view) = match cache_path.as_deref().map(cache::load) {
        Some(Ok(cache::Outcome::Loaded { store, roots, view })) => (store, roots, view),
        Some(Ok(cache::Outcome::Discarded)) => {
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
        moves: Vec::new(),
        unreadable: Vec::new(),
    };

    // **The library is not rebuilt because the app opened.** Scanning on every
    // launch made the library a function of gmuse having been run, which is
    // backwards: the library is the thing, and the player reads it. A file
    // appearing on disk is a change to the library, and a change to the library
    // is something you make, not something a launch notices on your behalf.
    //
    // Two cases still scan, and only two. `--rescan` is the deliberate ask.
    // An empty store is a first run, where the alternative is an empty library
    // and no way to tell why — the cache stays derivable, which keeps the
    // 2026-08-27 decision that it may be discarded at any time.
    let first_run = app.store.is_empty();
    if !app.roots.is_empty() && (args.rescan || first_run) {
        if args.rescan {
            // Forget mtimes so every file is re-read.
            app.store = Holdings::new();
        }
        let roots = app.roots.clone();
        let report = scan::scan(&mut app.store, &roots);
        println!("library: {report}");
        report_errors(&report);
        app.moves = report.moves.clone();
        app.unreadable = report.errors.iter().map(|(p, _)| p.clone()).collect();
        save_cache(&app);
    } else if !app.store.is_empty() {
        println!(
            "library: {} tracks — `gmuse --rescan` to take in changes on disk",
            app.store.len()
        );
    } else {
        println!(
            "library: empty — scan one with `gmuse --library <dir>`, or set\n\
             `library = [\"~/Music\"]` in {}",
            config::default_path().display()
        );
    }

    if args.migrate_log {
        let machine = crate::events::machine_id(&config::default_state_dir());
        match crate::model::migrate::run(
            &loaded.data_dir,
            &app.roots,
            &machine,
            crate::history::now(),
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
fn stale_cache(args: &cli::Cli) -> Option<std::path::PathBuf> {
    (!args.no_cache).then(|| args.cache.clone().unwrap_or_else(cache::default_path))
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
        && let Err(e) = cache::discard(path)
    {
        eprintln!("cache: {e}");
    }
}

/// One library operation, and then exit.
fn run_library(
    op: &cli::Shelf,
    loaded: &config::Loaded,
    roots: &[std::path::PathBuf],
    cache_path: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let machine = crate::events::machine_id(&config::default_state_dir());
    let device = crate::model::id::Id::derived(machine.as_bytes());
    match op {
        cli::Shelf::Restore { paths, write } => {
            let mut files: Vec<std::path::PathBuf> = Vec::new();
            for path in paths {
                if path.is_dir() {
                    for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
                        if entry.file_type().is_file() && crate::scan::is_audio(entry.path()) {
                            files.push(entry.path().to_path_buf());
                        }
                    }
                } else if crate::scan::is_audio(path) {
                    files.push(path.clone());
                }
            }
            files.sort();
            if files.is_empty() {
                eprintln!("no audio under what was named");
                return Ok(());
            }

            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let cat = log.catalogue();
            // **Resolved by hashing, like a retirement**, for the same reason:
            // identity is the audio and a path belongs to one device's copy.
            // It is what lets the bytes be moved back first and named here by
            // wherever they now sit.
            let mut coming = Vec::new();
            for file in &files {
                match crate::model::ingest::read(file) {
                    Ok(scanned) => {
                        let id = crate::model::id::Id::<crate::model::entity::Encoding>::of_audio(
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
                let now = crate::history::now_ms();
                for id in &coming {
                    log.restore_encoding(now, *id);
                }
                log.flush(&loaded.data_dir)?
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
        cli::Shelf::Remove { paths, write } => {
            // Every audio file under what was named, `.departed` included:
            // an album retired by hand before this existed is exactly the case
            // that wants a tombstone and no move.
            let mut files: Vec<std::path::PathBuf> = Vec::new();
            for path in paths {
                if path.is_dir() {
                    for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
                        if entry.file_type().is_file() && crate::scan::is_audio(entry.path()) {
                            files.push(entry.path().to_path_buf());
                        }
                    }
                } else if crate::scan::is_audio(path) {
                    files.push(path.clone());
                }
            }
            files.sort();
            if files.is_empty() {
                eprintln!("no audio under what was named");
                return Ok(());
            }

            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let cat = log.catalogue();
            // **Resolved by hashing, not by path.** A retirement is about
            // bytes, and the path is a property of one device's copy — which
            // is the whole reason nothing here is keyed by one. It also means
            // a file already sitting in `.departed` still resolves.
            // **Where every device says it holds each copy.** Identity is
            // the audio, so two paths can be one encoding — and retiring the
            // one you meant would retire the one you did not.
            let departed = root_of(roots).join(crate::scan::DEPARTED);
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
                match crate::model::ingest::read(file) {
                    Ok(scanned) => {
                        let id = crate::model::id::Id::<crate::model::entity::Encoding>::of_audio(
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
            let done = crate::storage::retire(
                &root,
                &retiring.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>(),
                *write,
            );
            for (path, why) in &done.failed {
                eprintln!("refused: {} — {why}", path.display());
            }

            let fresh: usize = if *write {
                let now = crate::history::now_ms();
                let before = log.records().len();
                for (_, id) in &retiring {
                    log.retire_encoding(now, *id);
                }
                let _ = before;
                log.flush(&loaded.data_dir)?
            } else {
                0
            };
            eprintln!(
                "{} file(s), {} to retire, {} would move to {}, {} refused",
                files.len(),
                retiring.len(),
                done.moved,
                crate::scan::DEPARTED,
                done.failed.len(),
            );
            if *write {
                eprintln!("{fresh} tombstone(s) written, {} moved", done.moved);
                drop_cache(cache_path.as_deref());
            } else {
                eprintln!("dry run — nothing written. Add --write.");
            }
        }
        cli::Shelf::Names => {
            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            crate::model::populate::populate(
                &mut log,
                roots,
                crate::history::now_ms(),
                crate::model::populate::Admits::Identified,
                &mut crate::model::ingest::Reads::default(),
            );
            let cat = log.catalogue();

            // Every name each id has ever been given, in log order, so the
            // last one — the one in force — reads last.
            //
            // **Through the merge**, because the raw log still names the loser
            // and the loser is no longer a row. Without this, every artist
            // merge adds a permanent entry here: an id nothing holds, listed
            // as being in doubt about a name it no longer owns.
            let mut said: std::collections::BTreeMap<_, Vec<&str>> = Default::default();
            for record in log.records() {
                if let crate::model::log::Fact::ArtistNamed { artist, name } = &record.fact {
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
                    *subject == crate::model::reference::Cataloguable::Artist(*artist)
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
        cli::Shelf::Duplicates => {
            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            crate::model::populate::populate(
                &mut log,
                roots,
                crate::history::now_ms(),
                crate::model::populate::Admits::Identified,
                &mut crate::model::ingest::Reads::default(),
            );
            let cat = log.catalogue();
            let library = crate::model::seam::Librarian::from_catalogue(&cat);

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
                    .credited_as(crate::model::reference::CreditOn::ReleaseGroup(*id));
                // **Folded, because capitalisation is a typo and not a
                // distinction.** `normalise` folds case when it mints a group
                // id for exactly that reason, so a report that compares titles
                // exactly is asking a different question from the one that
                // created the duplicate. `Early James — Strange Time To Be
                // Alive` and `… to Be Alive` are one record held twice, and
                // this listed neither.
                by_name
                    .entry((
                        crate::model::ingest::normalise(&who),
                        crate::model::ingest::normalise(&group.title),
                    ))
                    .or_default()
                    .push(*id);
            }

            let mut found = 0usize;
            // What the name rule already said, so the overlap pass below adds
            // to the report rather than repeating it.
            let mut reported: Vec<crate::model::id::Id<crate::model::entity::ReleaseGroup>> =
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
                        .credited_as(crate::model::reference::CreditOn::ReleaseGroup(ids[0])),
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
                crate::model::id::Id<crate::model::entity::ReleaseGroup>,
                std::collections::BTreeSet<crate::model::id::Id<crate::model::entity::Recording>>,
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
                                .credited_as(crate::model::reference::CreditOn::ReleaseGroup(id)),
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
            let files_on = |release: &crate::model::id::Id<crate::model::entity::Release>| {
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
                            .credited_as(crate::model::reference::CreditOn::ReleaseGroup(**group)),
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
        cli::Shelf::Copies => {
            let (log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let cat = log.catalogue();
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
        cli::Shelf::Slots { work } => {
            use crate::model::entity::{Medium, Recording};
            use crate::model::id::Id;

            let (log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let cat = log.catalogue();
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
            let library = crate::model::seam::Librarian::from_catalogue(&cat);
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
                for row in query::rows(
                    roots,
                    log.records(),
                    device,
                    crate::history::now_ms(),
                    query::Only::Everything,
                    // A contested seat is a question about the *recording* —
                    // which performance sits here — so one copy answers it.
                    query::Copies::Shown,
                    None,
                    &mut crate::model::ingest::Reads::default(),
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
        cli::Shelf::Verify => {
            let (log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let cat = log.catalogue();
            // **This device's holdings.** A locator another machine published
            // names a path that is not here, and hashing it is not something
            // this run can do or should claim to have done.
            let mine = cat.holdings.get(&device);
            let held: Vec<(
                &String,
                crate::model::id::Id<crate::model::entity::Encoding>,
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
        cli::Shelf::Artists => {
            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            crate::model::populate::populate(
                &mut log,
                roots,
                crate::history::now_ms(),
                crate::model::populate::Admits::Identified,
                &mut crate::model::ingest::Reads::default(),
            );
            let cat = log.catalogue();

            // **Every name an id has ever held, not just the one in force.**
            // A name-derived id can be renamed afterwards — `Flume & Chet
            // Faker` was minted and is now called `Flume` — and testing only
            // the current name would call that id MusicBrainz-derived and
            // merge the wrong way. So an id is name-derived if *any* name it
            // has ever been given mints it.
            let mut from_name: std::collections::BTreeSet<
                crate::model::id::Id<crate::model::entity::Artist>,
            > = Default::default();
            for record in log.records() {
                if let crate::model::log::Fact::ArtistNamed { artist, name } = &record.fact
                    && crate::model::id::Id::<crate::model::entity::Artist>::of_artist(None, name)
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
                    .entry(crate::model::ingest::normalise(name))
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
        cli::Shelf::Tree { levels, tracks } => {
            // **Parse at the boundary** (§12): the spec becomes `Levels`
            // before anything groups by it, through the *same* parser the `:`
            // line uses. A second reader of `genre@1` is a second place for
            // the depth syntax to drift, and #79's whole point is that the
            // compound string stops being load-bearing.
            let spec =
                crate::cmd::parse::levels_spec(levels).map_err(|e| format!("--levels: {e}"))?;
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

            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            crate::model::populate::populate(
                &mut log,
                roots,
                crate::history::now_ms(),
                crate::model::populate::Admits::Identified,
                &mut crate::model::ingest::Reads::default(),
            );
            let librarian = crate::model::seam::Librarian::from_catalogue(&log.catalogue());

            // Every row the library holds. The browser groups its *members* —
            // whatever the active filter left — and a filter is a separate
            // sentence from a level, so this asks the levels question against
            // the whole shelf and says how many rows that was.
            let members: std::collections::HashSet<_> = {
                use crate::model::rows::Listing as _;
                librarian.rows().into_iter().collect()
            };

            // `true` because that is what the UI starts with — the point is to
            // read what the browser would draw, not a second arrangement of it.
            let tree = crate::model::tree::Tree::grouped(&librarian, pair, true, &members);

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
                                crate::model::rows::CopyId::On(track) => Some(track),
                                crate::model::rows::CopyId::Loose(_) => None,
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
        cli::Shelf::Unfiled => {
            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let report = crate::model::populate::populate(
                &mut log,
                roots,
                crate::history::now_ms(),
                crate::model::populate::Admits::Identified,
                &mut crate::model::ingest::Reads::default(),
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
        cli::Shelf::Import {
            paths,
            write,
            into,
            unidentified,
        } => {
            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            // **Named paths, or whatever a scan found that nothing knows.**
            // The second is the common case and the reason this can be run
            // with no arguments: the scan already said which files it did not
            // recognise, and importing them is the answer to that offer.
            let targets: Vec<std::path::PathBuf> = if paths.is_empty() {
                let seen = crate::model::populate::reconcile(
                    &mut log,
                    roots,
                    crate::history::now_ms(),
                    &mut crate::model::ingest::Reads::default(),
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
                    let n = log.flush(&loaded.data_dir)?;
                    if n > 0 {
                        eprintln!("{n} record(s) written: the holdings moved");
                        drop_cache(cache_path.as_deref());
                    }
                }
                return Ok(());
            }

            let report = crate::model::populate::populate(
                &mut log,
                &targets,
                crate::history::now_ms(),
                if *unidentified {
                    crate::model::populate::Admits::Anything
                } else {
                    crate::model::populate::Admits::Identified
                },
                &mut crate::model::ingest::Reads::default(),
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
                crate::model::populate::reconcile(
                    &mut log,
                    &walk,
                    crate::history::now_ms(),
                    &mut crate::model::ingest::Reads::default(),
                );

                // Import is strict, so anything that got this far is
                // describable — every row has a credit and a title, because
                // ingest is total — and the renderer cannot fail for want of
                // metadata. What it can refuse is a *destination*: a name too
                // long, a collision. Such a file is catalogued and left where
                // it is, because the library's contents must not depend on
                // path lengths.
                let root = roots.first().cloned().unwrap_or_default();
                let mut placed = crate::model::seam::Librarian::from_catalogue(&log.catalogue());
                placed.here(device);
                let taken: Vec<_> = report
                    .identified
                    .keys()
                    .filter_map(|at| crate::model::ingest::read(at).ok())
                    .map(|s| crate::model::id::Id::of_audio(&s.audio))
                    .collect();
                let plan = crate::storage::plan_of(&placed, &root, &taken);
                let done = crate::storage::carry_out(&plan, &root, true);
                for (at, why) in &done.failed {
                    eprintln!("could not place {}: {why}", at.display());
                }
                for (copy, why) in &plan.refused {
                    eprintln!("left where it is: {copy} — {why}");
                }
                eprintln!("{} file(s) placed, {} left alone", done.moved, plan.settled);

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
                    crate::model::populate::reconcile(
                        &mut log,
                        &walk,
                        crate::history::now_ms(),
                        &mut crate::model::ingest::Reads::default(),
                    );
                }
                // **Filed as it is brought in** (#94). The collection is a
                // judgement a person made, and this is the moment they made
                // it — deriving it afterwards from the folder is the
                // inference the collection model exists to replace.
                if let Some(name) = into {
                    let now = crate::history::now_ms();
                    let collection =
                        crate::model::id::Id::<crate::model::entity::Collection>::of_name(name);
                    log.append_fact(
                        now,
                        crate::model::log::Fact::CollectionNamed {
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
                            crate::model::log::Fact::CollectionEntryAdded {
                                collection,
                                recording: *recording,
                                ord: crate::model::ord::OrdKey::first(),
                            },
                        );
                    }
                    eprintln!("{} filed into {name}", report.identified.len());
                }
                let n = log.flush(&loaded.data_dir)?;
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
        cli::Shelf::Render { write } => {
            let (mut library_log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let mut library =
                crate::model::seam::Librarian::from_catalogue(&library_log.catalogue());
            library.here(device);
            let root = roots.first().cloned().unwrap_or_default();
            let plan = crate::storage::plan(&library, &root);

            for (_, from, to) in &plan.moves {
                println!("{}\t{}", from.display(), root.join(to).display());
            }
            for (copy, why) in &plan.refused {
                eprintln!("refused: {copy} — {why}");
            }
            let done = crate::storage::carry_out(&plan, &root, *write);
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
            if *write {
                // **The locators follow the bytes.** Identity is the audio, so
                // a moved file is the same encoding at a new place — and the
                // holdings fact is what carries that.
                crate::model::populate::reconcile(
                    &mut library_log,
                    roots,
                    crate::history::now_ms(),
                    &mut crate::model::ingest::Reads::default(),
                );
                let n = library_log.flush(&loaded.data_dir)?;
                eprintln!("{} folder(s) pruned, {n} record(s) written", done.emptied);
                if done.moved > 0 || n > 0 {
                    drop_cache(cache_path.as_deref());
                }
            } else {
                eprintln!("dry run — nothing moved. Add --write.");
            }
        }
        cli::Shelf::Query {
            unidentified,
            every_copy,
            filter,
        } => {
            let (log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let only = if *unidentified {
                query::Only::Unidentified
            } else {
                query::Only::Everything
            };
            let copies = if *every_copy {
                query::Copies::Every
            } else {
                query::Copies::Shown
            };
            // **Parse at the boundary** (§12), through the parser the `:` line
            // uses. A second reader of the filter language is a second place
            // for it to drift.
            let filter = filter
                .as_deref()
                .map(crate::filter::Expr::parse)
                .transpose()
                .map_err(|e| format!("--filter: {e}"))?;
            // **Kept between runs** — #188. Both walks below ask the same
            // question of the same files, and every run before this one asked
            // it too. A `stat` says whether the answer still holds.
            let seen_path = crate::cache::measurements_path();
            let mut seen = crate::cache::load_measurements(&seen_path);
            for row in query::rows(
                roots,
                log.records(),
                device,
                crate::history::now_ms(),
                only,
                copies,
                filter.as_ref(),
                &mut seen,
            ) {
                println!("{}", serde_json::to_string(&row)?);
            }
            let (hit, read) = seen.tally();
            if read > 0 || hit > 0 {
                eprintln!("{hit} file(s) unchanged, {read} read");
            }
            seen.prune_missing();
            crate::cache::save_measurements(&seen_path, &seen);
        }
        cli::Shelf::Apply { file, write } => {
            let outcome = apply::run(
                file,
                &loaded.data_dir,
                roots,
                device,
                crate::history::now_ms(),
                *write,
            )?;
            println!("{outcome}");
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
fn run_tui(
    app: &mut App,
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

    // Every log in the directory — this device's and any that arrived by sync.
    //
    // **No `moved` bookkeeping and no `seen` sweep.** Both existed to keep a
    // path-keyed history attached to files that move, and neither has anything
    // to do now that a fact names a recording (#81). A rename is invisible to
    // the log, which is the point.
    let machine = crate::events::machine_id(&config::default_state_dir());
    let device = crate::model::id::Id::derived(machine.as_bytes());
    let now = crate::history::now();
    let now_ms = crate::history::now_ms();
    let (log, damage) = crate::model::instance::Instance::open(&loaded.data_dir, device, now_ms);
    let listened = log.catalogue();
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
    let first_run = log.records().is_empty();

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

    // One scan, two shapes. The store is what the player still reads through;
    // the librarian is what it is moving to (#77), and building both from the
    // same pass is what keeps them from disagreeing.
    let assembling = std::time::Instant::now();
    let library_view = crate::bridge::librarian(&app.store, log.records(), device, now_ms);
    let known = library_view.recordings().count();
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
        .filter(|h| !library_view.holds_encoding(crate::model::id::Id::of_audio(&h.audio)))
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
        .and_then(|src| view::Stack::parse(src).ok())
        .unwrap_or_default();
    let members = composed
        .resolve(
            &library_view,
            &listened,
            crate::filter::Env {
                listened: Some(&listened),
                now,
            },
        )
        .copies(&library_view);

    // **Grouped by what the model says, not by what the tags carry** (#84).
    // The librarian has to exist first, which is why this sits below rather
    // than beside the membership it is built from.
    let library = Shelf::project(&library_view, members, smart_artist_sort);

    let mut library_view = library_view;
    let ctx = ui::Context {
        library: &library_view,
        player: Some(&app.player),
        remote: server.as_ref(),
    };
    let mut state = ui::Ui::new(library);
    state.playlists = loaded.playlists.clone();

    // Opened before the terminal is taken over, so a failure is visible.
    if let Some(path) = log_path {
        match audit::Audit::open(&path) {
            Ok(mut audit) => {
                audit.line("device", &format!("{} Hz output", app.player.device_rate()));
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

    // An acquisition date is a *library* fact now, so it arrives with an
    // import rather than being invented by whichever machine noticed the file
    // first. Nothing is stamped here.
    {
        let records = log.records().len();
        let known = listened.ratings.len() + listened.plays.len();
        state.log = log;
        state.listened = listened;
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
                        .and_then(|p| library_view.row_of(p.recording, p.copy))
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
            "replaygain={} shuffle={} aaa_mode={}",
            state.replaygain.label(),
            state.playback.shuffle,
            state.playback.aaa_mode.label(),
        ),
    );

    // **The loop hands back a reason, and this is what acts on it** (#120).
    //
    // The librarian is owned here and lent to the loop, so nothing inside
    // could ever rebuild it — the borrow outlives every command. Now the loop
    // returns, the borrow ends, the fold is redone, and it runs again. The
    // state the UI holds is untouched: only what it *reads through* is
    // replaced.
    let mut terminal = ratatui::init();
    let result = loop {
        let stop = {
            let ctx = ui::Context {
                library: &library_view,
                player: Some(&app.player),
                remote: server.as_ref(),
            };
            match ui::run(&mut terminal, &mut state, &ctx) {
                Ok(stop) => stop,
                Err(e) => break Err(e),
            }
        };
        // Every borrow of `library_view` has ended here, which is the whole
        // reason the loop returns rather than acting in place.
        match stop {
            ui::Stop::Quit => break Ok(()),
            ui::Stop::Rescan => {
                // The disk first, then the log — a scan reconciles holdings
                // and reads no tags it has not been asked for, and the refold
                // below is what turns that into rows.
                let roots = app.roots.clone();
                let report = scan::scan(&mut app.store, &roots);
                state.audit.line("rescan", &report.to_string());
                state.notify(format!("rescan: {report}"));
                app.unreadable = report.errors.iter().map(|(p, _)| p.clone()).collect();
                save_cache(app);
                library_view =
                    crate::bridge::librarian(&app.store, state.log.records(), device, now_ms);
            }
            ui::Stop::Refold => {
                library_view =
                    crate::bridge::librarian(&app.store, state.log.records(), device, now_ms);
            }
            // **Outside the draw loop, which is the whole point of the trip
            // out here** (#99): the editor wants the terminal, so the screen
            // is given back before it runs and taken again after.
            ui::Stop::Edit => {
                ratatui::restore();
                let said = match edit::edit(&state.composed, state.data_dir.as_deref()) {
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
            }
        }
        // The shelf is a projection of the librarian, so it goes too — through
        // the same door a `:view` change uses, or refiltering would file
        // things a second way (#84).
        let ctx = ui::Context {
            library: &library_view,
            player: Some(&app.player),
            remote: server.as_ref(),
        };
        ui::refresh_library(&mut state, &ctx);
    };
    ratatui::restore();
    // Written after the terminal is restored, so a failure is visible.
    // A fresh borrow of whatever the last refold produced, not the one the
    // setup above took: that one is long gone, and it named an older fold.
    let ctx = ui::Context {
        library: &library_view,
        player: Some(&app.player),
        remote: server.as_ref(),
    };
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
        && let Err(e) = cache::save(path, &app.store, &app.roots, app.view.as_deref())
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

/// What one holding turned out to be, when the bytes were read again.
#[derive(Debug, PartialEq)]
enum Holds {
    /// The file hashes to the id the library recorded for it.
    Agrees,
    /// It hashes to something else — the bytes were replaced without the
    /// library being told, and every id derived from the old ones now names
    /// audio that is not there.
    Changed(crate::model::id::Id<crate::model::entity::Encoding>),
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
fn still_holds(at: &str, said: crate::model::id::Id<crate::model::entity::Encoding>) -> Holds {
    let path = std::path::Path::new(at);
    if !path.exists() {
        return Holds::Gone;
    }
    match crate::model::ingest::read(path) {
        Ok(scanned) => {
            let now =
                crate::model::id::Id::<crate::model::entity::Encoding>::of_audio(&scanned.audio);
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
    cat: &'a crate::model::log::Catalogue,
    departed: &std::path::Path,
    going: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeMap<crate::model::id::Id<crate::model::entity::Encoding>, Vec<&'a str>>
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

#[cfg(test)]
mod tests {
    use super::*;

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
        use crate::model::entity::Encoding;
        use crate::model::id::Id;

        let dir = std::env::temp_dir().join("gmuse-verify");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join("a.wav");
        wav(&at, 1000);

        let said =
            Id::<Encoding>::of_audio(&crate::model::ingest::read(&at).expect("readable").audio);
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
        use crate::model::entity::Encoding;
        use crate::model::id::Id;
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
        use crate::model::entity::{Encoding, Recording};
        use crate::model::id::Id;

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
        use crate::model::entity::{Device, Encoding};
        use crate::model::id::Id;
        use crate::model::log::{Asserted, Catalogue};

        let going_path = "/m/Bodzin & Romboy/Luna/01 Triton.mp3";
        let bytes = Id::<Encoding>::derived(b"triton");
        let mut cat = Catalogue::default();
        cat.holdings.insert(
            Id::<Device>::derived(b"laptop"),
            Asserted {
                at: crate::model::clock::Stamp {
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
        use crate::model::entity::{Device, Encoding};
        use crate::model::id::Id;
        use crate::model::log::{Asserted, Catalogue};

        let going_path = "/m/Early James/copy two/01 Blue Pill.mp3";
        let staying = "/m/Early James/Strange Time/01 Blue Pill.mp3";
        let bytes = Id::<Encoding>::derived(b"one rip, two folders");
        let mut cat = Catalogue::default();
        cat.holdings.insert(
            Id::<Device>::derived(b"laptop"),
            Asserted {
                at: crate::model::clock::Stamp {
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
        use crate::model::entity::{Device, Encoding};
        use crate::model::id::Id;
        use crate::model::log::{Asserted, Catalogue};

        let bytes = Id::<Encoding>::derived(b"moved by hand");
        let mut cat = Catalogue::default();
        cat.holdings.insert(
            Id::<Device>::derived(b"laptop"),
            Asserted {
                at: crate::model::clock::Stamp {
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
