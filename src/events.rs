//! The old path-keyed event log, kept **only to be read** — issue #82.
//!
//! This was the record of what you did, and it is not any more: `model::log`
//! is, keyed by identity rather than by path. What survives here is the
//! decoder, because the log on a real disk is the only copy of six thousand
//! events and `--migrate-log` has to be able to read it.
//!
//! ```text
//! <data_dir>/events-<host>.log     appended by a version that no longer runs
//! ```
//!
//! **Nothing writes this format any more.** The append side, the fold, the
//! sequence counter and the error type are gone with it. When the last machine
//! has migrated, this file goes too.
//!
//! `machine_id` is the one thing here that was never about events: it names
//! *this device*, and the identity-keyed log derives its `Id<Device>` from it.

use std::path::{Path, PathBuf};

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
/// Every event in a directory's logs, in the order they were written.
///
/// Exposed so a migration can read the log without folding it into a history
/// keyed by the very thing the migration exists to stop using (#82).
pub fn events_in(data_dir: &Path) -> Vec<Event> {
    let mut events: Vec<Event> = Vec::new();
    for log in logs_in(data_dir) {
        let Ok(text) = std::fs::read_to_string(&log) else {
            continue;
        };
        events.extend(text.lines().filter_map(decode));
    }
    events.sort_by_key(|e| e.at);
    events
}

/// Turn a whole `Entry` into the events that would produce it.
///
/// Used once, to seed a log from a pre-log history. Deliberately **not** used
/// at startup: the base file is the base, and re-emitting it every launch
/// would double every play count.
#[allow(dead_code)]
#[cfg(test)]
mod tests {
    use super::*;

    /// The read side is all that is left, and it is what a migration needs.
    #[test]
    fn a_line_decodes_to_what_wrote_it() {
        let dir = std::env::temp_dir().join("gmuse-events-read");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("events-a.log"),
            "1788115290\trated\t/m/x.mp3\t4\ta\t1\n\
             1788115291\tplayed\t/m/x.mp3\ta\t2\n\
             1788115292\tseen\t/m/y.mp3\ta\t3\n",
        )
        .unwrap();

        let events = events_in(&dir);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0].kind, Kind::Rated(_, Some(4))));
        assert!(matches!(events[1].kind, Kind::Played(_)));
        assert!(matches!(events[2].kind, Kind::Seen(_)));
    }

    /// A line written before ids existed has no `device:seq`, so it falls back
    /// to a hash of itself — the dedup those lines have always had.
    #[test]
    fn a_legacy_line_still_has_an_id() {
        let legacy = Event {
            at: 1,
            device: String::new(),
            seq: 0,
            kind: Kind::Played("/m/x.mp3".into()),
        };
        assert!(legacy.id().starts_with("legacy:"));
        assert_eq!(legacy.id(), legacy.id(), "and it is stable");
    }

    /// Only this format's logs, so a data directory's other files are not fed
    /// to the decoder line by line.
    #[test]
    fn logs_in_finds_only_event_logs() {
        let dir = std::env::temp_dir().join("gmuse-events-finding");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events-a.log"), "").unwrap();
        std::fs::write(dir.join("history.toml"), "").unwrap();
        std::fs::write(dir.join("log-b.jsonl"), "").unwrap();
        assert_eq!(logs_in(&dir).len(), 1);
    }
}
