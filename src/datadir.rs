//! Noticing that `data_dir` moved — issue #27.
//!
//! Editing `data_dir` changes where gmuse *looks*, not where the data *is*.
//! Nothing follows it, so the next launch folds from nothing and reports an
//! empty history while the ratings sit intact in the previous directory. That
//! is the failure #24's migration was written to prevent, one step later, and
//! its reasoning applies verbatim: *a version that silently started from an
//! empty history looks exactly like the ratings being gone.*
//!
//! **It cannot be fixed by moving the data automatically**, because three
//! situations are indistinguishable from inside:
//!
//! - `data_dir` was changed, and the data is in the old place
//! - `data_dir` is right and this is genuinely a first run
//! - `data_dir` is right and nothing has synced from the other machine yet
//!
//! The third is real now that the directory is meant to be shared, and it is
//! the one where helpfully copying from somewhere else would be wrong. So this
//! detects, declines, and names the remedy — the same shape as every other
//! ambiguity in the program.
//!
//! The last directory used is remembered in `$XDG_STATE_HOME`, **not** in
//! `data_dir`: it is a fact about this machine, and putting it in the synced
//! directory would make two machines overwrite each other's answer.

use std::path::{Path, PathBuf};

const REMEMBERED: &str = "last-data-dir";

/// What the current `data_dir` looks like against the remembered one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Same directory as last time, or a first run, or the data is already
    /// where it should be. Nothing to say.
    Fine,
    /// It moved, and the old directory still holds data. **Say so.**
    Stranded { from: PathBuf },
}

/// Decide, without touching anything.
///
/// `has_data` is passed rather than probed so this stays pure and can be
/// tested against every combination without a filesystem.
pub fn check(remembered: Option<&Path>, current: &Path, has_data: impl Fn(&Path) -> bool) -> Check {
    let Some(from) = remembered else {
        return Check::Fine;
    };
    if from == current {
        return Check::Fine;
    }
    // Only the *old* directory being empty settles it. Asking whether the new
    // one has data does not: gmuse writes an events log there on the very
    // launch that warns, so the second launch would see data in both places,
    // conclude the move had happened by hand, and go quiet for ever with the
    // ratings still stranded — the silent loss this exists to prevent, arrived
    // at by a different route.
    //
    // Two directories both holding ratings is therefore reported rather than
    // excused. It is what a half-finished move looks like, and being told is
    // the point.
    if !has_data(from) {
        return Check::Fine;
    }
    Check::Stranded {
        from: from.to_path_buf(),
    }
}

/// Whether a directory holds anything worth migrating.
pub fn has_data(dir: &Path) -> bool {
    if dir.join("history.toml").exists() || dir.join("playlists").is_dir() {
        return true;
    }
    !crate::events::logs_in(dir).is_empty()
}

pub fn remembered(state_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(state_dir.join(REMEMBERED)).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// Record where the data is now.
///
/// Only called when there is nothing stranded — otherwise a launch that
/// ignored the warning would forget where the data was and the warning would
/// never come back.
pub fn remember(state_dir: &Path, dir: &Path) {
    if std::fs::create_dir_all(state_dir).is_ok() {
        let _ = std::fs::write(state_dir.join(REMEMBERED), format!("{}\n", dir.display()));
    }
}

/// What a migration did, and what it declined to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    pub took: Vec<PathBuf>,
    /// An events log that landed under a suffixed name because one of its own
    /// name was already there. See [`move_data`] for why that is safe.
    pub merged: Vec<Merged>,
    /// Left behind because something of that name was already there and could
    /// not be merged. Never overwritten: the destination is newer by
    /// definition, and clobbering it is the loss this whole issue is about.
    pub left: Vec<PathBuf>,
}

/// One log folded in beside another of the same name.
#[derive(Debug, PartialEq, Eq)]
pub struct Merged {
    pub at: PathBuf,
    /// Events the destination did not already have.
    pub took: usize,
    /// Events it did, skipped rather than carried over. Reported rather than
    /// assumed to be none: a log *copied* forward instead of moved is entirely
    /// these, and the listener should hear that nothing new was found in it.
    pub already: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    #[error("moving {what} to {to}: {source}")]
    Failed {
        what: PathBuf,
        to: PathBuf,
        source: std::io::Error,
    },
}

/// Move everything gmuse keeps in a data directory.
///
/// The base, **every** log, and the playlists — `history.toml` alone would
/// leave the recent plays and ratings behind, which is the same silent loss in
/// a smaller shape.
///
/// A rename first, falling back to copy-then-remove, because the two may well
/// be on different disks — pointing `data_dir` at a NAS is why the setting
/// exists. Anything already at the destination is left alone rather than
/// overwritten.
pub fn move_data(from: &Path, to: &Path) -> Result<Moved, MoveError> {
    let mut moved = Moved::default();
    if from == to || !from.exists() {
        return Ok(moved);
    }
    std::fs::create_dir_all(to).map_err(|source| MoveError::Failed {
        what: from.to_path_buf(),
        to: to.to_path_buf(),
        source,
    })?;

    let mut wanted: Vec<PathBuf> = vec![from.join("history.toml"), from.join("playlists")];
    wanted.extend(crate::events::logs_in(from));

    for path in wanted {
        if !path.exists() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let target = to.join(name);
        if target.exists() {
            // A name clash on an events log is the *likely* case, not a rare
            // one: it is the same machine's log, and gmuse writes one at the
            // new location on the very launch that warns. Logs fold together
            // by timestamp when they share a directory, so the incoming events
            // only need a file of their own.
            match plan_log(&path, &target) {
                Some((at, content, took, already)) => {
                    // Written, not renamed: only the events the destination
                    // lacks travel, so nothing can be counted twice.
                    std::fs::write(&at, &content).map_err(|source| MoveError::Failed {
                        what: path.clone(),
                        to: at.clone(),
                        source,
                    })?;
                    let _ = remove(&path);
                    moved.merged.push(Merged { at, took, already });
                }
                // Anything else is left where it is, and **said so**. That is
                // the difference between a decision the listener can make and
                // a loss they never hear about.
                None => moved.left.push(path),
            }
            continue;
        }
        if std::fs::rename(&path, &target).is_err() {
            copy_across(&path, &target).map_err(|source| MoveError::Failed {
                what: path.clone(),
                to: target.clone(),
                source,
            })?;
            let _ = remove(&path);
        }
        moved.took.push(target);
    }
    Ok(moved)
}

/// Where an incoming log should land, what to write there, and the counts.
///
/// Only the events the destination **lacks** travel. An event is a fact, and a
/// log copied forward rather than moved holds facts the destination already
/// has — carrying those over would count every one of those plays twice.
/// Taking the difference handles every case: a full copy contributes nothing,
/// a partial overlap contributes its new half, a disjoint log contributes all
/// of itself. The counts go back to the caller so none of it is silent.
///
/// Only logs. A second `history.toml` is a *base*, not a stream of facts, and
/// two of them cannot be folded together.
fn plan_log(incoming: &Path, existing: &Path) -> Option<(PathBuf, String, usize, usize)> {
    let name = incoming.file_name()?.to_str()?;
    if !(name.starts_with("events-") && name.ends_with(".log")) {
        return None;
    }
    let (mine, theirs_text) = (
        std::fs::read_to_string(incoming).ok()?,
        std::fs::read_to_string(existing).ok()?,
    );
    let theirs: std::collections::HashSet<&str> = theirs_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();

    let (mut content, mut took, mut already) = (String::new(), 0usize, 0usize);
    for line in mine.lines().filter(|l| !l.trim().is_empty()) {
        if theirs.contains(line) {
            already += 1;
        } else {
            content.push_str(line);
            content.push('\n');
            took += 1;
        }
    }
    if took == 0 {
        // Nothing new. Reported by the caller as left behind, which is honest:
        // the file is still there and holds nothing the destination lacks.
        return None;
    }

    let dir = existing.parent()?;
    let stem = name.trim_end_matches(".log");
    for n in 0..100 {
        let candidate = match n {
            0 => dir.join(format!("{stem}-recovered.log")),
            _ => dir.join(format!("{stem}-recovered-{n}.log")),
        };
        if !candidate.exists() {
            return Some((candidate, content, took, already));
        }
    }
    None
}

/// `std::fs::copy` handles a file; a directory needs walking.
fn copy_across(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_file() {
        std::fs::copy(from, to)?;
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_across(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

fn remove(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("gmuse-datadir-{name}-{n}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The three cases that are indistinguishable from inside, and the one
    /// that is not.
    #[test]
    fn only_a_move_that_stranded_data_is_reported() {
        let never = |_: &Path| false;
        let always = |_: &Path| true;
        let (old, new) = (Path::new("/old"), Path::new("/new"));

        // A first run: nothing remembered, so nothing to say.
        assert_eq!(check(None, new, always), Check::Fine);
        // Same directory as last time.
        assert_eq!(check(Some(new), new, always), Check::Fine);
        // Moved by hand: the old place is empty, so nothing was stranded.
        assert_eq!(check(Some(old), new, never), Check::Fine);

        // Both hold data — a half-finished move. Reported, **not** excused:
        // gmuse writes a log into the new directory on the very launch that
        // warns, so treating "the new place has data" as proof of a hand-move
        // would make the second launch go quiet with the ratings still
        // stranded.
        assert_eq!(
            check(Some(old), new, always),
            Check::Stranded {
                from: old.to_path_buf()
            }
        );

        // Moved, the new place is empty and the old is not. **This** is the
        // one worth interrupting for.
        assert_eq!(
            check(Some(old), new, |p| p == old),
            Check::Stranded {
                from: old.to_path_buf()
            }
        );
    }

    #[test]
    fn the_remembered_directory_round_trips() {
        let state = dir("remember");
        assert_eq!(remembered(&state), None);
        remember(&state, Path::new("/m/.gmuse"));
        assert_eq!(remembered(&state), Some(PathBuf::from("/m/.gmuse")));
        // Written with a trailing newline, so it reads as a text file; the
        // trim is what makes that safe.
        remember(&state, Path::new("/other"));
        assert_eq!(remembered(&state), Some(PathBuf::from("/other")));
    }

    #[test]
    fn a_directory_holds_data_if_any_of_the_three_are_there() {
        let d = dir("has-data");
        assert!(!has_data(&d));

        std::fs::write(d.join("history.toml"), "").unwrap();
        assert!(has_data(&d));

        let logs = dir("has-data-logs");
        std::fs::write(logs.join("events-laptop.log"), "").unwrap();
        assert!(has_data(&logs), "an events log alone counts");

        let lists = dir("has-data-playlists");
        std::fs::create_dir_all(lists.join("playlists")).unwrap();
        assert!(has_data(&lists), "playlists alone count");
    }

    /// Everything moves, not just the base: `history.toml` alone would leave
    /// the recent plays and ratings behind — the same silent loss, smaller.
    #[test]
    fn moving_takes_the_logs_and_the_playlists_too() {
        let (from, to) = (dir("move-from"), dir("move-to"));
        std::fs::write(from.join("history.toml"), "version = 1\n").unwrap();
        std::fs::write(from.join("events-laptop.log"), "100\tplayed\t/m/a.mp3\n").unwrap();
        std::fs::write(from.join("events-desktop.log"), "200\tplayed\t/m/b.mp3\n").unwrap();
        std::fs::create_dir_all(from.join("playlists")).unwrap();
        std::fs::write(from.join("playlists/mix.m3u8"), "#EXTM3U\n").unwrap();

        let moved = move_data(&from, &to).unwrap();
        assert_eq!(moved.took.len(), 4, "{moved:?}");
        assert!(moved.left.is_empty());
        assert!(to.join("history.toml").exists());
        assert!(to.join("events-laptop.log").exists());
        assert!(to.join("events-desktop.log").exists());
        assert!(
            to.join("playlists/mix.m3u8").exists(),
            "a directory moves whole"
        );
        assert!(!from.join("history.toml").exists(), "the original stayed");
    }

    /// Anything already at the destination wins: it is newer by definition,
    /// and overwriting it is the loss this whole issue is about.
    #[test]
    fn moving_never_overwrites_what_is_already_there() {
        let (from, to) = (dir("clash-from"), dir("clash-to"));
        std::fs::write(from.join("history.toml"), "old").unwrap();
        std::fs::write(to.join("history.toml"), "current").unwrap();

        let moved = move_data(&from, &to).unwrap();
        assert!(moved.took.is_empty());
        // And it is reported, not silently skipped.
        assert_eq!(moved.left, vec![from.join("history.toml")]);
        assert_eq!(
            std::fs::read_to_string(to.join("history.toml")).unwrap(),
            "current"
        );
        assert!(
            from.join("history.toml").exists(),
            "nothing moved, nothing removed"
        );
    }

    /// The likely clash, not a rare one: the same machine's log, because
    /// gmuse writes one at the new location on the launch that warns. Two
    /// disjoint logs merge by sitting in one directory — the fold reads every
    /// one — so the incoming file only needs a name of its own.
    #[test]
    fn a_clashing_log_lands_under_its_own_name() {
        let (from, to) = (dir("log-clash-from"), dir("log-clash-to"));
        std::fs::write(from.join("events-a.log"), "100\tplayed\t/m/a.mp3\n").unwrap();
        std::fs::write(to.join("events-a.log"), "200\tplayed\t/m/b.mp3\n").unwrap();

        let moved = move_data(&from, &to).unwrap();
        assert!(moved.left.is_empty(), "{moved:?}");
        assert_eq!(
            moved.merged,
            vec![Merged {
                at: to.join("events-a-recovered.log"),
                took: 1,
                already: 0,
            }]
        );
        // Both survive, and the fold sorts them together by timestamp.
        assert!(to.join("events-a.log").exists());
        assert!(to.join("events-a-recovered.log").exists());
        assert!(!from.join("events-a.log").exists(), "the original stayed");

        let (folded, applied) = crate::events::folded(&to.join("nothing.toml"), &to);
        assert_eq!(applied, 2, "both logs did not fold");
        assert_eq!(folded.len(), 2);
    }

    /// A log **copied** forward rather than moved holds only events the
    /// destination already has. Nothing new to take, so it is left and named
    /// — carrying it over would count every one of those plays twice.
    #[test]
    fn a_log_that_is_only_a_copy_is_left_alone() {
        let (from, to) = (dir("log-copy-from"), dir("log-copy-to"));
        let shared = "100\tplayed\t/m/a.mp3\n";
        std::fs::write(from.join("events-a.log"), shared).unwrap();
        std::fs::write(
            to.join("events-a.log"),
            format!("{shared}200\tplayed\t/m/b.mp3\n"),
        )
        .unwrap();

        let moved = move_data(&from, &to).unwrap();
        assert!(moved.merged.is_empty(), "a pure copy was merged");
        assert_eq!(moved.left, vec![from.join("events-a.log")]);
        assert!(!to.join("events-a-recovered.log").exists());
    }

    /// The realistic case: the logs overlap *partly*. Only the events the
    /// destination lacks travel, and the rest are counted and reported —
    /// declining the whole file would strand the new half.
    #[test]
    fn a_partly_overlapping_log_contributes_only_its_new_events() {
        let (from, to) = (dir("log-partial-from"), dir("log-partial-to"));
        let shared = "100\tplayed\t/m/a.mp3\n";
        std::fs::write(
            from.join("events-a.log"),
            format!("{shared}300\tplayed\t/m/c.mp3\n"),
        )
        .unwrap();
        std::fs::write(
            to.join("events-a.log"),
            format!("{shared}200\tplayed\t/m/b.mp3\n"),
        )
        .unwrap();

        let moved = move_data(&from, &to).unwrap();
        assert_eq!(
            moved.merged,
            vec![Merged {
                at: to.join("events-a-recovered.log"),
                took: 1,
                already: 1,
            }]
        );
        // Exactly the new event, so the shared one cannot be counted twice.
        let (folded, applied) = crate::events::folded(&to.join("nothing.toml"), &to);
        assert_eq!(applied, 3, "an event was lost or duplicated");
        assert_eq!(folded.get(Path::new("/m/a.mp3")).unwrap().plays, 1);
        assert_eq!(folded.get(Path::new("/m/c.mp3")).unwrap().plays, 1);
    }

    /// Only logs merge. A second `history.toml` is a *base*, not a stream of
    /// facts, and two of them cannot be folded together.
    #[test]
    fn a_clashing_base_is_still_left_alone() {
        let (from, to) = (dir("base-clash-from"), dir("base-clash-to"));
        std::fs::write(from.join("history.toml"), "old").unwrap();
        std::fs::write(to.join("history.toml"), "current").unwrap();

        let moved = move_data(&from, &to).unwrap();
        assert!(moved.merged.is_empty());
        assert_eq!(moved.left, vec![from.join("history.toml")]);
    }

    #[test]
    fn moving_from_nowhere_is_not_an_error() {
        let to = dir("move-nowhere");
        assert_eq!(
            move_data(Path::new("/nope/gmuse"), &to).unwrap(),
            Moved::default()
        );
        assert_eq!(
            move_data(&to, &to).unwrap(),
            Moved::default(),
            "a no-op move"
        );
    }
}
