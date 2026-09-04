//! What a tool says to the library — `library-api.md` §5.
//!
//! A tool proposes in bulk, a person reviews the file, and gmuse applies it.
//! One JSON object per line, because facts are heterogeneous: a TSV would need
//! a column union across every kind of thing anyone might ever want to assert,
//! and this project has already been bitten by a TSV whose quoting fought its
//! own content.
//!
//! ```jsonl
//! {"fact":"measured","path":"…/03 Callisto.mp3","cutoff_hz":19800,"analyser":1}
//! {"fact":"genre_assigned","path":"…/03 Callisto.mp3","genre":"Techno","basis":"discogs","confidence":"matched"}
//! ```
//!
//! ## A tool names an identity, not a file
//!
//! A proposal says `"encoding":"…uuid…"`, never `"path":"/m/a.mp3"`. A path is
//! a property of one device's copy: it is stale the moment a file moves, it
//! means nothing on another machine, and keying anything by one is the failure
//! this whole model exists to end.
//!
//! The objection that a tool cannot *compute* an id — `audio_id` is XXH3 over
//! Symphonia's packet boundaries and is not reproducible in Python — is real
//! and answered by not asking it to. A tool does not compute an identity, it
//! **receives** one:
//!
//! ```text
//! gmuse library query … --format jsonl   rows carrying the id and a locator
//!        ↓
//! the tool opens the locator, measures, and proposes against the id
//!        ↓
//! gmuse library apply proposals.jsonl
//! ```
//!
//! So the path appears in query *output*, where it is a thing to open, and
//! never in proposal *input*, where it would be an identifier. An id that no
//! longer names anything is reported, never guessed at.

use std::collections::BTreeSet;

use crate::model::clock::HybridClock;
use crate::model::entity::{Device, Encoding, Recording};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::log::{Fact, Record};
use crate::model::reference::Source;

/// One assertion, as a tool writes it.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum Proposal {
    /// What an analyser found about **an encoding**. `quality.py`'s output.
    ///
    /// The subject is the encoding rather than the recording, because a cutoff
    /// is a fact about particular bytes: two encodings of one performance
    /// measure differently, and that difference is the whole point.
    Measured {
        encoding: Id<Encoding>,
        cutoff_hz: u32,
        analyser: u16,
    },
    /// A genre, and where it came from. The genre pipeline's output — and the
    /// reason `basis` and `confidence` are on the wire rather than implied:
    /// a Discogs guess and a typed answer are the same shape and must not be
    /// the same fact.
    GenreAssigned {
        recording: Id<Recording>,
        genre: String,
        basis: Source,
        confidence: Confidence,
    },
    /// A rating. Included because it is the simplest possible proposal and
    /// therefore the one worth testing the format against.
    Rated {
        recording: Id<Recording>,
        stars: Option<u8>,
    },
}

/// What `query --format jsonl` hands a tool.
///
/// The other half of the contract, and the reason a proposal never carries a
/// path: everything a tool needs to do its work and then say what it found
/// arrives here. The **id** is what it will propose against; the **locator**
/// is what it will open.
///
/// They are deliberately different fields with different lifetimes. The
/// locator is good until the file moves; the id is good forever.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Queried {
    pub recording: Id<Recording>,
    /// The encoding a tool would measure — the canonical one, unless the query
    /// asked for something else.
    pub encoding: Option<Id<Encoding>>,
    /// Where this machine can open it, if it can. Absent for a recording
    /// nothing here holds, which is an ordinary row.
    pub locator: Option<String>,
    pub title: String,
    pub artist: String,
}

/// What a proposal is about — one of the two things a tool may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Encoding(Id<Encoding>),
    Recording(Id<Recording>),
}

impl Proposal {
    pub fn subject(&self) -> Subject {
        match self {
            Proposal::Measured { encoding, .. } => Subject::Encoding(*encoding),
            Proposal::GenreAssigned { recording, .. } | Proposal::Rated { recording, .. } => {
                Subject::Recording(*recording)
            }
        }
    }
}

/// What applying a proposal file did.
#[derive(Debug, Default)]
pub struct Report {
    pub applied: usize,
    /// Lines that were not JSON, or were JSON of the wrong shape. Named with
    /// their line number, because a proposal file is something a person edits
    /// and an editor needs somewhere to jump to.
    pub malformed: Vec<(usize, String)>,
    /// Subjects the library does not hold. A proposal written against a
    /// library that has since changed is stale, not wrong, and saying which
    /// ones is what lets a tool re-query rather than re-measure.
    pub unknown: Vec<Subject>,
}

impl Report {
    pub fn balances(&self, lines: usize) -> bool {
        lines == self.applied + self.malformed.len() + self.unknown.len()
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} applied, {} unknown, {} malformed",
            self.applied,
            self.unknown.len(),
            self.malformed.len()
        )
    }
}

/// Turn a proposal file into records.
///
/// Blank lines and `#` comments are skipped, because a reviewed file is one a
/// person has been editing — and a note beside a row they were unsure about is
/// the point of reviewing it.
///
/// **Tolerated on the way in, never written on the way out.** A comment is not
/// part of JSON Lines, so a file carrying one is refused by `fx`, by `jq`
/// without `-R`, and by every other reader that takes the format at its word.
/// The tools used to write a provenance header and it cost exactly that: the
/// files they produced could not be opened by the things one opens JSONL with.
/// Liberal in what is accepted, strict in what is emitted.
///
/// **Nothing is partially applied.** A malformed line does not stop the others
/// — that would make a typo cost a whole pass — but it is reported, and a
/// caller that wants all-or-nothing checks the report before appending.
pub fn parse(
    text: &str,
    device: Id<Device>,
    now: i64,
    holds: impl Fn(Subject) -> bool,
) -> (BTreeSet<Record>, Report) {
    let mut clock = HybridClock::new(device);
    let mut out = BTreeSet::new();
    let mut report = Report::default();

    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let proposal: Proposal = match serde_json::from_str(line) {
            Ok(p) => p,
            Err(e) => {
                report.malformed.push((n + 1, e.to_string()));
                continue;
            }
        };
        if !holds(proposal.subject()) {
            report.unknown.push(proposal.subject());
            continue;
        }
        out.insert(Record {
            stamp: clock.stamp(now),
            fact: into_fact(proposal),
        });
        report.applied += 1;
    }
    (out, report)
}

fn into_fact(proposal: Proposal) -> Fact {
    match proposal {
        Proposal::Measured {
            encoding,
            cutoff_hz,
            analyser,
        } => Fact::EncodingMeasured {
            encoding,
            cutoff_hz,
            analyser,
        },
        // The genre is named rather than identified, because a tool has no way
        // to mint a concept id and should not: the vocabulary is ours (§2), so
        // the name is resolved against it here.
        Proposal::GenreAssigned {
            recording, genre, ..
        } => Fact::GenreAssigned { recording, genre },
        Proposal::Rated { recording, stars } => Fact::RecordingRated { recording, stars },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> Id<Device> {
        Id::derived(b"laptop")
    }

    fn enc(n: u128) -> Id<Encoding> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn rec(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// A library that holds encodings 1 and 2 and recordings 1 and 2.
    fn held(s: Subject) -> bool {
        match s {
            Subject::Encoding(e) => e == enc(1) || e == enc(2),
            Subject::Recording(r) => r == rec(1) || r == rec(2),
        }
    }

    fn measured(e: u128, hz: u32) -> String {
        format!(
            r#"{{"fact":"measured","encoding":"{}","cutoff_hz":{hz},"analyser":1}}"#,
            enc(e)
        )
    }

    /// The shape `quality.py` writes: a measurement about **an encoding**,
    /// because a cutoff is a fact about particular bytes.
    #[test]
    fn a_measurement_becomes_a_fact() {
        let (records, report) = parse(&measured(1, 15_700), dev(), 100, held);

        assert_eq!(report.applied, 1, "{report}");
        assert_eq!(
            records.iter().next().unwrap().fact,
            Fact::EncodingMeasured {
                encoding: enc(1),
                cutoff_hz: 15_700,
                analyser: 1,
            }
        );
    }

    /// **`basis` and `confidence` are on the wire.** A Discogs guess and a
    /// typed answer are the same shape, and a format that could not tell them
    /// apart would throw away the only thing that makes a review queue
    /// possible.
    #[test]
    fn a_genre_carries_where_it_came_from() {
        let good = format!(
            r#"{{"fact":"genre_assigned","recording":"{}","genre":"Techno","basis":"discogs","confidence":"matched"}}"#,
            rec(1)
        );
        let (_, report) = parse(&good, dev(), 100, held);
        assert_eq!(report.applied, 1, "{report}");

        let bare = format!(
            r#"{{"fact":"genre_assigned","recording":"{}","genre":"Techno"}}"#,
            rec(1)
        );
        let (_, report) = parse(&bare, dev(), 100, held);
        assert_eq!(
            report.malformed.len(),
            1,
            "a genre with no stated basis is not a fact this library will take"
        );
    }

    /// **A subject the library does not hold is reported, never invented.**
    ///
    /// A proposal written against a library that has since changed is *stale*
    /// rather than wrong, and naming what it missed is what lets a tool
    /// re-query instead of re-measuring.
    #[test]
    fn an_unknown_subject_is_named_not_invented() {
        let text = format!("{}\n{}", measured(1, 100), measured(99, 200));
        let (records, report) = parse(&text, dev(), 100, held);

        assert_eq!(records.len(), 1);
        assert_eq!(report.unknown, vec![Subject::Encoding(enc(99))]);
        assert!(report.balances(2), "{report}");
    }

    /// **A path is not an identifier**, and the format will not take one.
    ///
    /// This is the whole point of the change: a path is a property of one
    /// device's copy, stale the moment a file moves and meaningless on another
    /// machine.
    #[test]
    fn a_proposal_naming_a_path_is_refused() {
        let text = r#"{"fact":"measured","path":"/m/a.mp3","cutoff_hz":15700,"analyser":1}"#;
        let (records, report) = parse(text, dev(), 100, held);

        assert!(records.is_empty());
        assert_eq!(report.malformed.len(), 1);
    }

    /// A typo must not cost a whole pass, but it must be findable — so the
    /// line number comes back with it.
    #[test]
    fn a_bad_line_is_reported_with_its_number_and_the_rest_still_apply() {
        let text = format!("{}\n{{not json at all\n{}", measured(1, 1), measured(2, 2));
        let (records, report) = parse(&text, dev(), 100, held);

        assert_eq!(report.applied, 2);
        assert_eq!(report.malformed.len(), 1);
        assert_eq!(report.malformed[0].0, 2, "the line a person would jump to");
        assert_eq!(records.len(), 2);
    }

    /// A reviewed file is one somebody has been editing, and both of these are
    /// what people write while editing.
    #[test]
    fn blank_lines_and_comments_are_not_errors() {
        let text = format!("# measured 2026-09-02\n\n{}\n\n", measured(1, 1));
        let (records, report) = parse(&text, dev(), 100, held);
        assert_eq!(report.applied, 1);
        assert!(report.malformed.is_empty());
        assert_eq!(records.len(), 1);
    }

    /// Two measurements taken in the same second must not collapse into one.
    #[test]
    fn proposals_in_one_file_do_not_collide() {
        let text = format!("{}\n{}", measured(1, 1), measured(2, 2));
        let (records, _) = parse(&text, dev(), 100, held);
        assert_eq!(records.len(), 2);
    }

    /// **The round trip.** A tool is handed an id and a locator, opens the
    /// locator, and proposes against the id — so it never computes an identity
    /// and never keys anything by a path.
    #[test]
    fn a_tool_proposes_against_what_the_query_handed_it() {
        let row = Queried {
            recording: rec(1),
            encoding: Some(enc(1)),
            locator: Some("/m/a.mp3".into()),
            title: "Callisto".into(),
            artist: "Bodzin & Romboy".into(),
        };
        let wire = serde_json::to_string(&row).unwrap();
        let back: Queried = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, row, "the query output survives the wire");

        // The tool opens `locator`, measures, and names `encoding`.
        let proposal = format!(
            r#"{{"fact":"measured","encoding":"{}","cutoff_hz":19800,"analyser":1}}"#,
            back.encoding.unwrap()
        );
        let (records, report) = parse(&proposal, dev(), 100, held);
        assert_eq!(report.applied, 1, "{report}");
        assert_eq!(
            records.iter().next().unwrap().fact,
            Fact::EncodingMeasured {
                encoding: enc(1),
                cutoff_hz: 19_800,
                analyser: 1,
            }
        );
    }

    /// A recording this machine does not hold still comes back from a query —
    /// decided about, not owned — with nothing to open.
    #[test]
    fn a_row_with_no_copy_here_is_still_a_row() {
        let row = Queried {
            recording: rec(2),
            encoding: None,
            locator: None,
            title: "Wanted".into(),
            artist: "Someone".into(),
        };
        let wire = serde_json::to_string(&row).unwrap();
        assert_eq!(serde_json::from_str::<Queried>(&wire).unwrap(), row);
    }

    /// An empty file is a successful no-op, not a failure. A pass that found
    /// nothing to say should not read as a pass that broke.
    #[test]
    fn nothing_to_say_is_not_an_error() {
        let (records, report) = parse("", dev(), 100, held);
        assert!(records.is_empty() && report.applied == 0);
        assert!(report.malformed.is_empty() && report.unknown.is_empty());
    }
}
