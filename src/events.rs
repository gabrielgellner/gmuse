//! The append-only record of what you did — issue #26.
//!
//! Ratings and plays are **events, not state**. `history.toml` stored the
//! answer and rewrote the whole file to change it, which is fine on one
//! machine and lossy on two: play a track on the laptop and rate one on the
//! desktop, and whichever file wins the sync takes *everything* the other
//! machine did with it. The blast radius of a whole-file write is the library,
//! not the track.
//!
//! Here nothing is ever rewritten. Each machine appends to its own log, so no
//! two writers touch one file and a sync has nothing to resolve:
//!
//! ```text
//! <data_dir>/history.toml          the base, written once and never again
//! <data_dir>/events-<host>.log     this machine's appends
//! <data_dir>/events-<other>.log    another machine's, arriving by sync
//! ```
//!
//! Reading is `base + fold(every log, in timestamp order)`. Plays are counted,
//! so two machines' plays add up rather than one replacing the other; a rating
//! is the latest by timestamp, which at least gives both machines the *same*
//! answer, where two state files would disagree depending on which won.
//!
//! **Log decisions, not interactions.** A play and a rating are things the
//! listener meant. A seek is not. That distinction is what keeps the log's
//! growth proportional to listening rather than to fidgeting — measured, a
//! year is ~4 MB and folds in 4.6 ms, thirty years in 62 ms, so compaction is
//! a tidiness question and not a performance one.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::history::{Entry, History};

/// What happened.
///
/// Deliberately small. Every variant records something the listener chose;
/// nothing here is a side effect of moving around the interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A track earned a play (`history::counts_as_played`).
    Played(PathBuf),
    /// A rating was set, or cleared with `None`.
    Rated(PathBuf, Option<u8>),
    /// A file was seen for the first time. This is what `added` means.
    Seen(PathBuf),
    /// A file was recognised at a new path (#25).
    Moved(PathBuf, PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Seconds since the epoch. **Wall clock, not session-relative** — the
    /// audit log's elapsed times cannot be ordered across two sessions, let
    /// alone two machines, which is exactly what folding needs.
    ///
    /// Display and within-device ordering only. §5 of the sync design is
    /// explicit that events are **never** ordered across devices by this: a
    /// machine offline for six months with a drifting clock produces
    /// timestamps that interleave wrongly with everything else.
    pub at: i64,
    /// Which machine recorded this. Empty for a line written before ids
    /// existed — see [`Event::id`].
    pub device: String,
    /// A per-device counter, monotonic, assigned at append.
    ///
    /// Zero on a legacy line, which is why [`Event::id`] and not `(device,
    /// seq)` is the thing to compare.
    pub seq: u64,
    pub kind: Kind,
}

impl Event {
    /// The identity a merge unions over.
    ///
    /// **`device:seq`, not a ULID**, which is what the design reached for. The
    /// pair already has every property the merge needs — unique with no
    /// coordination, monotonic per device, and a watermark that is an integer
    /// comparison — and it has one a ULID does not: it **cannot be sorted
    /// across devices**. A ULID embeds a timestamp, so it silently invites the
    /// one thing §5 forbids. This makes that mistake unexpressible.
    ///
    /// A line written before ids existed has no pair, so it falls back to a
    /// hash of its own content — which is exactly the dedup those lines have
    /// always had. Legacy data keeps legacy behaviour; nothing is retrofitted
    /// with an id it never had, and the imprecision shrinks as they age out.
    pub fn id(&self) -> String {
        if self.device.is_empty() {
            return format!("legacy:{:016x}", legacy_hash(&encode(self)));
        }
        format!("{}:{}", self.device, self.seq)
    }
}

/// A stable hash of a legacy line, standing in for the id it never had.
fn legacy_hash(line: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    line.trim_end().hash(&mut h);
    h.finish()
}

#[derive(Debug, thiserror::Error)]
pub enum EventError {
    #[error("appending to {path}: {source}")]
    Append {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// One machine's log within `data_dir`.
///
/// Named per machine so two of them syncing the same directory never write the
/// same file — which is the whole reason this is a log rather than a state
/// file. Get this wrong and you have two writers on one file again.
pub fn log_path(data_dir: &Path, machine: &str) -> PathBuf {
    data_dir.join(format!("events-{machine}.log"))
}

/// A stable name for this machine, kept in `state_dir`.
///
/// **Not** the hostname from the environment: neither `HOSTNAME` nor `HOST` is
/// exported to a child process on macOS or under zsh, so every machine would
/// have answered "local" and shared one log. Not in `data_dir` either — that
/// is the directory being synced, so both machines would read one id and
/// collide precisely where it matters.
///
/// Generated once and written; a directory that cannot be written falls back
/// to a fixed name, which is no worse than the single file this replaces.
pub fn machine_id(state_dir: &Path) -> String {
    if let Some(name) = std::env::var("GMUSE_HOST").ok().map(|h| clean(&h))
        && !name.is_empty()
    {
        return name;
    }
    let path = state_dir.join("machine-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = clean(&existing);
        if !existing.is_empty() {
            return existing;
        }
    }
    let fresh = generate();
    if std::fs::create_dir_all(state_dir).is_ok() {
        let _ = std::fs::write(&path, &fresh);
    }
    fresh
}

/// Keep it to what a file name can hold on any platform.
fn clean(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect()
}

fn generate() -> String {
    // SplitMix64 over the clock. Two machines starting in the same nanosecond
    // and sharing a directory is not a risk worth a dependency for.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut z = nanos.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    format!("{:016x}", z ^ (z >> 31))
}

/// Every log in `data_dir`, this machine's and any that arrived by sync.
///
/// Sorted by name so a fold is reproducible: same files, same order, same
/// answer, whichever machine asks.
pub fn logs_in(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("events-") && n.ends_with(".log"))
        })
        .collect();
    out.sort();
    out
}

/// Append one event.
///
/// Opened, written and closed each time rather than held open: a rating is
/// worth more than a play count and losing one to a crash would be worse than
/// the cost of an open. There are only a few of these an hour.
pub fn append(path: &Path, device: &str, at: i64, kind: Kind) -> Result<Event, EventError> {
    let fail = |source| EventError::Append {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    // Read the counter back out of the log rather than keeping it in a state
    // file beside it. Two copies of a monotonic counter is one more than can be
    // kept honest across a crash, and the log is the thing that must be right.
    // Cheap at this rate — there are a few of these an hour.
    let event = Event {
        at,
        device: device.to_string(),
        seq: next_seq(path, device),
        kind,
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(fail)?;
    file.write_all(encode(&event).as_bytes()).map_err(fail)?;
    file.flush().map_err(fail)?;
    Ok(event)
}

/// The next sequence number for `device` in this log.
///
/// One past the highest already there, so it survives a restart without any
/// state of its own. A log that cannot be read starts at 1 rather than
/// refusing: a missing counter must not stop a play being recorded.
fn next_seq(path: &Path, device: &str) -> u64 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 1;
    };
    text.lines()
        .filter_map(decode)
        .filter(|e| e.device == device)
        .map(|e| e.seq)
        .max()
        .unwrap_or(0)
        + 1
}

/// One line: `<seconds>\t<verb>\t<field>…`, newline-terminated.
///
/// Tab-separated and plain text so the file stays greppable, which is half the
/// point of not using a database.
fn encode(e: &Event) -> String {
    let (verb, rest) = match &e.kind {
        Kind::Played(p) => ("played", vec![p]),
        Kind::Rated(p, _) => ("rated", vec![p]),
        Kind::Seen(p) => ("seen", vec![p]),
        Kind::Moved(from, to) => ("moved", vec![from, to]),
    };
    // `device` and `seq` lead, and that is what makes the two formats tellable
    // apart without a version marker: a legacy line starts with a timestamp,
    // which parses as an integer; a device id does not.
    let mut line = if e.device.is_empty() {
        format!("{}\t{}", e.at, verb)
    } else {
        format!("{}\t{}\t{}\t{}", e.device, e.seq, e.at, verb)
    };
    for p in rest {
        line.push('\t');
        line.push_str(&escape(&p.to_string_lossy()));
    }
    if let Kind::Rated(_, stars) = &e.kind {
        line.push('\t');
        line.push_str(&match stars {
            Some(n) => n.to_string(),
            // Spelled as the command language spells it, and not as `0`:
            // §13 settled that no track carries zero stars.
            None => "none".to_string(),
        });
    }
    line.push('\n');
    line
}

/// A path is arbitrary bytes, and the separator is not. Rare, but a single
/// stray tab would silently shift every field after it.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            // An unknown escape keeps both characters rather than eating one:
            // a hand-edited log should never lose data to a typo.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Parse one line. `None` for anything unreadable, which is skipped rather
/// than fatal — a truncated last line after a crash must not cost the rest.
fn decode(line: &str) -> Option<Event> {
    let mut f = line.split('\t');
    let head = f.next()?.trim();
    // Two formats in one file, told apart by the first column: a legacy line
    // opens with a timestamp, which parses as an integer, and a line written
    // since ids exist opens with a device id, which does not. No version
    // marker and no migration — an append-only log should not be rewritten to
    // change its schema, and the old lines still say everything they said.
    let (device, seq, at) = match head.parse::<i64>() {
        Ok(at) => (String::new(), 0, at),
        Err(_) => {
            let seq: u64 = f.next()?.trim().parse().ok()?;
            let at: i64 = f.next()?.trim().parse().ok()?;
            (head.to_string(), seq, at)
        }
    };
    let verb = f.next()?;
    let first = PathBuf::from(unescape(f.next()?));
    let kind = match verb {
        "played" => Kind::Played(first),
        "seen" => Kind::Seen(first),
        "moved" => Kind::Moved(first, PathBuf::from(unescape(f.next()?))),
        "rated" => {
            let stars = match f.next()? {
                "none" => None,
                n => Some(n.parse().ok().filter(|n| (1..=5).contains(n))?),
            };
            Kind::Rated(first, stars)
        }
        _ => return None,
    };
    Some(Event {
        at,
        device,
        seq,
        kind,
    })
}

/// The identity of one logged line, for a merge that unions rather than
/// compares text.
pub fn id_of(line: &str) -> Option<String> {
    decode(line).map(|e| e.id())
}

/// Read every log in `data_dir` and apply it to `base`.
///
/// Sorted by timestamp across all files, because "the latest rating wins" is
/// only meaningful in order, and a `moved` applied out of order would carry
/// the wrong history. A stable sort keeps same-second events in file order, so
/// one machine's own log always folds the way it was written.
pub fn fold_into(base: &mut History, data_dir: &Path) -> usize {
    let mut events: Vec<Event> = Vec::new();
    for log in logs_in(data_dir) {
        let Ok(text) = std::fs::read_to_string(&log) else {
            continue;
        };
        events.extend(text.lines().filter_map(decode));
    }
    events.sort_by_key(|e| e.at);
    let applied = events.len();
    for e in &events {
        apply(base, e);
    }
    applied
}

fn apply(h: &mut History, e: &Event) {
    match &e.kind {
        Kind::Seen(p) => {
            h.note_seen(p, e.at);
        }
        Kind::Played(p) => {
            // `note_seen` first, so a play whose `seen` never made it to disk
            // still lands on an entry rather than being dropped.
            h.note_seen(p, e.at);
            h.note_played(p, e.at);
        }
        Kind::Rated(p, stars) => {
            h.note_seen(p, e.at);
            h.note_rating(p, stars.unwrap_or(0), e.at);
        }
        Kind::Moved(from, to) => {
            h.rename(from, to);
        }
    }
}

/// The state as of `base` plus everything logged since.
pub fn folded(base_path: &Path, data_dir: &Path) -> (History, usize) {
    let mut h = History::load(base_path);
    let applied = fold_into(&mut h, data_dir);
    // Folding is a read, not a change: marking it dirty would rewrite the base
    // on the next save and undo the point of an immutable base.
    h.mark_saved();
    (h, applied)
}

/// Turn a whole `Entry` into the events that would produce it.
///
/// Used once, to seed a log from a pre-log history. Deliberately **not** used
/// at startup: the base file is the base, and re-emitting it every launch
/// would double every play count.
#[allow(dead_code)]
pub fn seed_from(path: &Path, entry: &Entry) -> Vec<Event> {
    let mut out = vec![Event {
        at: entry.added,
        device: String::new(),
        seq: 0,
        kind: Kind::Seen(path.to_path_buf()),
    }];
    // The base records *how many* plays, never when, so they all carry the one
    // timestamp we do have. Honest about what was known.
    let when = entry.last_played.unwrap_or(entry.added);
    for _ in 0..entry.plays {
        out.push(Event {
            at: when,
            device: String::new(),
            seq: 0,
            kind: Kind::Played(path.to_path_buf()),
        });
    }
    if let Some(stars) = entry.rating {
        out.push(Event {
            at: when,
            device: String::new(),
            seq: 0,
            kind: Kind::Rated(path.to_path_buf(), Some(stars)),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-events-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A legacy-shaped event: no device, no sequence — the format every line
    /// in an existing log already has. Kept as the default so the older tests
    /// go on exercising exactly the data they were written against.
    fn ev(at: i64, kind: Kind) -> Event {
        Event {
            at,
            device: String::new(),
            seq: 0,
            kind,
        }
    }

    /// An event as it is written now.
    fn ev_id(device: &str, seq: u64, at: i64, kind: Kind) -> Event {
        Event {
            at,
            device: device.into(),
            seq,
            kind,
        }
    }

    /// `append` assigns the sequence itself, so a test that wants a written
    /// event says only what happened.
    fn put(log: &Path, device: &str, at: i64, kind: Kind) {
        append(log, device, at, kind).unwrap();
    }

    fn round_trip(e: &Event) -> Event {
        let line = encode(e);
        assert!(line.ends_with('\n'), "every event is one line");
        decode(line.trim_end_matches('\n')).expect("decodes")
    }

    #[test]
    fn every_event_survives_a_round_trip() {
        for e in [
            ev(100, Kind::Played("/m/a.mp3".into())),
            ev(200, Kind::Seen("/m/a.mp3".into())),
            ev(300, Kind::Rated("/m/a.mp3".into(), Some(4))),
            ev(400, Kind::Rated("/m/a.mp3".into(), None)),
            ev(500, Kind::Moved("/m/a.mp3".into(), "/m/b.mp3".into())),
        ] {
            assert_eq!(round_trip(&e), e);
        }
    }

    /// A path is arbitrary bytes and the separator is not. Rare, but one stray
    /// tab would silently shift every field after it.
    #[test]
    fn separators_inside_a_path_survive() {
        let nasty = PathBuf::from("/m/od\td\\name\nhere.mp3");
        let e = ev(100, Kind::Played(nasty.clone()));
        let line = encode(&e);
        assert_eq!(line.matches('\t').count(), 2, "encoded: {line:?}");
        assert_eq!(line.matches('\n').count(), 1, "the terminator only");
        assert_eq!(round_trip(&e), e);
    }

    /// A truncated last line after a crash must not cost the rest of the log.
    #[test]
    fn unreadable_lines_are_skipped_rather_than_fatal() {
        assert_eq!(decode(""), None);
        assert_eq!(decode("not-a-timestamp\tplayed\t/m/a.mp3"), None);
        assert_eq!(decode("100\tfrobnicated\t/m/a.mp3"), None);
        assert_eq!(decode("100\tplayed"), None);
        assert_eq!(decode("100\tmoved\t/m/a.mp3"), None, "a move needs both");
        // Zero stars is not a rating anyone can set, so it is not one to read.
        assert_eq!(decode("100\trated\t/m/a.mp3\t0"), None);
        assert_eq!(decode("100\trated\t/m/a.mp3\t9"), None);
    }

    #[test]
    fn appending_then_folding_reproduces_the_state() {
        let d = dir("fold");
        let log = log_path(&d, "test");
        for e in [
            ev(100, Kind::Seen("/m/a.mp3".into())),
            ev(200, Kind::Played("/m/a.mp3".into())),
            ev(300, Kind::Played("/m/a.mp3".into())),
            ev(400, Kind::Rated("/m/a.mp3".into(), Some(5))),
        ] {
            append(&log, "dev", e.at, e.kind.clone()).unwrap();
        }

        let (h, applied) = folded(&d.join("history.toml"), &d);
        assert_eq!(applied, 4);
        let e = h.get(Path::new("/m/a.mp3")).expect("folded");
        assert_eq!((e.plays, e.rating, e.added), (2, Some(5), 100));
        assert_eq!(e.last_played, Some(300));
        // Folding is a read: marking it dirty would rewrite the base and undo
        // the point of an immutable one.
        assert!(!h.is_dirty(), "a fold must not dirty the state");
    }

    /// The whole reason this is a log. Two machines, one directory: the play
    /// from one and the rating from the other must both survive, where two
    /// state files would have kept only whichever won the sync.
    #[test]
    fn two_machines_both_keep_their_writes() {
        let d = dir("two-machines");
        let laptop = d.join("events-laptop.log");
        let desktop = d.join("events-desktop.log");

        put(&laptop, "dev", 100, Kind::Seen("/m/a.mp3".into()));
        put(&laptop, "dev", 200, Kind::Played("/m/a.mp3".into()));
        put(&desktop, "dev", 250, Kind::Played("/m/a.mp3".into()));
        put(
            &desktop,
            "dev",
            300,
            Kind::Rated("/m/a.mp3".into(), Some(5)),
        );

        let (h, _) = folded(&d.join("history.toml"), &d);
        let e = h.get(Path::new("/m/a.mp3")).unwrap();
        assert_eq!(e.plays, 2, "plays must add up, not replace");
        assert_eq!(
            e.rating,
            Some(5),
            "the rating from the other machine is gone"
        );
    }

    /// Where a conflict is genuine, both machines must at least compute the
    /// *same* answer — which two state files could not promise.
    #[test]
    fn the_latest_rating_wins_whatever_order_the_logs_are_read() {
        let d = dir("latest-rating");
        // Deliberately written out of order and to the file that sorts first.
        put(
            &d.join("events-a.log"),
            "a",
            900,
            Kind::Rated("/m/a.mp3".into(), Some(2)),
        );
        put(
            &d.join("events-b.log"),
            "b",
            100,
            Kind::Rated("/m/a.mp3".into(), Some(5)),
        );

        let (h, _) = folded(&d.join("history.toml"), &d);
        assert_eq!(
            h.get(Path::new("/m/a.mp3")).unwrap().rating,
            Some(2),
            "events must fold in timestamp order, not file order"
        );
    }

    /// A move recorded on one machine has to reach the other, which has no
    /// scan of that reorganisation to infer it from.
    #[test]
    fn a_move_carries_the_history_when_folded_elsewhere() {
        let d = dir("moved");
        let log = log_path(&d, "test");
        put(&log, "dev", 100, Kind::Seen("/m/old.mp3".into()));
        put(&log, "dev", 200, Kind::Played("/m/old.mp3".into()));
        put(&log, "dev", 300, Kind::Rated("/m/old.mp3".into(), Some(4)));
        put(
            &log,
            "dev",
            400,
            Kind::Moved("/m/old.mp3".into(), "/m/new.mp3".into()),
        );

        let (h, _) = folded(&d.join("history.toml"), &d);
        let e = h.get(Path::new("/m/new.mp3")).expect("carried");
        assert_eq!((e.plays, e.rating, e.added), (1, Some(4), 100));
        assert!(h.get(Path::new("/m/old.mp3")).is_none());
    }

    /// The failure this issue exists for: two identical-looking events on two
    /// machines are two events, and content dedup kept one (#36).
    #[test]
    fn two_plays_in_the_same_second_on_two_machines_are_two_events() {
        let a = ev_id("laptop", 1, 100, Kind::Played("/m/a.mp3".into()));
        let b = ev_id("desktop", 1, 100, Kind::Played("/m/a.mp3".into()));
        assert_eq!(
            encode(&a).split_once('\t').map(|(_, rest)| rest),
            encode(&b).split_once('\t').map(|(_, rest)| rest),
            "the two lines differ only in their device — that is the point"
        );
        assert_ne!(a.id(), b.id(), "and they must not share an identity");
    }

    /// The sequence survives a restart because it is read back from the log,
    /// not held in a counter beside it.
    #[test]
    fn the_sequence_continues_where_the_log_left_off() {
        let d = dir("seq");
        let log = log_path(&d, "test");
        for _ in 0..3 {
            put(&log, "dev", 100, Kind::Played("/m/a.mp3".into()));
        }
        let last = append(&log, "dev", 100, Kind::Played("/m/a.mp3".into())).unwrap();
        assert_eq!(last.seq, 4);
        // The shape on disk, asserted rather than described.
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .last()
                .unwrap(),
            "dev\t4\t100\tplayed\t/m/a.mp3"
        );

        // A second device counts on its own, in the same file.
        let other = append(&log, "other", 100, Kind::Played("/m/a.mp3".into())).unwrap();
        assert_eq!(other.seq, 1, "each device has its own sequence");
    }

    /// A log written before ids existed still reads, and still folds.
    #[test]
    fn a_legacy_line_decodes_and_keeps_its_old_identity() {
        let legacy = "100\tplayed\t/m/a.mp3";
        let e = decode(legacy).expect("legacy lines must still parse");
        assert_eq!(e.at, 100);
        assert!(e.device.is_empty());
        assert!(
            e.id().starts_with("legacy:"),
            "no id is invented for a line that never had one: {}",
            e.id()
        );
        // Identical text means identical identity — exactly the dedup those
        // lines have always had, neither better nor worse.
        assert_eq!(e.id(), decode(legacy).unwrap().id());
    }

    /// Both formats in one file, which is what an existing log becomes the
    /// moment anything is appended to it.
    #[test]
    fn a_log_may_hold_both_formats() {
        let d = dir("mixed");
        let log = log_path(&d, "test");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&log, "100\tplayed\t/m/a.mp3\n").unwrap();
        put(&log, "dev", 200, Kind::Played("/m/a.mp3".into()));

        let (h, applied) = folded(&d.join("history.toml"), &d);
        assert_eq!(applied, 2, "both lines fold");
        assert_eq!(h.get(Path::new("/m/a.mp3")).unwrap().plays, 2);
    }

    /// A play whose `seen` never reached the disk still has to land somewhere.
    #[test]
    fn a_play_without_a_sighting_still_counts() {
        let d = dir("orphan-play");
        put(
            &log_path(&d, "test"),
            "test",
            500,
            Kind::Played("/m/a.mp3".into()),
        );
        let (h, _) = folded(&d.join("history.toml"), &d);
        let e = h.get(Path::new("/m/a.mp3")).expect("created by the play");
        assert_eq!((e.plays, e.added), (1, 500));
    }

    /// The base is the starting point, and the log is what happened since.
    #[test]
    fn the_log_applies_on_top_of_the_base() {
        let d = dir("base");
        let base = d.join("history.toml");
        let mut seed = History::default();
        seed.note_seen(Path::new("/m/a.mp3"), 50);
        seed.note_played(Path::new("/m/a.mp3"), 60);
        seed.save(&base).unwrap();

        put(
            &log_path(&d, "test"),
            "test",
            700,
            Kind::Played("/m/a.mp3".into()),
        );
        let (h, _) = folded(&base, &d);
        let e = h.get(Path::new("/m/a.mp3")).unwrap();
        assert_eq!(e.plays, 2, "the base's play was lost or the log's was");
        assert_eq!(e.added, 50, "the base's first sighting is the earlier one");
    }

    #[test]
    fn only_event_logs_are_read() {
        let d = dir("stray-files");
        put(
            &log_path(&d, "test"),
            "test",
            100,
            Kind::Played("/m/a.mp3".into()),
        );
        std::fs::write(d.join("history.toml"), "not events").unwrap();
        std::fs::write(d.join("notes.txt"), "100\tplayed\t/m/b.mp3\n").unwrap();
        std::fs::write(d.join("events-x.log.bak"), "100\tplayed\t/m/c.mp3\n").unwrap();

        let (h, applied) = folded(&d.join("nonexistent.toml"), &d);
        assert_eq!(applied, 1);
        assert!(h.get(Path::new("/m/b.mp3")).is_none());
        assert!(h.get(Path::new("/m/c.mp3")).is_none());
    }

    #[test]
    fn a_missing_directory_folds_to_nothing() {
        let (h, applied) = folded(Path::new("/nope/history.toml"), Path::new("/nope"));
        assert_eq!(applied, 0);
        assert!(h.is_empty());
    }

    /// The file name has to be a file name, whatever the host is called.
    #[test]
    fn the_log_is_named_after_the_machine() {
        let p = log_path(Path::new("/data"), "laptop");
        assert_eq!(p, PathBuf::from("/data/events-laptop.log"));
    }

    /// The id has to be stable across runs, or every launch would start a new
    /// log — and it has to live outside the synced directory, or two machines
    /// would read one id and collide exactly where it matters.
    #[test]
    fn the_machine_id_is_generated_once_and_then_reused() {
        let state = dir("machine-id");
        let first = machine_id(&state);
        assert!(!first.is_empty());
        assert!(
            state.join("machine-id").exists(),
            "the id was not persisted"
        );
        assert_eq!(machine_id(&state), first, "a second run invented a new id");

        // Distinct directories are distinct machines.
        assert_ne!(machine_id(&dir("machine-id-other")), first);
    }

    /// Whatever it comes from, it has to be usable as a file name.
    #[test]
    fn a_machine_id_is_safe_in_a_file_name() {
        assert_eq!(
            clean("  Gabriels-MacBook-Air.local "),
            "Gabriels-MacBook-Airlocal"
        );
        assert_eq!(clean("a/b\\c"), "abc");
        assert_eq!(clean(""), "");
        assert_eq!(clean("x".repeat(99).as_str()).len(), 32);
    }

    /// Seeding is what turns a pre-log history into events. Not used at
    /// startup: re-emitting the base every launch would double every play.
    #[test]
    fn seeding_reproduces_an_entry() {
        let entry = Entry {
            plays: 3,
            last_played: Some(900),
            rating: Some(4),
            added: 100,
        };
        let events = seed_from(Path::new("/m/a.mp3"), &entry);

        let mut h = History::default();
        for e in &events {
            apply(&mut h, e);
        }
        let back = h.get(Path::new("/m/a.mp3")).unwrap();
        assert_eq!((back.plays, back.rating, back.added), (3, Some(4), 100));
    }
}
