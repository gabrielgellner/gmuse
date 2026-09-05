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

use crate::model::basis::Basis;
use crate::model::clock::HybridClock;
use crate::model::entity::{Collection, Concept, Device, Encoding, Facet, Recording};
use crate::model::facet::{self, Described};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::log::{Fact, Record};
use crate::model::ord::OrdKey;
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
    /// **This file holds that performance** — the proposal 712 confident
    /// matches have been waiting for.
    ///
    /// The subject is the **encoding**, not the recording, and that is the
    /// whole shape of it. A recording that nothing has named was minted from
    /// its bytes, so its id *is* the claim being corrected; naming the
    /// recording would mean naming the thing this replaces. The file is what
    /// stays put.
    ///
    /// Applying one is therefore a **relink**, not an append: the encoding's
    /// binding moves from the id the bytes minted to the id the authority
    /// names. The old recording is left rather than swept — §1 is explicit
    /// that a row nothing holds is ordinary, and deleting it would take its
    /// ratings and plays with it.
    ///
    /// Nothing about the recording's *description* crosses. An identification
    /// says which performance this is, and a title from MusicBrainz is a
    /// separate claim somebody else can propose.
    Identified {
        encoding: Id<Encoding>,
        basis: Basis,
    },
    /// Which version of a song a recording is — issue #101.
    ///
    /// The route that replaces a stopgap. `identify.py` currently *downgrades*
    /// a match whose title carries a version qualifier ours does not mention,
    /// because there was nowhere to record what it had noticed: an
    /// instrumental and its vocal are the same length by construction and
    /// score 100 on the name, so both checks the matcher has say "same". Three
    /// of 607 confident matches were exactly this.
    ///
    /// Downgrading throws the observation away. Proposing keeps it, and a
    /// person reviewing the file decides.
    QualifierAssigned {
        recording: Id<Recording>,
        /// The concept's label, not its id. A tool should not have to mint —
        /// `Id::of_label` is the model's job, and a label is what a person
        /// reviewing the file can read.
        qualifier: String,
        basis: Source,
        confidence: Confidence,
    },
    /// **This recording belongs to that collection** — #94.
    ///
    /// The route a path-based catalog takes to become a curated set. Measured
    /// on this library, no genre rule reproduces `~/MusicLibrary/Dance`: every
    /// one of its 790 files carries an `Electronic` term, and so do 876 files
    /// in `General` — Laurie Anderson, Beck, Enya, DEVO. Those are correctly
    /// tagged and are simply not what the folder means.
    ///
    /// What the folder records is not a genre but a judgement: *this is dance
    /// music and I listen to it as dance music*. Enya is `Electronic` and is
    /// not in it. That belongs in a collection, whose membership is facts
    /// (#92) — and once it is, the path stops mattering, which is what #90
    /// asks for and what #94's second requirement is waiting on.
    ///
    /// The collection is named rather than identified, for the same reason a
    /// genre is: a tool has no way to mint an id and should not, and a name is
    /// what a person reviewing the file can read.
    CollectedInto {
        recording: Id<Recording>,
        collection: String,
        /// Whether the collection's order means anything. A catalog seeded
        /// from a folder is a set somebody curated, not a running order.
        #[serde(default)]
        ordered: bool,
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

/// What `parse` needs to know about the library it is proposing against.
///
/// It was a single `holds` closure, and that was enough while every proposal
/// was a function of its own line. An identification is not: it moves an
/// encoding's binding, and where it moves *from* is a fact about the library
/// now. A tool cannot supply that — it would be naming the thing its proposal
/// replaces — so the caller answers instead.
pub trait Holdings {
    /// Whether the library has this at all. A proposal against a library that
    /// has since changed is stale rather than wrong.
    fn holds(&self, subject: Subject) -> bool;
    /// Which recording an encoding currently holds.
    fn bound(&self, encoding: Id<Encoding>) -> Option<Id<Recording>>;
    /// What a recording's id was derived from.
    ///
    /// The rung is what separates a repair from a claim: a recording resting
    /// on its bytes is one nothing had named, so identifying it merges. One an
    /// authority already named is a different performance, and a file pointed
    /// at it is a relink with no merge in it.
    fn identified(&self, recording: Id<Recording>) -> Option<Basis>;
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
            Proposal::Measured { encoding, .. } | Proposal::Identified { encoding, .. } => {
                Subject::Encoding(*encoding)
            }
            Proposal::GenreAssigned { recording, .. }
            | Proposal::QualifierAssigned { recording, .. }
            | Proposal::Rated { recording, .. }
            | Proposal::CollectedInto { recording, .. } => Subject::Recording(*recording),
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
    /// Lines the vocabulary refused, with why.
    ///
    /// Distinct from `malformed`, because the line parsed and the subject is
    /// held — what is wrong is the claim. `qualifier = mix-variant` asserts a
    /// grouping concept, which says nothing about a recording and would fold
    /// to an assignment nobody can see; refusing is better than accepting,
    /// because an assignment nobody can see reads as one nobody made.
    pub refused: Vec<(usize, String)>,
}

impl Report {
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — an invariant a test asserts, not an answer anything needs at runtime"
    )]
    pub fn balances(&self, lines: usize) -> bool {
        lines == self.applied + self.malformed.len() + self.unknown.len() + self.refused.len()
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} applied, {} unknown, {} refused, {} malformed",
            self.applied,
            self.unknown.len(),
            self.refused.len(),
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
    library: &impl Holdings,
) -> (BTreeSet<Record>, Report) {
    let mut clock = HybridClock::new(device);
    // Keyed by rendering, because `Fact` is neither `Hash` nor `Ord` and a
    // declaration's identity is exactly what it says.
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
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
        if !library.holds(proposal.subject()) {
            report.unknown.push(proposal.subject());
            continue;
        }
        let facts = match into_facts(proposal, library) {
            Ok(facts) => facts,
            Err(why) => {
                report.refused.push((n + 1, why));
                continue;
            }
        };
        for fact in facts {
            // **A concept is declared once per file, not once per line.**
            //
            // Every assignment brings its concept with it, which is right —
            // an id no fact resolves is an id nobody can render. But a file of
            // 761 proposals into one collection carried 761 identical
            // `CollectionNamed` records, and a file of genre assignments does
            // the same with `GenreCatalogued`. They fold to one row and cost a
            // durable line each; the Dance seeding put 761 of them into a log
            // of 7,974.
            //
            // **Only the declarations**, and the narrowing is a guard rather
            // than a fix. No proposal produces a fact that has to survive
            // twice — a play is recorded by the player, not proposed — so
            // deduplicating everything would behave identically today, and a
            // mutation making it do so passes every test here.
            //
            // It is still the right shape. `RecordingPlayed` carries no
            // timestamp, so two plays of one track are the same fact told
            // apart only by their stamps; the day something proposes one, a
            // blanket rule would count one play where there were two, and
            // silently. `declares_a_concept` is what makes adding that a
            // deliberate act rather than an accident.
            if declares_a_concept(&fact) && !declared.insert(format!("{fact:?}")) {
                continue;
            }
            out.insert(Record {
                stamp: clock.stamp(now),
                fact,
            });
        }
        report.applied += 1;
    }
    (out, report)
}

/// Whether a fact only *declares* something, so saying it twice says nothing.
///
/// These name a concept so that an id resolves to something a person reads.
/// They are pure upserts: two identical ones fold to one row, and the second
/// buys a line in a durable log and nothing else.
///
/// **Kept to a list rather than inferred.** "Idempotent" is not a property the
/// type system carries here — `RecordingPlayed` is *counted*, and it holds no
/// timestamp, so two plays of one track are the same fact told apart only by
/// their stamps. Anything added to this list has to be checked, not guessed.
fn declares_a_concept(fact: &Fact) -> bool {
    matches!(
        fact,
        Fact::ConceptCatalogued { .. } | Fact::CollectionNamed { .. }
    )
}

/// One proposal becomes the facts it implies, or says why it cannot.
///
/// **More than one, because a concept has to exist before it can be
/// assigned.** This used to return a single fact, and an applied genre
/// proposal for a term the vocabulary had never seen wrote an assignment whose
/// id nothing named — an id no fact resolves is an id nobody can render.
/// `Instance::assign_genre` got this right; the apply path bypassed it.
fn into_facts(proposal: Proposal, library: &impl Holdings) -> Result<Vec<Fact>, String> {
    Ok(match proposal {
        Proposal::Measured {
            encoding,
            cutoff_hz,
            analyser,
        } => vec![Fact::EncodingMeasured {
            encoding,
            cutoff_hz,
            analyser,
        }],
        // The genre is named rather than identified, because a tool has no way
        // to mint a concept id and should not: the vocabulary is ours (§2), so
        // the name is resolved against it here.
        // **All four fields cross now.** This line used to read
        // `{ recording, genre, .. } => Fact::GenreAssigned { recording, genre }`,
        // and that `..` threw away the basis and the confidence the tool had
        // gone to the trouble of establishing — so a Discogs guess and a typed
        // answer landed in the log as the same fact.
        Proposal::GenreAssigned {
            recording,
            genre,
            basis,
            confidence,
        } => {
            let concept = Id::<Concept>::in_facet(facet::GENRE, &genre);
            vec![
                Fact::ConceptCatalogued {
                    facet: Id::<Facet>::of_key(facet::GENRE),
                    concept,
                    label: genre.trim().to_string(),
                    node_label: false,
                },
                Fact::ConceptAsserted {
                    subject: Described::Recording(recording),
                    concept,
                    basis,
                    confidence,
                },
            ]
        }
        // The collection is named rather than identified, for the reason a
        // genre is: a tool has no way to mint an id and should not.
        Proposal::CollectedInto {
            recording,
            collection,
            ordered,
        } => {
            let name = collection.trim();
            if name.is_empty() {
                return Err("a collection needs a name".to_string());
            }
            let id = Id::<Collection>::of_name(name);
            vec![
                Fact::CollectionNamed {
                    collection: id,
                    name: name.to_string(),
                    ordered,
                },
                Fact::CollectionEntryAdded {
                    collection: id,
                    recording,
                    // **Every seeded entry shares a position, and that is the
                    // honest encoding.** A folder has no running order, so
                    // giving each entry a distinct key would invent one — and
                    // an arbitrary order that looks meaningful is worse than
                    // none. Entries stay distinct because the fold keys them
                    // by `(collection, position, recording)`, and applying the
                    // same proposal twice is the same row.
                    //
                    // A collection later made *ordered* wants its keys spread
                    // first, which is the compaction `domain-model.md` names.
                    ord: OrdKey::first(),
                },
            ]
        }
        // Refused at the door rather than folded to nothing, for the reason
        // `Instance::assign_qualifier` refuses it: a grouping concept is a
        // place in the tree, not a claim about a song, and an assignment
        // nobody can see is indistinguishable from one nobody made.
        Proposal::QualifierAssigned {
            recording,
            qualifier,
            basis,
            confidence,
        } => {
            if !crate::model::qualifier::assignable(&qualifier) {
                return Err(format!(
                    "{qualifier:?} is a node label and says nothing about a recording"
                ));
            }
            let concept = Id::<Concept>::in_facet(facet::VERSION, &qualifier);
            vec![
                Fact::ConceptCatalogued {
                    facet: Id::<Facet>::of_key(facet::VERSION),
                    concept,
                    label: qualifier.trim().to_string(),
                    node_label: false,
                },
                Fact::ConceptAsserted {
                    subject: Described::Recording(recording),
                    concept,
                    basis,
                    confidence,
                },
            ]
        }
        // **A relink, and the one proposal that is not a function of its own
        // line.** Where the binding moves *from* is a fact about the library
        // now, not about what the tool found, so `parse` has to be able to ask.
        Proposal::Identified { encoding, basis } => {
            let to = Id::<Recording>::of_basis(&basis);
            let from = library.bound(encoding);
            if from == Some(to) {
                return Err("already holds that performance".into());
            }
            let mut facts = vec![
                Fact::RecordingIdentified {
                    recording: to,
                    basis,
                },
                Fact::EncodingBound { encoding, from, to },
            ];
            // **The history has to come too.**
            //
            // Naming a performance mints a new id, and every rating and play
            // anyone gave the old one hangs off the id nothing points at now.
            // Applying 377 identifications stranded 7 of 11 ratings and 118 of
            // 255 played recordings exactly that way — nothing failed, the
            // facts were all still in the log, and they were unreachable.
            //
            // **Only when the old recording was minted from the bytes.** That
            // rung means "nothing had named this", so the two are one
            // performance badly named rather than two performances — and
            // merging is a repair rather than a claim. A recording an
            // authority had already named is a different performance, and
            // pointing a file at it is a relink with no merge in it.
            if let Some(old) = from
                && matches!(library.identified(old), Some(Basis::Audio(_)))
            {
                facts.push(Fact::RecordingMerged {
                    loser: old,
                    winner: to,
                });
            }
            facts
        }
        Proposal::Rated { recording, stars } => {
            vec![Fact::RecordingRated { recording, stars }]
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn dev() -> Id<Device> {
        Id::derived(b"laptop")
    }

    fn enc(n: u128) -> Id<Encoding> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    pub(crate) fn rec(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// The MusicBrainz id encoding 3 has already been identified as, so that
    /// "propose what is already true" is reachable rather than hypothetical.
    fn settled() -> uuid::Uuid {
        uuid::Uuid::from_u128(0xaaaa)
    }

    /// A library holding encodings 1–3 and recordings 1 and 2. Encoding 1 is
    /// bound to a recording minted from its bytes, encoding 2 is unbound, and
    /// encoding 3 already holds the performance `settled()` names.
    pub(crate) struct Held;

    impl Holdings for Held {
        fn holds(&self, s: Subject) -> bool {
            match s {
                Subject::Encoding(e) => e == enc(1) || e == enc(2) || e == enc(3),
                Subject::Recording(r) => r == rec(1) || r == rec(2),
            }
        }

        fn bound(&self, encoding: Id<Encoding>) -> Option<Id<Recording>> {
            match encoding {
                e if e == enc(1) => Some(rec(1)),
                e if e == enc(3) => Some(Id::of_basis(&Basis::MusicBrainz(settled()))),
                _ => None,
            }
        }

        /// `rec(1)` rests on its bytes — nothing had named it — and the
        /// recording encoding 3 holds was named by MusicBrainz.
        fn identified(&self, recording: Id<Recording>) -> Option<Basis> {
            if recording == rec(1) {
                Some(Basis::Audio(crate::audio_id::AudioId {
                    scheme: 1,
                    hash: 1,
                }))
            } else {
                Some(Basis::MusicBrainz(settled()))
            }
        }
    }

    pub(crate) const HELD: &Held = &Held;

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
        let (records, report) = parse(&measured(1, 15_700), dev(), 100, HELD);

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
        let (_, report) = parse(&good, dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        let bare = format!(
            r#"{{"fact":"genre_assigned","recording":"{}","genre":"Techno"}}"#,
            rec(1)
        );
        let (_, report) = parse(&bare, dev(), 100, HELD);
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
        let (records, report) = parse(&text, dev(), 100, HELD);

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
        let (records, report) = parse(text, dev(), 100, HELD);

        assert!(records.is_empty());
        assert_eq!(report.malformed.len(), 1);
    }

    /// A typo must not cost a whole pass, but it must be findable — so the
    /// line number comes back with it.
    #[test]
    fn a_bad_line_is_reported_with_its_number_and_the_rest_still_apply() {
        let text = format!("{}\n{{not json at all\n{}", measured(1, 1), measured(2, 2));
        let (records, report) = parse(&text, dev(), 100, HELD);

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
        let (records, report) = parse(&text, dev(), 100, HELD);
        assert_eq!(report.applied, 1);
        assert!(report.malformed.is_empty());
        assert_eq!(records.len(), 1);
    }

    /// Two measurements taken in the same second must not collapse into one.
    #[test]
    fn proposals_in_one_file_do_not_collide() {
        let text = format!("{}\n{}", measured(1, 1), measured(2, 2));
        let (records, _) = parse(&text, dev(), 100, HELD);
        assert_eq!(records.len(), 2);
    }

    /// **A concept has to exist before it can be assigned.**
    ///
    /// The apply path used to emit the assignment alone, so a genre the
    /// vocabulary had never seen arrived as an id no fact resolved — an id
    /// nobody can render, and a row that looks like a bug in the renderer
    /// rather than a gap in the log. `Instance::assign_genre` always got this
    /// right; this path bypassed it.
    #[test]
    fn an_assignment_brings_its_concept_with_it() {
        let text = format!(
            r#"{{"fact":"genre_assigned","recording":"{}","genre":"Dub Techno","basis":"discogs","confidence":"matched"}}"#,
            rec(1)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 1, "one line, however many facts");
        assert!(
            records.iter().any(
                |r| matches!(&r.fact, Fact::ConceptCatalogued { label, .. } if label == "Dub Techno")
            ),
            "the assignment names a concept nothing defines"
        );
        assert_eq!(records.len(), 2);
    }

    /// **The wire form is what a person would type.**
    ///
    /// Snake case turned `MusicBrainz` into `music_brainz`, which nobody has
    /// ever written down. Found by writing a test against the format the way a
    /// person would use it, which is the only way this kind of defect shows
    /// up — every round trip through serde agreed with itself.
    #[test]
    fn a_basis_is_spelled_the_way_it_is_said() {
        for (wire, basis) in [
            ("musicbrainz", Source::MusicBrainz),
            ("discogs", Source::Discogs),
            ("manual", Source::Manual),
            ("tag_variant", Source::TagVariant),
        ] {
            let text = format!(
                r#"{{"fact":"genre_assigned","recording":"{}","genre":"Techno","basis":"{wire}","confidence":"matched"}}"#,
                rec(1)
            );
            let (records, report) = parse(&text, dev(), 100, HELD);
            assert!(report.malformed.is_empty(), "{wire}: {report}");
            assert!(records.iter().any(|r| matches!(
                &r.fact,
                Fact::ConceptAsserted { basis: b, .. } if *b == basis
            )));
        }
    }

    /// A qualifier proposal is the route that replaces a stopgap: the matcher
    /// used to *downgrade* a match whose title carried a version it had not
    /// asked for, because there was nowhere to record what it had noticed.
    #[test]
    fn a_qualifier_can_be_proposed() {
        let text = format!(
            r#"{{"fact":"qualifier_assigned","recording":"{}","qualifier":"Instrumental","basis":"musicbrainz","confidence":"matched"}}"#,
            rec(1)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 1, "{report}");
        assert!(report.refused.is_empty(), "{report}");
        assert!(records.iter().any(|r| matches!(
            &r.fact,
            Fact::ConceptAsserted { concept, .. }
                if *concept == Id::<Concept>::in_facet(facet::VERSION, "instrumental")
        )));
    }

    /// **A grouping concept is refused, and refusal is its own outcome.**
    ///
    /// The line parsed and the library holds the recording — what is wrong is
    /// the claim, so calling it malformed would send a person looking for a
    /// typo. `mix-variant` is a place in the tree rather than a statement
    /// about a song, and folding it would make an assignment nobody can see.
    #[test]
    fn a_grouping_concept_is_refused_with_its_reason() {
        let text = format!(
            r#"{{"fact":"qualifier_assigned","recording":"{}","qualifier":"mix-variant","basis":"manual","confidence":"confirmed"}}"#,
            rec(1)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert!(records.is_empty());
        assert!(report.malformed.is_empty(), "a refusal is not a typo");
        assert_eq!(report.refused.len(), 1);
        assert_eq!(report.refused[0].0, 1, "the line a person would jump to");
        assert!(report.balances(1), "{report}");
    }

    /// **An identification is a relink**, and the binding says what it moved
    /// from.
    ///
    /// The encoding is the subject because the recording's id *is* the claim
    /// being corrected — a recording nothing had named was minted from these
    /// bytes, so naming it would mean naming the thing this replaces.
    #[test]
    fn an_identification_moves_the_binding() {
        let mbid = uuid::Uuid::from_u128(0xca11_1500);
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{mbid}"}}}}"#,
            enc(1)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 1, "{report}");
        let named = Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid));
        assert!(
            records.iter().any(|r| matches!(
                &r.fact,
                Fact::EncodingBound { encoding, from, to }
                    if *encoding == enc(1) && *from == Some(rec(1)) && *to == named
            )),
            "the binding did not move, or lost what it replaced"
        );
        assert!(records.iter().any(|r| matches!(
            &r.fact,
            Fact::RecordingIdentified { basis: Basis::MusicBrainz(u), .. } if *u == mbid
        )));
    }

    /// **A description does not ride along.** An identification says which
    /// performance this is; what it is called is a separate claim somebody
    /// else can propose, and inventing one here would be the projection's old
    /// habit in a new place.
    #[test]
    fn an_identification_does_not_rename_anything() {
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{}"}}}}"#,
            enc(1),
            uuid::Uuid::from_u128(7)
        );
        let (records, _) = parse(&text, dev(), 100, HELD);
        assert!(
            !records
                .iter()
                .any(|r| matches!(&r.fact, Fact::RecordingCatalogued { .. })),
            "a title arrived from somewhere nobody proposed one"
        );
    }

    /// **Re-running a reviewed file relinks nothing.**
    ///
    /// Refused rather than applied and then deduplicated: the second run has
    /// nothing to say, and an `EncodingBound { from: X, to: X }` in the log
    /// would read as a decision somebody made.
    #[test]
    fn identifying_what_is_already_identified_is_refused() {
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{}"}}}}"#,
            enc(3),
            settled()
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert!(records.is_empty());
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
        assert!(report.balances(1), "{report}");
    }

    /// **The history comes with it**, which is the whole point of merging.
    ///
    /// Naming a performance mints a new id. Without this, every rating and
    /// play given to the byte-minted one hangs off an id nothing points at:
    /// 7 of 11 ratings and 118 of 255 played recordings, on the first real
    /// run, all still in the log and none of them reachable.
    #[test]
    fn identifying_a_byte_minted_recording_merges_it() {
        let mbid = uuid::Uuid::from_u128(0xca11_1500);
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{mbid}"}}}}"#,
            enc(1)
        );
        let (records, _) = parse(&text, dev(), 100, HELD);

        let named = Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid));
        assert!(
            records.iter().any(|r| matches!(
                &r.fact,
                Fact::RecordingMerged { loser, winner } if *loser == rec(1) && *winner == named
            )),
            "the ratings and plays were left on the id nothing points at"
        );
    }

    /// **A recording an authority already named is not merged away.**
    ///
    /// Then the two are different performances and pointing a file at one of
    /// them is an ordinary relink. Merging would be a claim rather than a
    /// repair, and a false merge is the error this model refuses to make —
    /// a split is one merge from repaired and a merge is permanent.
    #[test]
    fn relinking_away_from_a_named_recording_does_not_merge() {
        // Encoding 3 holds a recording MusicBrainz named; point it elsewhere.
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"musicbrainz":"{}"}}}}"#,
            enc(3),
            uuid::Uuid::from_u128(0xbbbb)
        );
        let (records, report) = parse(&text, dev(), 100, HELD);

        assert_eq!(report.applied, 1, "{report}");
        assert!(
            !records
                .iter()
                .any(|r| matches!(&r.fact, Fact::RecordingMerged { .. })),
            "a relink between two named performances merged them"
        );
    }

    /// The old spelling still reads, because it is already in the live log.
    #[test]
    fn a_basis_written_the_old_way_still_parses() {
        let mbid = uuid::Uuid::from_u128(9);
        let text = format!(
            r#"{{"fact":"identified","encoding":"{}","basis":{{"MusicBrainz":"{mbid}"}}}}"#,
            enc(2)
        );
        let (_, report) = parse(&text, dev(), 100, HELD);
        assert!(report.malformed.is_empty(), "{report}");
        assert_eq!(report.applied, 1);
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
        let (records, report) = parse(&proposal, dev(), 100, HELD);
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
        let (records, report) = parse("", dev(), 100, HELD);
        assert!(records.is_empty() && report.applied == 0);
        assert!(report.malformed.is_empty() && report.unknown.is_empty());
    }
}

#[cfg(test)]
mod collecting {
    //! **A path-based catalog becoming a curated set** — #94.
    //!
    //! Measured on the real library: no genre rule reproduces
    //! `~/MusicLibrary/Dance`. Every one of its 790 files carries an
    //! `Electronic` term and so do 876 files in `General` — Laurie Anderson,
    //! Beck, Enya, DEVO, Britney Spears — which are correctly tagged and are
    //! simply not what the folder means. The best rule tried still misplaces
    //! 139 files; the obvious one more than doubles the catalog.
    //!
    //! What the folder records is a judgement rather than a genre, and a
    //! collection is where a judgement lives.

    use super::tests::*;
    use super::*;

    fn line(r: u128, into: &str) -> String {
        format!(
            r#"{{"fact":"collected_into","recording":"{}","collection":"{into}"}}"#,
            rec(r)
        )
    }

    /// The concept before the membership, for the reason a genre needs one: an
    /// id no fact resolves is an id nobody can render.
    #[test]
    fn a_collection_is_named_before_anything_is_put_in_it() {
        let (records, report) = parse(&line(1, "Dance"), dev(), 100, HELD);
        assert_eq!(report.applied, 1, "{report}");

        let facts: Vec<&Fact> = records.iter().map(|r| &r.fact).collect();
        assert!(
            facts
                .iter()
                .any(|f| matches!(f, Fact::CollectionNamed { name, .. } if name == "Dance")),
            "the collection was never named: {facts:?}"
        );
        assert!(
            facts
                .iter()
                .any(|f| matches!(f, Fact::CollectionEntryAdded { .. })),
            "nothing was put in it: {facts:?}"
        );
    }

    /// **A concept is declared once per file, not once per line.**
    ///
    /// Every assignment brings its concept with it, because an id no fact
    /// resolves is an id nobody can render. But seeding the Dance collection
    /// from its folder was 761 proposals into one collection, and it put 761
    /// identical `CollectionNamed` records into a log of 7,974 — they fold to
    /// one row and cost a durable line each.
    #[test]
    fn one_collection_is_named_once_however_many_go_into_it() {
        let file = (1..=2)
            .map(|n| line(n, "Dance"))
            .collect::<Vec<_>>()
            .join("\n");
        let (records, report) = parse(&file, dev(), 100, HELD);
        assert_eq!(report.applied, 2, "{report}");

        let named = records
            .iter()
            .filter(|r| matches!(&r.fact, Fact::CollectionNamed { .. }))
            .count();
        assert_eq!(named, 1, "the collection was named once per entry");
        let added = records
            .iter()
            .filter(|r| matches!(&r.fact, Fact::CollectionEntryAdded { .. }))
            .count();
        assert_eq!(added, 2, "and both entries still went in");
    }

    /// **But a play is counted, so the rule is a list and not a guess.**
    ///
    /// `RecordingPlayed` carries no timestamp: two plays of one track are the
    /// same fact told apart only by their stamps. Nothing proposes one today —
    /// a play is recorded by the player, not proposed — so this cannot be
    /// reached through `parse`, and asserting the predicate is the honest way
    /// to say it. Somebody adding a `Played` proposal has to come past this.
    #[test]
    fn a_counted_fact_is_not_a_declaration() {
        assert!(declares_a_concept(&Fact::CollectionNamed {
            collection: Id::<Collection>::of_name("Dance"),
            name: "Dance".into(),
            ordered: false,
        }));
        assert!(
            !declares_a_concept(&Fact::RecordingPlayed {
                recording: rec(1),
                encoding: None,
            }),
            "deduplicating this would count one play where there were two"
        );
        assert!(
            !declares_a_concept(&Fact::CollectionEntryAdded {
                collection: Id::<Collection>::of_name("Dance"),
                recording: rec(1),
                ord: crate::model::ord::OrdKey::first(),
            }),
            "an entry is membership, not a declaration"
        );
    }

    /// **Applying the same file twice changes nothing**, which is what makes a
    /// seeding pass safe to re-run against a live log.
    #[test]
    fn seeding_twice_is_seeding_once() {
        let file = format!("{}\n{}", line(1, "Dance"), line(2, "Dance"));
        let (first, _) = parse(&file, dev(), 100, HELD);
        let (again, _) = parse(&file, dev(), 200, HELD);

        // Compared as rendered facts, since a `Fact` is not `Ord` — and the
        // stamps deliberately differ, so it is the facts that must not.
        let facts = |rs: &BTreeSet<Record>| {
            rs.iter()
                .map(|r| format!("{:?}", r.fact))
                .collect::<BTreeSet<String>>()
        };
        assert_eq!(
            facts(&first),
            facts(&again),
            "a second pass proposed different facts"
        );
    }

    /// Two spellings are one collection, because the id is the normalised name
    /// — the same rule every other named concept follows.
    #[test]
    fn two_spellings_name_one_collection() {
        let (records, _) = parse(
            &format!("{}\n{}", line(1, "Dance"), line(2, "  dance  ")),
            dev(),
            100,
            HELD,
        );
        let ids: BTreeSet<_> = records
            .iter()
            .filter_map(|r| match &r.fact {
                Fact::CollectionEntryAdded { collection, .. } => Some(*collection),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 1, "one collection became two");
    }

    /// A nameless collection is refused rather than folded into one nobody can
    /// find, which is the same call `QualifierAssigned` makes.
    #[test]
    fn a_collection_with_no_name_is_refused() {
        let (_, report) = parse(&line(1, "   "), dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.refused.len(), 1, "{report}");
    }

    /// And a recording the library does not hold is named, not invented — the
    /// rule every proposal follows.
    #[test]
    fn a_recording_the_library_does_not_hold_is_refused() {
        let (_, report) = parse(&line(99, "Dance"), dev(), 100, HELD);
        assert_eq!(report.applied, 0);
        assert_eq!(report.unknown.len(), 1, "{report}");
    }
}
