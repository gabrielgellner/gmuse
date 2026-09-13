//! Where the log actually lives — issue #82.
//!
//! `model::log` says what a record *is*; this says where it goes and how it
//! comes back. Until now the model's log existed only in memory, which made
//! `events.rs` the one on disk and therefore the real one whatever the design
//! documents said.
//!
//! ## One file per device, and nothing is ever rewritten
//!
//! ```text
//! <data_dir>/log-<device>.jsonl     this device's records
//! <data_dir>/log-<other>.jsonl      another device's, arriving by sync
//! ```
//!
//! A device appends **only records it authored**, so no two writers ever touch
//! one file and a sync has nothing to resolve — it copies files. Reading is a
//! union over all of them, which is sound because a `Record` is identified by
//! its `Stamp` and a stamp cannot collide across devices (`clock.rs`).
//!
//! That is `events.rs`'s layout, and it is kept deliberately: it was right.
//! What was wrong there is what the records *say* — a path instead of an
//! identity, and `(device, seq)` instead of a clock that advances past what it
//! reads.
//!
//! ## JSONL, and the decision it leaves open
//!
//! One JSON object per line. Greppable, `jq`-able, and appendable without
//! reading — the last of which is what makes an append-only log cheap. #40
//! holds the open question of Parquet and DuckDB against exactly this; nothing
//! here forecloses it, because a columnar store would be built *from* the log
//! rather than instead of it.
//!
//! ## A damaged line is skipped, never fatal
//!
//! A line that does not parse is reported and passed over. Refusing to load
//! the whole log because one line is torn would throw away the one file in
//! this project that cannot be rebuilt — for a fault that, by construction,
//! can only be at the end of a file some process died in the middle of.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::facts::Record;
use crate::model::clock::Stamp;
use crate::model::entity::Device;
use crate::model::id::Id;

/// The prefix every log file shares, so `logs_in` can find them.
const PREFIX: &str = "log-";
const SUFFIX: &str = ".jsonl";

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("appending to {path}: {source}")]
    Append {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// A line that could not be read, kept so it can be looked at.
///
/// Counted *and* named. A count says the log is damaged; the line says what
/// was in it, which is the difference between "something was lost" and knowing
/// whether it mattered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damage {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

/// Where this device appends.
pub fn log_path(data_dir: &Path, device: Id<Device>) -> PathBuf {
    data_dir.join(format!("{PREFIX}{device}{SUFFIX}"))
}

/// Every log in `data_dir`, this device's and any that arrived by sync.
///
/// Sorted, so a fold is reproducible: same files, same order, same result.
/// Nothing downstream depends on the order — a `BTreeSet` does not care — but
/// a reproducible *report* does, and so does anyone reading a diff of two runs.
pub fn logs_in(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(SUFFIX))
        })
        .collect();
    found.sort();
    found
}

/// Read one log, skipping anything that will not parse.
pub fn read(path: &Path) -> (BTreeSet<Record>, Vec<Damage>) {
    let mut records = BTreeSet::new();
    let mut damage = Vec::new();
    let Ok(file) = std::fs::File::open(path) else {
        return (records, damage);
    };
    for (index, line) in std::io::BufReader::new(file).lines().enumerate() {
        let Ok(text) = line else { continue };
        if text.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&text) {
            Ok(record) => {
                records.insert(record);
            }
            Err(_) => damage.push(Damage {
                path: path.to_path_buf(),
                line: index + 1,
                text,
            }),
        }
    }
    (records, damage)
}

/// Every record in `data_dir`, from every device.
pub fn read_all(data_dir: &Path) -> (BTreeSet<Record>, Vec<Damage>) {
    let mut records = BTreeSet::new();
    let mut damage = Vec::new();
    for path in logs_in(data_dir) {
        let (theirs, torn) = read(&path);
        records.extend(theirs);
        damage.extend(torn);
    }
    (records, damage)
}

/// Append records to a log, creating it if this is the first write.
///
/// Takes what to write rather than deciding it: the caller knows which records
/// are its own and which are already on disk, and a function that guessed
/// would have to re-read the file to find out.
pub fn append(path: &Path, records: &BTreeSet<Record>) -> Result<usize, LogError> {
    if records.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LogError::Append {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| LogError::Append {
            path: path.to_path_buf(),
            source,
        })?;

    // One buffer, one write. A record per `write!` would leave a half-written
    // log behind a crash at any line rather than only at the last one.
    let mut buffer = String::new();
    for record in records {
        // A record that cannot be encoded is a bug in `Fact`, not a runtime
        // condition, and writing nothing is better than writing a torn line.
        if let Ok(line) = serde_json::to_string(record) {
            buffer.push_str(&line);
            buffer.push('\n');
        }
    }
    file.write_all(buffer.as_bytes())
        .map_err(|source| LogError::Append {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(records.len())
}

/// **Replace a log with a smaller set of the same meaning** — #201.
///
/// The only operation here that removes anything, and the only one that can
/// lose a library. So it does not edit the log: it writes a new one beside it,
/// **reads that back and checks it says what it should**, and only then swaps
/// it in with a rename, which is atomic on every filesystem this runs on. A
/// crash at any point leaves either the old log or the new one, never a torn
/// one, and never a gap.
///
/// The read-back is not ceremony. `append` is careful to write one buffer so a
/// crash cannot tear a line, but a rewrite can fail in a way an append cannot:
/// a full disk takes the *whole* log rather than the tail of it. Verifying
/// before the swap is what makes the difference between a slow day and a lost
/// library.
///
/// Compaction is the only caller and the only thing that should ever be one.
/// `compact` drops superseded holdings assertions and nothing else — a
/// superseded play still carries a listen.
/// **`pub(crate)`, so one appender is a property of the build** — #249.
///
/// It was `pub`, and that was the hole `commit`'s doc claimed did not exist:
/// `Instance::flush` was locked down so facts could only reach disk through an
/// `Appending`, and this sat beside it wide open — so `shelf compact --write` rewrote
/// every device's log with no lock at all. Compacting is the one operation that
/// *removes*, which makes it the worst possible thing to leave outside the gate.
///
/// [`crate::librarian::rewrite_all`] is the way in, and it takes an `Appending` for the
/// reason it says: a process that may not add to a log certainly may not rewrite it.
pub(crate) fn rewrite(path: &Path, records: &BTreeSet<Record>) -> Result<usize, LogError> {
    let beside = path.with_extension("jsonl.compacting");
    let _ = std::fs::remove_file(&beside);

    // Written through `append` to a fresh path, so there is one encoder and
    // one place that decides what a line looks like.
    let written = append(&beside, records)?;

    // **Read it back before trusting it.** A rewrite that lost records is the
    // failure this whole function is shaped around, and it is cheap to rule
    // out: the set that comes back has to be the set that went in.
    let (back, damage) = read(&beside);
    if !damage.is_empty() || back != *records {
        let _ = std::fs::remove_file(&beside);
        return Err(LogError::Append {
            path: path.to_path_buf(),
            source: std::io::Error::other(format!(
                "the rewritten log did not read back: {} record(s) in, {} out, {} damaged — \
                 the original is untouched",
                records.len(),
                back.len(),
                damage.len(),
            )),
        });
    }

    std::fs::rename(&beside, path).map_err(|source| LogError::Append {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(written)
}

/// The highest stamp `device` wrote, or `None` if it has written nothing.
///
/// This is the watermark an instance flushes from. It works because a hybrid
/// clock's own stamps strictly increase, so "everything of mine above this"
/// is exactly "everything of mine not yet written" — one `Stamp` of state
/// instead of a set of every record ever flushed.
pub fn high_water(records: &BTreeSet<Record>, device: Id<Device>) -> Option<Stamp> {
    records
        .iter()
        .filter(|r| r.stamp.device == device)
        .map(|r| r.stamp)
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Fact;
    use crate::model::clock::HybridClock;
    use crate::model::entity::{Artist, Recording};

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("gmuse-durable-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn device(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn some_fact(n: u128) -> Fact {
        Fact::ArtistNamed {
            artist: Id::<Artist>::from_uuid(uuid::Uuid::from_u128(n)),
            name: format!("artist {n}"),
        }
    }

    /// `n` records authored by `device`, stamped by one clock.
    fn authored(device: Id<Device>, n: u128) -> BTreeSet<Record> {
        let mut clock = HybridClock::new(device);
        (0..n)
            .map(|i| Record {
                stamp: clock.stamp(1_000 + i as i64),
                fact: some_fact(i),
            })
            .collect()
    }

    /// **A rewrite replaces the log and loses nothing** — #201.
    ///
    /// The only operation here that removes anything, so this asserts the two
    /// properties that make it survivable: what is on disk afterwards is
    /// exactly what was handed over, and the scratch file it writes through
    /// does not survive.
    #[test]
    fn a_rewrite_replaces_the_log_with_exactly_what_it_was_given() {
        let dir = dir("rewrite");
        let d = device(7);
        let path = log_path(&dir, d);

        let all = authored(d, 20);
        append(&path, &all).unwrap();
        let full = std::fs::metadata(&path).unwrap().len();

        // Keep five of the twenty, the way compaction keeps one snapshot.
        let keep: BTreeSet<Record> = all.iter().take(5).cloned().collect();
        assert_eq!(rewrite(&path, &keep).unwrap(), 5);

        let (back, damage) = read(&path);
        assert!(damage.is_empty(), "a rewritten log must not be damaged");
        assert_eq!(back, keep, "the log is not what it was handed");
        assert!(
            std::fs::metadata(&path).unwrap().len() < full,
            "the file did not actually shrink"
        );

        // **No scratch file left behind.** It sits beside the log inside the
        // data directory, so one that survives is one a later read could trip
        // over.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("compacting"))
            .collect();
        assert!(leftovers.is_empty(), "scratch left behind: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A rewrite that cannot be written leaves the original alone.**
    ///
    /// The failure this function is shaped around: a rewrite can fail in a way
    /// an append cannot, because a full disk takes the *whole* log rather than
    /// the tail of it. The new log is written beside, verified and only then
    /// renamed over — so a failure anywhere before the rename is a slow day
    /// rather than a lost library.
    #[test]
    fn a_failed_rewrite_does_not_touch_the_log() {
        let dir = dir("rewrite-fails");
        let d = device(8);
        let path = log_path(&dir, d);

        let all = authored(d, 12);
        append(&path, &all).unwrap();
        let before = std::fs::read(&path).unwrap();

        // A directory where the scratch file wants to be, so writing it fails.
        std::fs::create_dir_all(path.with_extension("jsonl.compacting")).unwrap();

        let keep: BTreeSet<Record> = all.iter().take(3).cloned().collect();
        assert!(
            rewrite(&path, &keep).is_err(),
            "it should not have succeeded"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the original log was touched by a rewrite that failed"
        );
        let (back, _) = read(&path);
        assert_eq!(back, all, "and it still says everything it said");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole point: what went to disk is what comes back.
    #[test]
    fn a_log_round_trips() {
        let dir = dir("round-trip");
        let d = device(1);
        let records = authored(d, 5);
        let path = log_path(&dir, d);
        assert_eq!(append(&path, &records).unwrap(), 5);

        let (back, damage) = read(&path);
        assert_eq!(back, records, "every record, unchanged");
        assert!(damage.is_empty());
    }

    /// Sync copies files, so reading has to union them. Without this the
    /// second machine's ratings are on disk and invisible.
    #[test]
    fn two_devices_files_union() {
        let dir = dir("union");
        let (a, b) = (device(1), device(2));
        let mine = authored(a, 3);
        let theirs = authored(b, 4);
        append(&log_path(&dir, a), &mine).unwrap();
        append(&log_path(&dir, b), &theirs).unwrap();

        let (all, damage) = read_all(&dir);
        assert!(damage.is_empty());
        assert_eq!(all.len(), 7, "both devices, nothing dropped");
        assert!(mine.iter().all(|r| all.contains(r)));
        assert!(theirs.iter().all(|r| all.contains(r)));
    }

    /// A torn line loses that line and nothing else. The alternative — refusing
    /// to load — throws away the one file here that cannot be rebuilt.
    #[test]
    fn a_damaged_line_is_reported_and_the_rest_survives() {
        let dir = dir("damaged");
        let d = device(1);
        let path = log_path(&dir, d);
        let records = authored(d, 3);
        append(&path, &records).unwrap();
        // A process died mid-write.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"stamp\":{\"wall\":1,\"cou\n").unwrap();
        drop(f);

        let (back, damage) = read(&path);
        assert_eq!(back, records, "the intact records are all there");
        assert_eq!(damage.len(), 1, "and the torn one is named");
        assert_eq!(damage[0].line, 4);
        assert!(damage[0].text.contains("\"wall\":1"), "kept, not counted");
    }

    /// An empty append must not create a file. Otherwise every launch leaves a
    /// zero-byte log behind for a device that has said nothing.
    #[test]
    fn appending_nothing_writes_nothing() {
        let dir = dir("empty");
        let path = log_path(&dir, device(1));
        assert_eq!(append(&path, &BTreeSet::new()).unwrap(), 0);
        assert!(!path.exists());
        assert!(logs_in(&dir).is_empty());
    }

    /// Append means append: a second write must not disturb the first.
    #[test]
    fn a_second_append_keeps_the_first() {
        let dir = dir("append-only");
        let d = device(1);
        let path = log_path(&dir, d);
        let first = authored(d, 2);
        append(&path, &first).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let mut clock = HybridClock::new(d);
        for _ in 0..2 {
            clock.stamp(1_000);
        }
        let second: BTreeSet<Record> = (0..2)
            .map(|i| Record {
                stamp: clock.stamp(2_000 + i),
                fact: some_fact(100 + i as u128),
            })
            .collect();
        append(&path, &second).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(&before), "the first write is untouched");
        let (back, _) = read(&path);
        assert_eq!(back.len(), 4);
    }

    /// The watermark is per device, or a sync would make this device think it
    /// had already written records it has never seen.
    #[test]
    fn the_watermark_is_this_devices_own_high_stamp() {
        let (a, b) = (device(1), device(2));
        let mut all = authored(a, 3);
        // Another device's records, stamped *later* in wall time.
        let mut clock = HybridClock::new(b);
        for i in 0..3 {
            all.insert(Record {
                stamp: clock.stamp(9_000 + i),
                fact: some_fact(200 + i as u128),
            });
        }

        let mine = high_water(&all, a).expect("this device has written");
        assert_eq!(mine.device, a, "ours, not the newest overall");
        assert!(
            all.iter()
                .filter(|r| r.stamp.device == a)
                .all(|r| r.stamp <= mine),
            "and it is the highest of ours"
        );
        assert!(
            all.iter().any(|r| r.stamp > mine),
            "a later foreign stamp exists, and must not move our watermark"
        );
    }

    /// A device that has written nothing has no watermark, which is what makes
    /// a first flush write everything rather than nothing.
    #[test]
    fn a_silent_device_has_no_watermark() {
        let all = authored(device(1), 3);
        assert_eq!(high_water(&all, device(2)), None);
    }

    /// Only files that are logs. A data directory holds `history.toml`, a
    /// machine id and whatever else, and folding those would be a parse error
    /// per line of every one of them.
    #[test]
    fn logs_in_finds_only_logs() {
        let dir = dir("finding");
        let d = device(1);
        append(&log_path(&dir, d), &authored(d, 1)).unwrap();
        std::fs::write(dir.join("history.toml"), "not a log").unwrap();
        std::fs::write(dir.join("log-something.txt"), "nor this").unwrap();
        std::fs::write(dir.join("notlog-x.jsonl"), "nor this").unwrap();

        let found = logs_in(&dir);
        assert_eq!(found.len(), 1, "one log, three decoys");
        assert_eq!(found[0], log_path(&dir, d));
    }

    /// Reading a directory that does not exist is a first run, not a failure.
    #[test]
    fn a_missing_data_dir_reads_as_empty() {
        let (records, damage) = read_all(Path::new("/nonexistent/gmuse-not-here"));
        assert!(records.is_empty());
        assert!(damage.is_empty());
    }

    /// The record's own content survives, not just its count — a fold reads
    /// the fact, and a round trip that preserved only stamps would pass every
    /// other test here.
    #[test]
    fn the_fact_itself_survives_the_round_trip() {
        let dir = dir("fact");
        let d = device(1);
        let mut clock = HybridClock::new(d);
        let recording = Id::<Recording>::from_uuid(uuid::Uuid::from_u128(7));
        let records: BTreeSet<Record> = [Record {
            stamp: clock.stamp(1_000),
            fact: Fact::RecordingRated {
                recording,
                stars: Some(4),
            },
        }]
        .into();
        let path = log_path(&dir, d);
        append(&path, &records).unwrap();

        let (back, _) = read(&path);
        match &back.iter().next().expect("one record").fact {
            Fact::RecordingRated {
                recording: r,
                stars,
            } => {
                assert_eq!(*r, recording);
                assert_eq!(*stars, Some(4), "four stars, not none and not five");
            }
            other => panic!("wrong fact came back: {other:?}"),
        }
    }
}
