//! Turning the old log into the new one — issue #82.
//!
//! Two append-only logs both claiming to be the truth is one too many.
//! `events.rs` records what happened against a **path**; `model::log` records
//! it against an **identity**. This converts the first into the second.
//!
//! ## Nothing is destroyed
//!
//! A pure function over decoded events. It reads and returns; it does not
//! write, delete, or touch the file it came from. The live log stays exactly
//! where it is until someone decides otherwise, which is the only responsible
//! way to treat the one file in this project that cannot be rebuilt.
//!
//! ## Most of it is not history at all
//!
//! Measured on the live log: **6227 of 6573 events are `seen`** — the
//! scanner noting a file exists on this disk. That is local state
//! (`library-sync-design.md` §1), regenerable by rescanning, and meaningless
//! on another machine. It is dropped rather than converted, which is 95% of
//! the log gone and nothing lost.
//!
//! `moved` produces no fact either — history keyed by identity does not need
//! carrying across a rename — but it is **not** thrown away first. It is the
//! only record of where a file went, so it is replayed into a path-rewrite map
//! and used to resolve the events that came before it. A `moved` event is the
//! scar from the wound this migration closes, and the scar is still evidence.

use std::collections::BTreeSet;
use std::path::Path;

use crate::events::{Event, Kind};
use crate::model::clock::{HybridClock, Stamp};
use crate::model::entity::{Device, Recording};
use crate::model::id::Id;
use crate::model::log::{Fact, Record};

/// What a conversion did, and what it could not do.
#[derive(Debug, Default)]
pub struct Report {
    /// Events that became facts.
    pub converted: usize,
    /// `seen` and `moved`, dropped on purpose.
    pub dropped: usize,
    /// Events whose path no resolver could identify — the only lossy case,
    /// and named rather than counted so it can be looked at.
    pub unresolved: Vec<(String, String)>,
}

impl Report {
    /// Whether every event was either converted, deliberately dropped, or
    /// named as unresolvable.
    pub fn balances(&self, total: usize) -> bool {
        total == self.converted + self.dropped + self.unresolved.len()
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} converted, {} dropped as local state, {} unresolved",
            self.converted,
            self.dropped,
            self.unresolved.len()
        )
    }
}

/// Convert a decoded log into records the model can fold.
///
/// `identify` answers what a path is — supplied by the caller because only a
/// scan knows, and because a migration that guessed would be worse than one
/// that reported what it could not place.
///
/// Ordering is preserved by stamping in sequence from one clock. The original
/// wall times ride along, so "played this on Tuesday" survives; what they no
/// longer do is decide the order across devices, which is what
/// `library-sync-design.md` §5 forbids and the old `at` field invited.
pub fn to_records(
    events: &[Event],
    device: Id<Device>,
    identify: impl Fn(&Path) -> Option<Id<Recording>>,
) -> (BTreeSet<Record>, Report) {
    let mut clock = HybridClock::new(device);
    let mut out = BTreeSet::new();
    let mut report = Report::default();
    let moves = rewrites(events);

    // Where a path ended up, following every rename recorded since.
    let resolve = |path: &Path| -> Option<Id<Recording>> {
        if let Some(id) = identify(path) {
            return Some(id);
        }
        let mut at = path.to_path_buf();
        for _ in 0..MAX_MOVES {
            let next = moves.get(&at)?.clone();
            if let Some(id) = identify(&next) {
                return Some(id);
            }
            at = next;
        }
        None
    };

    for event in events {
        let (path, what) = match &event.kind {
            Kind::Played(p) => (p.as_path(), Carried::Play),
            Kind::Rated(p, stars) => (p.as_path(), Carried::Rating(*stars)),
            // Local state, and a scar from the problem being fixed.
            Kind::Seen(_) | Kind::Moved(_, _) => {
                report.dropped += 1;
                continue;
            }
        };

        let Some(recording) = resolve(path) else {
            report
                .unresolved
                .push((path.display().to_string(), what.name()));
            continue;
        };
        let stamp = at(&mut clock, event.at);
        out.insert(Record {
            stamp,
            fact: what.into_fact(recording),
        });
        report.converted += 1;
    }
    (out, report)
}

/// What an old event was actually recording.
///
/// A named two-case enum rather than a boxed closure, so the conversion reads
/// as the two things it is instead of as machinery for being generic over them.
#[derive(Debug, Clone, Copy)]
enum Carried {
    Play,
    Rating(Option<u8>),
}

impl Carried {
    fn into_fact(self, recording: Id<Recording>) -> Fact {
        match self {
            // The old log never recorded which encoding played. §4 wants it and
            // cannot have it retroactively, so it is absent rather than guessed.
            Carried::Play => Fact::RecordingPlayed {
                recording,
                encoding: None,
            },
            Carried::Rating(stars) => Fact::RecordingRated { recording, stars },
        }
    }

    fn name(self) -> String {
        match self {
            Carried::Play => "played".into(),
            Carried::Rating(_) => "rated".into(),
        }
    }
}

/// How far a chain of renames may be followed.
///
/// A file that has moved sixteen times has almost certainly moved in a circle,
/// and a bounded wrong answer beats an unbounded loop — the same rule §3 puts
/// on redirects and §5 on derivation.
const MAX_MOVES: usize = 16;

/// Where every renamed file went, from the `moved` events.
///
/// Built before anything is converted, because a play recorded *before* a
/// rename names the old path and only these events know what became of it.
fn rewrites(events: &[Event]) -> std::collections::HashMap<std::path::PathBuf, std::path::PathBuf> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::Moved(from, to) => Some((from.clone(), to.clone())),
            _ => None,
        })
        .collect()
}

/// Stamp an old event at its own wall time, in order.
///
/// The clock never goes backwards, so a log whose timestamps are out of order
/// — which a hand-edited or clock-skewed one can be — still converts to a
/// strictly increasing sequence rather than to records that shuffle.
fn at(clock: &mut HybridClock, seconds: i64) -> Stamp {
    clock.stamp(seconds.saturating_mul(1_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn event(at: i64, kind: Kind) -> Event {
        Event {
            at,
            device: String::new(),
            seq: 0,
            kind,
        }
    }

    fn rec(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn dev() -> Id<Device> {
        Id::derived(b"laptop")
    }

    fn known(path: &Path) -> Option<Id<Recording>> {
        match path.to_str() {
            Some("/m/a.mp3") => Some(rec(1)),
            Some("/m/b.mp3") => Some(rec(2)),
            _ => None,
        }
    }

    /// **95% of the log is not history.** Dropping it loses nothing, because a
    /// scan regenerates it and it never meant anything on another machine.
    #[test]
    fn scan_observations_are_dropped_not_converted() {
        let events = vec![
            event(100, Kind::Seen(PathBuf::from("/m/a.mp3"))),
            event(200, Kind::Played(PathBuf::from("/m/a.mp3"))),
            event(
                300,
                Kind::Moved(PathBuf::from("/m/a.mp3"), "/m/b.mp3".into()),
            ),
        ];
        let (records, report) = to_records(&events, dev(), known);

        assert_eq!(records.len(), 1, "the play, and nothing else");
        assert_eq!(report.converted, 1);
        assert_eq!(report.dropped, 2, "the seen and the move");
        assert!(report.balances(events.len()), "{report}");
    }

    /// **A `moved` event is evidence, not noise.** A play recorded before a
    /// rename names a path that no longer exists, and the move is the only
    /// thing that knows where it went.
    #[test]
    fn history_from_before_a_rename_still_finds_its_track() {
        let events = vec![
            event(100, Kind::Played(PathBuf::from("/m/old.mp3"))),
            event(
                200,
                Kind::Moved(PathBuf::from("/m/old.mp3"), "/m/a.mp3".into()),
            ),
        ];
        let (records, report) = to_records(&events, dev(), known);

        assert_eq!(report.converted, 1, "the play was not lost to the rename");
        assert!(report.unresolved.is_empty());
        assert_eq!(
            records.iter().next().unwrap().fact,
            Fact::RecordingPlayed {
                recording: rec(1),
                encoding: None
            },
            "and it found the track under its new name"
        );
    }

    /// A file renamed twice is followed both times.
    #[test]
    fn a_chain_of_renames_is_followed_to_the_end() {
        let events = vec![
            event(100, Kind::Played(PathBuf::from("/m/first.mp3"))),
            event(
                200,
                Kind::Moved(PathBuf::from("/m/first.mp3"), "/m/second.mp3".into()),
            ),
            event(
                300,
                Kind::Moved(PathBuf::from("/m/second.mp3"), "/m/a.mp3".into()),
            ),
        ];
        let (_, report) = to_records(&events, dev(), known);
        assert_eq!(report.converted, 1);
        assert!(report.unresolved.is_empty());
    }

    /// A rename that goes in a circle terminates rather than hanging.
    #[test]
    fn a_circular_rename_stops_instead_of_looping() {
        let events = vec![
            event(100, Kind::Played(PathBuf::from("/m/x.mp3"))),
            event(
                200,
                Kind::Moved(PathBuf::from("/m/x.mp3"), "/m/y.mp3".into()),
            ),
            event(
                300,
                Kind::Moved(PathBuf::from("/m/y.mp3"), "/m/x.mp3".into()),
            ),
        ];
        let (_, report) = to_records(&events, dev(), known);
        assert_eq!(report.unresolved.len(), 1, "reported, not spun on");
    }

    /// Plays and ratings survive with their times, which is what makes "played
    /// this on Tuesday" still answerable afterwards.
    #[test]
    fn history_keeps_its_shape_and_its_times() {
        let events = vec![
            event(1_000, Kind::Played(PathBuf::from("/m/a.mp3"))),
            event(2_000, Kind::Rated(PathBuf::from("/m/b.mp3"), Some(4))),
        ];
        let (records, report) = to_records(&events, dev(), known);

        assert_eq!(report.converted, 2);
        let stamps: Vec<_> = records.iter().map(|r| r.stamp.wall).collect();
        assert_eq!(
            stamps,
            [1_000_000, 2_000_000],
            "seconds became milliseconds"
        );

        let facts: Vec<_> = records.iter().map(|r| r.fact.clone()).collect();
        assert!(facts.contains(&Fact::RecordingPlayed {
            recording: rec(1),
            encoding: None
        }));
        assert!(facts.contains(&Fact::RecordingRated {
            recording: rec(2),
            stars: Some(4)
        }));
    }

    /// **The only lossy case, and it is named rather than counted.** A path
    /// nothing can identify is a file that has gone; its history cannot be
    /// re-keyed, and pretending otherwise would invent a recording.
    #[test]
    fn an_unidentifiable_path_is_reported_not_guessed() {
        let events = vec![
            event(100, Kind::Played(PathBuf::from("/m/gone.mp3"))),
            event(200, Kind::Played(PathBuf::from("/m/a.mp3"))),
        ];
        let (records, report) = to_records(&events, dev(), known);

        assert_eq!(records.len(), 1);
        assert_eq!(report.unresolved.len(), 1);
        assert!(report.unresolved[0].0.ends_with("gone.mp3"));
        assert!(report.balances(events.len()), "{report}");
    }

    /// A log whose timestamps run backwards — a hand edit, a clock stepping —
    /// still converts to a strictly increasing sequence rather than to records
    /// that shuffle against each other.
    #[test]
    fn an_out_of_order_log_still_converts_in_order() {
        let events = vec![
            event(3_000, Kind::Played(PathBuf::from("/m/a.mp3"))),
            event(1_000, Kind::Played(PathBuf::from("/m/a.mp3"))),
            event(2_000, Kind::Played(PathBuf::from("/m/a.mp3"))),
        ];
        let (records, _) = to_records(&events, dev(), known);

        assert_eq!(records.len(), 3, "three plays, none collapsed");
        let stamps: Vec<_> = records.iter().map(|r| r.stamp).collect();
        assert!(
            stamps.windows(2).all(|w| w[0] < w[1]),
            "the clock never went backwards"
        );
    }

    /// Two plays of one track at the same second are two plays, not one.
    #[test]
    fn plays_in_the_same_second_are_not_collapsed() {
        let events = vec![
            event(500, Kind::Played(PathBuf::from("/m/a.mp3"))),
            event(500, Kind::Played(PathBuf::from("/m/a.mp3"))),
        ];
        let (records, report) = to_records(&events, dev(), known);
        assert_eq!(report.converted, 2);
        assert_eq!(records.len(), 2, "the counter separates them");
    }

    /// **Against the real log**, because a migration that has only been tried
    /// on fixtures has not been tried.
    ///
    /// Measured 2026-09-02 on 6573 live events: **327 converted, 6230 dropped
    /// as local state, 16 unresolved**.
    ///
    /// Every **rating** resolves — checked separately, and it is the thing
    /// that matters. The 16 are plays of files moved out of the library by
    /// hand, into `~/LowQualityMusic` during the quality quarantine, so no
    /// `moved` event records where they went and nothing can bridge the gap.
    /// They resolve again the moment those files come back (#87).
    ///
    /// Reads only. Run with
    /// `cargo test --release converts_the_real_log -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the real library and its log"]
    fn converts_the_real_log() {
        use crate::model::populate;

        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let (root, data) = (home.join("MusicLibrary"), home.join("MusicLibrary/.gmuse"));
        if !data.exists() {
            return;
        }

        // What each path is, from a scan of every root a file might be under.
        //
        // The quarantine matters: files moved to `~/LowQualityMusic` pending a
        // re-buy still have history, and a migration that only looked at the
        // library would throw it away for exactly the tracks most likely to
        // come back.
        let mut roots = vec![root];
        let quarantine = home.join("LowQualityMusic");
        if quarantine.exists() {
            roots.push(quarantine);
        }

        let mut by_path = std::collections::HashMap::new();
        for file in populate::audio_files(&roots) {
            if let Ok(s) = crate::model::ingest::read(&file) {
                let id = Id::<Recording>::of_basis(
                    &s.identity
                        .clone()
                        .unwrap_or(crate::model::basis::Basis::Audio(s.audio)),
                );
                by_path.insert(file, id);
            }
        }

        let events = crate::events::events_in(&data);

        let (records, report) =
            to_records(&events, Id::derived(b"laptop"), |p| by_path.get(p).copied());
        eprintln!("{} events: {report}", events.len());
        for (path, _) in report.unresolved.iter().take(10) {
            eprintln!("  unresolved: {path}");
        }

        assert!(report.balances(events.len()), "every event accounted for");
        assert_eq!(
            records.len(),
            report.converted,
            "no record lost to a stamp clash"
        );
    }

    /// Clearing a rating is a fact, not an absence.
    #[test]
    fn an_unrating_survives_as_itself() {
        let events = vec![event(100, Kind::Rated(PathBuf::from("/m/a.mp3"), None))];
        let (records, _) = to_records(&events, dev(), known);
        assert_eq!(
            records.iter().next().unwrap().fact,
            Fact::RecordingRated {
                recording: rec(1),
                stars: None
            }
        );
    }
}

/// What a run over a real data directory did.
#[derive(Debug)]
pub struct Outcome {
    /// What the scan had to say, since the conversion is only as good as it.
    pub scan: crate::model::populate::Report,
    pub report: Report,
    /// Entries whose acquisition date was carried over from `history.toml`.
    pub acquired: usize,
    /// Paths the base still names that no file answers to any more.
    pub orphaned: Vec<String>,
    /// What the library's identities rest on, and where it is thin (#89).
    pub census: crate::model::basis::Census,
    /// Distinct artists after the fold. Worth printing beside the census
    /// because minting an artist from its credit *string* silently doubled it.
    pub artists: usize,
    /// Records written. Fewer than `report.converted` on a second run, because
    /// what is already on disk is not written again.
    pub written: usize,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}\nmigrate: {} — {} acquired ({} orphaned), {} written",
            self.scan,
            self.report,
            self.acquired,
            self.orphaned.len(),
            self.written
        )
    }
}

/// Convert this machine's event logs into the identity-keyed log, once.
///
/// **Only history is converted.** The old log's `seen` events are 95% of it
/// and are local state that a scan regenerates; the catalogue — artists,
/// recordings, encodings, holdings — is likewise re-derived by scanning, so
/// writing it here would persist what is already reproducible. What cannot be
/// reproduced is a rating somebody gave and a play that happened, and that is
/// what this carries across.
///
/// **Nothing is deleted.** The old logs stay exactly where they are. Running
/// this again after a scan that identifies more files converts more and
/// re-writes nothing, because what is already on disk is subtracted first.
pub fn run(
    data_dir: &Path,
    roots: &[std::path::PathBuf],
    device_name: &str,
    now: i64,
) -> Result<Outcome, crate::model::durable::LogError> {
    let device = Id::<Device>::derived(device_name.as_bytes());
    let events = crate::events::events_in(data_dir);
    let (scanned_instance, scan) = crate::model::populate::read_library(device_name, roots, now);
    let folded = scanned_instance.catalogue();
    let census = crate::model::basis::Census::of(folded.identified.iter());
    let artists = folded.artist_names.len();

    let (mut records, report) =
        to_records(&events, device, |path| scan.identified.get(path).copied());

    // `history.toml`'s `added` has no event to carry it — it was written into
    // the base and never appended — so it is read straight from the file. It
    // is worth carrying: the thing it records already happened, and cannot be
    // re-derived from anything on disk.
    //
    // Stamped from its own clock, so these sort after the events they came
    // with rather than interleaving with them.
    let base = crate::history::History::load(&crate::history::path_in(data_dir));
    let mut clock = HybridClock::new(device);
    let mut acquired = 0;
    let mut orphaned = Vec::new();
    for (path, entry) in base.by_path() {
        let Some(recording) = scan.identified.get(Path::new(path)).copied() else {
            // A path the base still names and the disk no longer has. Named
            // rather than counted, because "three dates were dropped" is a
            // different thing to be told than which three.
            orphaned.push(path.to_string());
            continue;
        };
        {
            records.insert(Record {
                stamp: at(&mut clock, entry.added),
                fact: Fact::RecordingAcquired {
                    recording,
                    at: entry.added,
                },
            });
            acquired += 1;
        }
    }

    // Subtract what is already there, so a second run is free rather than
    // additive. The set semantics would collapse duplicates on read anyway;
    // this keeps the file from growing by its own size every time.
    let path = crate::model::durable::log_path(data_dir, device);
    let (existing, _) = crate::model::durable::read(&path);
    let fresh: BTreeSet<Record> = records.difference(&existing).cloned().collect();
    let written = crate::model::durable::append(&path, &fresh)?;

    Ok(Outcome {
        scan,
        report,
        acquired,
        orphaned,
        census,
        artists,
        written,
    })
}
