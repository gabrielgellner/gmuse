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
        cli::Shelf::Names => {
            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            crate::model::populate::populate(&mut log, roots, crate::history::now_ms());
            let cat = log.catalogue();

            // Every name each id has ever been given, in log order, so the
            // last one — the one in force — reads last.
            let mut said: std::collections::BTreeMap<_, Vec<&str>> = Default::default();
            for record in log.records() {
                if let crate::model::log::Fact::ArtistNamed { artist, name } = &record.fact {
                    let seen: &mut Vec<&str> = said.entry(*artist).or_default();
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
            for (artist, names) in said.iter().filter(|(_, n)| n.len() > 1) {
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
                 artists with two real names, and which to use is yours to say."
            );
        }
        cli::Shelf::Duplicates => {
            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            crate::model::populate::populate(&mut log, roots, crate::history::now_ms());
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
            for ((_, _), mut ids) in by_name {
                if ids.len() < 2 {
                    continue;
                }
                found += 1;
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
        }
        cli::Shelf::Unfiled => {
            let (mut log, _) = crate::model::instance::Instance::open(
                &loaded.data_dir,
                device,
                crate::history::now_ms(),
            );
            let report =
                crate::model::populate::populate(&mut log, roots, crate::history::now_ms());
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
        cli::Shelf::Import { paths, write } => {
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
                let seen =
                    crate::model::populate::reconcile(&mut log, roots, crate::history::now_ms());
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

            let report =
                crate::model::populate::populate(&mut log, &targets, crate::history::now_ms());
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
            for (path, recording, had) in &report.joins {
                eprintln!(
                    "joins a recording already held as {had} other encoding(s): {}\n    {recording} — an upgrade, a duplicate, or a second format?",
                    path.display()
                );
            }
            eprintln!(
                "{} file(s), {} confirmed, {} resting on their bytes, {} refused, {} joining a recording already held, {} new fact(s)",
                report.ingested,
                report.confirmed,
                report.provisional,
                report.contradicts.len(),
                report.joins.len(),
                report.written
            );
            if *write {
                // **Publish the holdings first, then place** (#132, #145).
                // The planner asks where a copy *is* before saying where it
                // belongs, and a file taken in a moment ago is not held
                // anywhere until a walk says so.
                crate::model::populate::reconcile(&mut log, roots, crate::history::now_ms());

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
                if done.moved > 0 {
                    crate::model::populate::reconcile(&mut log, roots, crate::history::now_ms());
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
        cli::Shelf::Query { unidentified } => {
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
            for row in query::rows(roots, log.records(), device, crate::history::now_ms(), only) {
                println!("{}", serde_json::to_string(&row)?);
            }
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
