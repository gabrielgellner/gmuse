//! Per-track history: how often a track has been played, when, and when it
//! first appeared (issue #13).
//!
//! **Durable, not derived.** It cannot be rebuilt by rescanning, so it lives
//! under `data_dir` rather than in the cache — the Decision Log records what
//! happened the one time durable data shared a lifetime with derived data.
//! Written temp-then-rename.
//!
//! Under `data_dir` rather than `$XDG_STATE_HOME`, where it started, because
//! ratings and play counts are the one thing here worth backing up and the
//! state directory is exactly what a backup tool skips. The session and the
//! audit log stay behind: losing where you were is not a loss.
//!
//! **Keyed by path, not `TrackId`.** An arena index is meaningful only while
//! the cache that produced it survives; a discarded cache reassigns every one,
//! and a history keyed by id would silently reattach to different tracks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;

/// Four minutes, or half the track — last.fm's rule, and the usual one.
const LONG_ENOUGH: Duration = Duration::from_secs(240);

/// `<data_dir>/history.toml`.
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join("history.toml")
}

/// Where the history lived before it moved to `data_dir`.
///
/// Kept so [`migrate`] can find it. Not a fallback read: a version that
/// silently started from an empty history would look exactly like the ratings
/// being gone, which is the failure this whole move is meant to prevent.
fn legacy_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse").join("history.toml")
}

/// Move a pre-`data_dir` history into place, if there is one and nothing is
/// there yet. Returns where it came from, so the caller can say so.
///
/// Moved rather than copied, so there is one file and no question later about
/// which is current. A failed rename falls back to copy-then-remove, because
/// `data_dir` may well be on another disk — pointing it at a NAS is the whole
/// reason the setting exists.
pub fn migrate(to: &Path) -> Option<PathBuf> {
    migrate_from(&legacy_path(), to)
}

/// The half of [`migrate`] that does not read the environment.
///
/// Split out so it is testable: mutating `XDG_STATE_HOME` to exercise the real
/// entry point would race every other test in the process.
fn migrate_from(from: &Path, to: &Path) -> Option<PathBuf> {
    if to.exists() || !from.exists() || from == to {
        return None;
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    if std::fs::rename(from, to).is_err() {
        std::fs::copy(from, to).ok()?;
        // A copy that cannot be cleaned up is still a successful migration;
        // the stale original is harmless because nothing reads it.
        let _ = std::fs::remove_file(from);
    }
    Some(from.to_path_buf())
}

/// Seconds since the epoch. Passed in rather than read, so the callers that
/// record are testable without a clock.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Whether a track was played rather than skipped past.
///
/// Half of it, or four minutes, whichever comes first. A track of unknown
/// length has no half to take, so it has to earn the full four minutes —
/// otherwise a zero duration would count a play the instant it started.
pub fn counts_as_played(position: Duration, duration: Duration) -> bool {
    let threshold = if duration.is_zero() {
        LONG_ENOUGH
    } else {
        (duration / 2).min(LONG_ENOUGH)
    };
    position >= threshold
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub plays: u32,
    #[serde(default)]
    pub last_played: Option<i64>,
    /// Stars, 1 to 5. `None` is unrated, which is distinct from a low rating
    /// and is what `!rating?` finds.
    ///
    /// **Ours, not the file's.** §1 keeps tag editing out of scope, so a
    /// rating lives here rather than in a `POPM` frame. That costs
    /// portability — trollstov carves out exactly this exception for exactly
    /// that reason — and buys never being the program that rewrote someone's
    /// library. See issue #14 if that trade is ever revisited.
    #[serde(default)]
    pub rating: Option<u8>,
    /// When gmuse first saw the file.
    ///
    /// Not the file's mtime, which moves for reasons that have nothing to do
    /// with the music — writing ReplayGain tags across the library reset every
    /// one of them in an afternoon.
    pub added: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    version: u32,
    /// Sorted, so the file does not churn between saves for no reason.
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
    #[serde(skip)]
    dirty: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("serialising the history: {0}")]
    Encode(#[from] toml::ser::Error),
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl History {
    /// Read the history. Anything unreadable, unparseable, or from another
    /// version starts empty — losing counts is bad, but guessing at a field's
    /// meaning across a format change is how a store corrupts itself.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str::<History>(&text) {
            Ok(h) if h.version == VERSION => h,
            _ => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), HistoryError> {
        let mut out = self.clone();
        out.version = VERSION;
        let text = toml::to_string_pretty(&out)?;
        let write = |path: &Path| -> Result<(), std::io::Error> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("toml.tmp");
            std::fs::write(&tmp, text.as_bytes())?;
            std::fs::rename(&tmp, path)
        };
        write(path).map_err(|source| HistoryError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn get(&self, path: &Path) -> Option<&Entry> {
        self.entries.get(&key(path))
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record that a file exists, without disturbing anything already known.
    ///
    /// This is where `added` comes from, so it must never overwrite: a track
    /// seen every launch would otherwise be permanently "added today".
    pub fn note_seen(&mut self, path: &Path, now: i64) {
        self.entries.entry(key(path)).or_insert_with(|| {
            self.dirty = true;
            Entry {
                added: now,
                ..Default::default()
            }
        });
    }

    /// Set or clear a rating. Zero clears, because "no stars" and "unrated"
    /// are the same thing to anyone typing it.
    pub fn note_rating(&mut self, path: &Path, stars: u8, now: i64) {
        let e = self.entries.entry(key(path)).or_insert(Entry {
            added: now,
            ..Default::default()
        });
        e.rating = (stars > 0).then_some(stars.min(5));
        self.dirty = true;
    }

    /// Record a play.
    /// Carry an entry to a track's new path (#25).
    ///
    /// Returns whether anything moved. The old key is **removed**, not left as
    /// a copy: two entries for one track would double-count a play the moment
    /// the file moved back, and the stale one would never be cleaned up.
    ///
    /// A destination that already has an entry wins — it was created by
    /// `note_seen` for a file that really is there, and overwriting it with a
    /// departed track's ratings would be a guess in the wrong direction.
    pub fn rename(&mut self, from: &Path, to: &Path) -> bool {
        let from = from.to_string_lossy().into_owned();
        let to = to.to_string_lossy().into_owned();
        if from == to || self.entries.contains_key(&to) {
            return false;
        }
        let Some(entry) = self.entries.remove(&from) else {
            return false;
        };
        self.entries.insert(to, entry);
        self.dirty = true;
        true
    }

    pub fn note_played(&mut self, path: &Path, now: i64) {
        let e = self.entries.entry(key(path)).or_insert(Entry {
            added: now,
            ..Default::default()
        });
        e.plays += 1;
        e.last_played = Some(now);
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-history-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rename_carries_an_entry_to_a_new_path() {
        let mut h = History::default();
        h.note_seen(Path::new("/m/old.mp3"), 100);
        h.note_played(Path::new("/m/old.mp3"), 200);
        h.note_rating(Path::new("/m/old.mp3"), 4, 200);

        assert!(h.rename(Path::new("/m/old.mp3"), Path::new("/m/new.mp3")));
        let e = h.get(Path::new("/m/new.mp3")).expect("carried");
        assert_eq!((e.plays, e.rating, e.added), (1, Some(4), 100));
        assert!(
            h.get(Path::new("/m/old.mp3")).is_none(),
            "the old key must go — two entries would double-count a play"
        );
    }

    /// The destination was created by `note_seen` for a file that really is
    /// there. Overwriting it with a departed track's ratings guesses in the
    /// wrong direction.
    #[test]
    fn rename_will_not_overwrite_an_existing_entry() {
        let mut h = History::default();
        h.note_rating(Path::new("/m/old.mp3"), 1, 100);
        h.note_rating(Path::new("/m/new.mp3"), 5, 100);

        assert!(!h.rename(Path::new("/m/old.mp3"), Path::new("/m/new.mp3")));
        assert_eq!(h.get(Path::new("/m/new.mp3")).unwrap().rating, Some(5));
        assert_eq!(h.get(Path::new("/m/old.mp3")).unwrap().rating, Some(1));
    }

    #[test]
    fn renaming_something_unknown_changes_nothing() {
        let mut h = History::default();
        h.note_rating(Path::new("/m/a.mp3"), 3, 100);
        assert!(!h.rename(Path::new("/m/nope.mp3"), Path::new("/m/b.mp3")));
        assert!(!h.rename(Path::new("/m/a.mp3"), Path::new("/m/a.mp3")));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn the_path_sits_under_the_data_dir() {
        assert_eq!(
            path_in(Path::new("/data/gmuse")),
            PathBuf::from("/data/gmuse/history.toml")
        );
    }

    /// The upgrade case, and the one that matters: real ratings exist in the
    /// old place and must arrive in the new one.
    #[test]
    fn migration_moves_a_legacy_history_and_reports_where_from() {
        let d = tmpdir("migrate");
        let legacy = d.join("state").join("history.toml");
        let mut h = History::default();
        h.note_seen(Path::new("/m/a.mp3"), 100);
        h.note_rating(Path::new("/m/a.mp3"), 5, 200);
        h.save(&legacy).unwrap();

        let target = path_in(&d.join("data"));
        assert_eq!(migrate_from(&legacy, &target), Some(legacy.clone()));
        assert!(!legacy.exists(), "the original must not be left behind");
        assert_eq!(
            History::load(&target)
                .get(Path::new("/m/a.mp3"))
                .unwrap()
                .rating,
            Some(5)
        );

        // A second run finds nothing to do rather than clobbering.
        assert_eq!(migrate_from(&legacy, &target), None);
    }

    /// An existing history in the new place wins. Migrating over it would
    /// discard whatever has been rated since the upgrade.
    #[test]
    fn migration_never_overwrites_an_existing_history() {
        let d = tmpdir("migrate-existing");
        let legacy = d.join("state").join("history.toml");
        let mut old = History::default();
        old.note_rating(Path::new("/m/a.mp3"), 1, 100);
        old.save(&legacy).unwrap();

        let target = path_in(&d.join("data"));
        let mut current = History::default();
        current.note_rating(Path::new("/m/a.mp3"), 5, 300);
        current.save(&target).unwrap();

        assert_eq!(migrate_from(&legacy, &target), None);
        assert_eq!(
            History::load(&target)
                .get(Path::new("/m/a.mp3"))
                .unwrap()
                .rating,
            Some(5),
            "the newer history was overwritten by the legacy one"
        );
        assert!(
            legacy.exists(),
            "nothing was migrated, so nothing was moved"
        );
    }

    #[test]
    fn migration_does_nothing_without_a_legacy_file() {
        let d = tmpdir("migrate-none");
        let target = path_in(&d.join("data"));
        assert_eq!(migrate_from(&d.join("nope.toml"), &target), None);
        assert!(!target.exists());
    }

    /// Half the track or four minutes, whichever comes first — so a skip does
    /// not count, and a long track does not have to finish.
    #[test]
    fn what_counts_as_a_play() {
        let three_min = Duration::from_secs(180);
        assert!(!counts_as_played(Duration::from_secs(3), three_min));
        assert!(!counts_as_played(Duration::from_secs(89), three_min));
        assert!(counts_as_played(Duration::from_secs(90), three_min));

        // A twenty-minute track earns it at four minutes, not at ten.
        let twenty = Duration::from_secs(1200);
        assert!(!counts_as_played(Duration::from_secs(239), twenty));
        assert!(counts_as_played(Duration::from_secs(240), twenty));
    }

    /// An unknown duration has no half to take. Without this a zero-length
    /// track would count a play the instant it started.
    #[test]
    fn an_unknown_length_must_earn_the_full_four_minutes() {
        let unknown = Duration::ZERO;
        assert!(!counts_as_played(Duration::from_secs(1), unknown));
        assert!(!counts_as_played(Duration::from_secs(239), unknown));
        assert!(counts_as_played(Duration::from_secs(240), unknown));
    }

    #[test]
    fn plays_accumulate_and_stamp_the_time() {
        let mut h = History::default();
        let p = Path::new("/m/a.mp3");
        h.note_played(p, 100);
        h.note_played(p, 200);
        let e = h.get(p).unwrap();
        assert_eq!(e.plays, 2);
        assert_eq!(e.last_played, Some(200));
    }

    /// `added` is the *first* sighting. A track seen every launch would
    /// otherwise be permanently "added today", which is the one thing this
    /// field exists not to be.
    #[test]
    fn added_is_never_overwritten() {
        let mut h = History::default();
        let p = Path::new("/m/a.mp3");
        h.note_seen(p, 1000);
        h.note_seen(p, 2000);
        h.note_played(p, 3000);
        h.note_seen(p, 4000);
        assert_eq!(h.get(p).unwrap().added, 1000);
        assert_eq!(h.get(p).unwrap().plays, 1);
    }

    /// Zero clears, because "no stars" and "unrated" are the same thing to
    /// anyone typing it — and unrated has to stay distinct from rated-low.
    #[test]
    fn a_rating_of_zero_clears_it() {
        let mut h = History::default();
        let p = Path::new("/m/a.mp3");
        h.note_rating(p, 4, 1);
        assert_eq!(h.get(p).unwrap().rating, Some(4));
        h.note_rating(p, 1, 2);
        assert_eq!(h.get(p).unwrap().rating, Some(1), "one star is not none");
        h.note_rating(p, 0, 3);
        assert_eq!(h.get(p).unwrap().rating, None);
    }

    /// Rating a track that has never been played must not lose its history,
    /// and rating must not disturb the first-seen date.
    #[test]
    fn rating_leaves_the_rest_of_an_entry_alone() {
        let mut h = History::default();
        let p = Path::new("/m/a.mp3");
        h.note_seen(p, 100);
        h.note_played(p, 200);
        h.note_rating(p, 5, 300);
        let e = h.get(p).unwrap();
        assert_eq!(
            (e.added, e.plays, e.last_played, e.rating),
            (100, 1, Some(200), Some(5))
        );
    }

    #[test]
    fn a_round_trip_keeps_everything() {
        let path = tmpdir("roundtrip").join("history.toml");
        let mut h = History::default();
        h.note_seen(Path::new("/m/a.mp3"), 10);
        h.note_played(Path::new("/m/b with spaces.mp3"), 20);
        h.note_rating(Path::new("/m/b with spaces.mp3"), 4, 20);
        h.note_played(Path::new("/m/Löffler — “quoted”.mp3"), 30);
        h.save(&path).unwrap();

        let back = History::load(&path);
        assert_eq!(back.len(), 3);
        assert_eq!(back.get(Path::new("/m/a.mp3")).unwrap().added, 10);
        assert_eq!(
            back.get(Path::new("/m/b with spaces.mp3")).unwrap().rating,
            Some(4)
        );
        assert_eq!(
            back.get(Path::new("/m/Löffler — “quoted”.mp3"))
                .unwrap()
                .plays,
            1,
            "a path with quotes and dashes must survive being a table key"
        );
    }

    #[test]
    fn a_missing_or_corrupt_file_starts_empty() {
        let d = tmpdir("bad");
        assert!(History::load(&d.join("nothing.toml")).is_empty());
        let junk = d.join("junk.toml");
        std::fs::write(&junk, "not toml {{{").unwrap();
        assert!(History::load(&junk).is_empty());
    }

    /// The one failure this store must not have: replacing good counts with a
    /// broken file. Temp-then-rename is what prevents it.
    #[test]
    fn a_failed_write_leaves_the_previous_history_intact() {
        let d = tmpdir("atomic");
        let path = d.join("history.toml");
        let mut h = History::default();
        h.note_played(Path::new("/m/a.mp3"), 1);
        h.save(&path).unwrap();

        std::fs::create_dir(path.with_extension("toml.tmp")).unwrap();
        let mut next = h.clone();
        next.note_played(Path::new("/m/a.mp3"), 2);
        assert!(next.save(&path).is_err());

        assert_eq!(
            History::load(&path)
                .get(Path::new("/m/a.mp3"))
                .unwrap()
                .plays,
            1
        );
    }

    #[test]
    fn a_file_from_another_version_is_ignored() {
        let path = tmpdir("version").join("history.toml");
        let mut h = History::default();
        h.note_played(Path::new("/m/a.mp3"), 1);
        h.save(&path).unwrap();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace(&format!("version = {VERSION}"), "version = 999");
        std::fs::write(&path, text).unwrap();
        assert!(History::load(&path).is_empty());
    }
}
