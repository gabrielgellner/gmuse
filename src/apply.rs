//! `gmuse library apply` — a file of proposed facts becomes facts, or does
//! not — issue #91, `library-api.md` §2 and §5.
//!
//! **A dry run unless told otherwise.** The genre pipeline learned this
//! already: a pass that writes by default is a pass nobody reads the output of
//! first, and these files come from tools that are wrong often enough to be
//! worth reviewing. Today's identification run is the case in point — 3 of 607
//! confident matches were a different version of the right song.
//!
//! ## What "the library holds this" means here
//!
//! A proposal names an encoding or a recording by **id**, and `parse` needs to
//! know whether the library has one. That answer comes from a scan rather than
//! from the log: encodings are established by looking at the disk, and a
//! measurement proposed against a file that has since gone is stale rather
//! than wrong. Saying *which* ones is what lets a tool re-query instead of
//! re-measure.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::model::durable;
use crate::model::entity::Device;
use crate::model::id::Id;
use crate::model::instance::Instance;
use crate::model::proposal::{self, Report, Subject};
use crate::model::spine::Recording;

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("writing the log: {0}")]
    Write(#[from] durable::LogError),
}

/// The two things a proposal is checked against, and they are not one thing.
///
/// **What exists** comes from the disk: a measurement proposed against a file
/// that has since been deleted is stale rather than wrong, and saying which
/// ones lets a tool re-query instead of re-measure.
///
/// **What a file currently holds** comes from the log, because a relink is a
/// decision somebody recorded and a scan cannot see one. Reading the binding
/// off the disk instead would let an identification quietly undo the last one.
struct Shelf {
    disk: crate::model::log::Catalogue,
    log: crate::model::log::Catalogue,
}

impl proposal::Holdings for Shelf {
    fn holds(&self, subject: Subject) -> bool {
        match subject {
            Subject::Encoding(id) => self.disk.encodings.contains_key(&id),
            // **One place, because `disk` is folded log-first.** Adding the
            // log as a second source here was my first fix and it changed
            // nothing: the recording was missing from *both*, because a scan
            // into an empty instance re-mints a relinked recording from its
            // bytes. Fixing the fold made this arm right again, and leaving
            // the `||` would have been two ways to answer one question with
            // only one of them load-bearing.
            Subject::Recording(id) => self.disk.recordings.contains_key(&id),
            // **The vocabulary is always held** (#79). A hierarchy edge is
            // curation of the codelist, not a claim about the disk, and the
            // term a hierarchy is built ahead of is precisely the one nothing
            // is tagged with yet.
            Subject::Vocabulary => true,
            // **The disc has to already be here** — #155. A tracklist's ids
            // agree with the ones `import` minted only where the files carried
            // the album's MusicBrainz id; without one, `import` mints the
            // release from the *group* and a tracklist arriving against the
            // real MBID would catalogue a phantom disc beside the real one.
            //
            // One place, for the reason `Recording` above is: `disk` is folded
            // log-first, so it already carries every medium the log knows.
            Subject::Medium(id) => self.disk.media.contains_key(&id),
            // **The winner has to be an album the library holds** — #156. A
            // merge into an id nothing names is a redirect pointing nowhere,
            // and the loser is deliberately not checked: it is the row being
            // retired, and one already merged elsewhere is refused by
            // `Merges` rather than here.
            Subject::ReleaseGroup(id) => self.disk.groups.contains_key(&id),
            // **An artist the library credits somewhere** — #154. Held rather
            // than named: the case this exists for is an artist with a right
            // id and no name, so requiring a name would refuse exactly the row
            // it is meant to repair.
            Subject::Artist(id) => {
                self.disk.artist_names.contains_key(&id)
                    || self
                        .disk
                        .credits
                        .keys()
                        .any(|(_, artist, ..)| *artist == id)
            }
        }
    }

    fn identified(&self, recording: Id<Recording>) -> Option<crate::model::basis::Basis> {
        self.log
            .identified
            .get(&recording)
            .or_else(|| self.disk.identified.get(&recording))
            .cloned()
    }

    fn bound(&self, encoding: Id<crate::model::entity::Encoding>) -> Option<Id<Recording>> {
        self.log
            .bound
            .get(&encoding)
            .copied()
            .or_else(|| self.disk.encodings.get(&encoding).and_then(|e| e.recording))
    }

    fn slot(
        &self,
        encoding: Id<crate::model::entity::Encoding>,
    ) -> Option<crate::model::proposal::Slot> {
        let track = self
            .log
            .placed
            .get(&encoding)
            .or_else(|| self.disk.placed.get(&encoding))?;
        let placed = self
            .log
            .tracks
            .get(track)
            .or_else(|| self.disk.tracks.get(track))?;
        Some(crate::model::proposal::Slot {
            track: *track,
            medium: placed.medium,
            position: placed.position,
        })
    }
}

/// What a run did, and what it declined to do.
#[derive(Debug)]
pub struct Outcome {
    pub report: Report,
    /// Facts already in the log, so a second run is not a second play count.
    pub already: usize,
    /// Facts this run would append — **records, like `already`**.
    ///
    /// Stored rather than derived from `report.applied`, because that counts
    /// *lines* and this counts *facts*, and one proposal makes several. The
    /// display subtracted one from the other and so under-reported by however
    /// many declarations a file repeated: seeding the Dance collection said
    /// "106 would apply" and appended 107.
    pub fresh: usize,
    /// Records actually appended. Zero on a dry run.
    pub written: usize,
    pub dry_run: bool,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} proposals — {} new fact(s), {} already known, {} unknown to this library, {} refused, {} malformed",
            self.report.applied
                + self.report.unknown.len()
                + self.report.refused.len()
                + self.report.malformed.len(),
            self.fresh,
            self.already,
            self.report.unknown.len(),
            self.report.refused.len(),
            self.report.malformed.len(),
        )?;
        if self.dry_run {
            write!(f, "\nnothing written — pass `--write` to apply")
        } else {
            write!(f, "\n{} appended", self.written)
        }
    }
}

/// The edge in a hierarchy fact, and whether it asserts or withdraws.
///
/// **The two facts that come in a pair.** Row-existence claims fold to a set,
/// so what matters is the state after the last word about a pair — not whether
/// some record once said it. Everything else here is either idempotent in the
/// fold or, like a play, counted, and keeps the plain replay guard.
type PairKey = (
    Id<crate::model::entity::Hierarchy>,
    Id<crate::model::entity::Concept>,
    Id<crate::model::entity::Concept>,
);

fn paired(fact: &crate::model::log::Fact) -> Option<(PairKey, bool)> {
    use crate::model::log::Fact;
    match fact {
        Fact::HierarchyNarrowed {
            hierarchy,
            parent,
            child,
        } => Some(((*hierarchy, *parent, *child), true)),
        Fact::HierarchyNarrowRetracted {
            hierarchy,
            parent,
            child,
        } => Some(((*hierarchy, *parent, *child), false)),
        _ => None,
    }
}

/// The library a proposal is checked against.
///
/// **Extracted so a test can exercise the real thing.** The first version of
/// the test below built this by hand and passed while `run` still had the bug
/// — which is a test of a copy of the code, not of the code.
fn shelf(log: &Instance, roots: &[std::path::PathBuf], device: Id<Device>, now: i64) -> Shelf {
    let mut scanned = Instance::new(device);
    scanned.merge(now, log.records());
    crate::model::populate::populate(&mut scanned, roots, now);
    Shelf {
        // What the disk says exists, which is what "holds" asks about:
        // encodings are established by looking, and a proposal against a file
        // that has since gone is stale rather than wrong.
        disk: scanned.catalogue(),
        // Where a binding currently points is a fact about the *log*, not the
        // disk — a relink somebody made lives there and a scan cannot see it.
        log: log.catalogue(),
    }
}

/// Read a proposal file and, if told to, append what it proposes.
pub fn run(
    file: &Path,
    data_dir: &Path,
    roots: &[PathBuf],
    device: Id<Device>,
    now: i64,
    write: bool,
) -> Result<Outcome, ApplyError> {
    let text = std::fs::read_to_string(file).map_err(|source| ApplyError::Read {
        path: file.to_path_buf(),
        source,
    })?;

    let (mut log, _) = Instance::open(data_dir, device, now);
    // **The log first, then the disk** — the same order `query` takes, and for
    // the same reason it had to learn: `ingest` refuses to mint over a binding
    // somebody made, but only over one it can see, and a scan into an empty
    // instance sees none. Scanning first re-minted every relinked recording
    // from its bytes, so a proposal naming one was refused as unknown to the
    // library — 141 of 761 here, and silently, because `unknown` reads as
    // "stale, re-query" rather than "this is a bug".
    //
    // It is the same failure that let 377 identifications apply cleanly and
    // leave the work list where it was, in a second place.
    let library = shelf(&log, roots, device, now);
    let (proposed, report) = proposal::parse(&text, device, now, &library);

    // **Facts already in the log are not applied again.** A play is *counted*,
    // so replaying a file would inflate a play count — the one fact here where
    // saying it twice is not idempotent, and the reason this is subtracted
    // rather than left to the record set's union.
    //
    // Compared by wire form rather than by deriving `Ord` on `Fact`, because
    // that is what "the same fact" means: `Record` orders by stamp alone and
    // deliberately, so two records carrying one fact are two records.
    let seen: std::collections::HashSet<String> = log
        .records()
        .iter()
        .filter_map(|r| serde_json::to_string(&r.fact).ok())
        .collect();

    // **A paired fact is judged against the state, not against the history.**
    // "Already in the log" is not "still true": these come in assert/retract
    // pairs, so a record may be there *and* have been taken back — two lines
    // up in this same file, or a week ago. Judging by presence leaves whichever
    // came first as the last word, which is how curation quietly unwound on
    // this library: a pass that meant to move an edge dropped its
    // re-assertion as a duplicate and kept the retraction.
    //
    // Within the batch, only the **last** word about a pair matters, because
    // the fold is a set insert or a set removal and everything before it is
    // overwritten.
    let cat = log.catalogue();
    let mut last_for_pair: std::collections::BTreeMap<PairKey, bool> = Default::default();
    for fact in proposed.iter().map(|r| &r.fact) {
        if let Some((pair, asserting)) = paired(fact) {
            last_for_pair.insert(pair, asserting);
        }
    }
    let redundant = |fact: &crate::model::log::Fact| match paired(fact) {
        None => false,
        Some((pair, asserting)) => {
            // Superseded within this file, or the state already says it.
            last_for_pair.get(&pair) != Some(&asserting) || cat.narrows.contains(&pair) == asserting
        }
    };

    // **A name is judged against the state too, for the pair rule's reason.**
    //
    // `ArtistNamed` is last-writer-wins over one key, so "already in the log"
    // is not "still the name". Dolly Parton was named correctly and then, by a
    // joined credit the #145 guard could not see in Vorbis, renamed to
    // `Dolly Parton, Linda Ronstadt & Emmylou Harris`. Both facts are in the
    // log. Re-asserting the right one is the *only* way to outrank the wrong
    // one — and the replay guard dropped it as a duplicate, which is why a
    // re-import repairs nothing and reports `0 new fact(s)`.
    //
    // Judged by what the fold currently says, so a name that is already the
    // answer is still `already known` and a run over an unchanged library
    // still writes nothing.
    let restates_the_name = |fact: &crate::model::log::Fact| match fact {
        crate::model::log::Fact::ArtistNamed { artist, name } => {
            Some(cat.artist_names.get(artist).map(String::as_str) == Some(name.as_str()))
        }
        _ => None,
    };

    let fresh: BTreeSet<_> = proposed
        .iter()
        .filter(|r| {
            if paired(&r.fact).is_some() {
                return !redundant(&r.fact);
            }
            if let Some(already_the_name) = restates_the_name(&r.fact) {
                return !already_the_name;
            }
            serde_json::to_string(&r.fact).is_ok_and(|encoded| !seen.contains(&encoded))
        })
        .cloned()
        .collect();
    let already = proposed.len() - fresh.len();

    let written = if write {
        log.merge(now, &fresh);
        log.flush(data_dir)?
    } else {
        0
    };

    Ok(Outcome {
        report,
        already,
        fresh: fresh.len(),
        written,
        dry_run: !write,
    })
}

#[cfg(test)]
mod tests {
    //! **A proposal about a relinked recording is not "unknown"** — #94.
    //!
    //! `holds` decides whether the library has heard of a proposal's subject,
    //! and a subject it has not heard of lands in `unknown`, which reads as
    //! "stale, re-query" rather than "this is a bug". So getting it wrong is
    //! silent, and it was: `apply` scanned into an empty instance, so `ingest`
    //! re-minted every relinked recording from its bytes and no proposal
    //! naming one could be recognised. 141 of 761 on the real library.
    //!
    //! It is the same failure that let 377 identifications apply cleanly and
    //! leave the work list exactly where it was — `query` carries the fix and
    //! the note, and this is the second place that needed both.

    use crate::model::basis::Basis;
    use crate::model::entity::Device;
    use crate::model::id::Id;
    use crate::model::instance::Instance;
    use crate::model::proposal::{Holdings, Subject};

    use super::Shelf;

    /// **A re-assertion is not a replay** — the bug that silently unwound
    /// curation on the real library.
    ///
    /// Facts already in the log are skipped so that replaying a file does not
    /// count a play twice. But "already in the log" is not "still true": a
    /// retraction may sit after it, either two lines up in the same file or a
    /// week ago in the history. Skipping the assertion in either case leaves
    /// the retraction as the last word, and the edge quietly stays gone.
    ///
    /// Both directions here, because they failed one after the other.
    #[test]
    fn an_edge_taken_back_can_be_asserted_again() {
        use crate::model::entity::{Concept, Facet};
        use crate::model::facet::GENRE;

        let dir = std::env::temp_dir().join("gmuse-apply-readd");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let device = Id::<Device>::derived(b"laptop");
        let array = Id::<Facet>::array(GENRE);
        let pair = (
            array,
            Id::<Concept>::in_facet(GENRE, "Electronic"),
            Id::<Concept>::in_facet(GENRE, "Techno"),
        );
        let edge = r#"{"fact":"narrowed","broader":"Electronic","narrower":"Techno"}"#;
        let undo = r#"{"fact":"narrowing_retracted","broader":"Electronic","narrower":"Techno"}"#;
        let file = dir.join("edges.jsonl");
        let live = |dir: &std::path::Path| {
            let (log, _) = Instance::open(dir, device, 0);
            log.catalogue().narrows.contains(&pair)
        };

        std::fs::write(&file, format!("{edge}\n")).unwrap();
        super::run(&file, &dir, &[], device, 100, true).expect("applies");
        assert!(live(&dir), "the edge did not land");

        // **In the same file**, which is what a curation pass emits when a
        // term moves and moves back.
        std::fs::write(&file, format!("{undo}\n{edge}\n")).unwrap();
        super::run(&file, &dir, &[], device, 200, true).expect("applies");
        assert!(live(&dir), "a retraction two lines up swallowed the edge");

        // **And across runs**, where the retraction is history rather than
        // batch — the shape that actually bit.
        std::fs::write(&file, format!("{undo}\n")).unwrap();
        super::run(&file, &dir, &[], device, 300, true).expect("applies");
        assert!(!live(&dir), "the retraction did not take");

        std::fs::write(&file, format!("{edge}\n")).unwrap();
        super::run(&file, &dir, &[], device, 400, true).expect("applies");
        assert!(
            live(&dir),
            "a fact retracted last week read as already known"
        );
    }

    /// **A name restated is not a replay either** — #154, and the reason a
    /// re-import repairs nothing.
    ///
    /// `ArtistNamed` is last-writer-wins over one key, so "already in the log"
    /// is not "still the name". Dolly Parton was named correctly and then, by
    /// a joined credit the #145 guard could not see in Vorbis, renamed to
    /// `Dolly Parton, Linda Ronstadt & Emmylou Harris`. Both facts are in the
    /// log, so re-asserting the right one is the only way to outrank the
    /// wrong one — and the replay guard dropped it as a duplicate. That is why
    /// importing those files again reported `0 new fact(s)` and changed
    /// nothing.
    #[test]
    fn a_name_taken_over_by_a_later_one_can_be_restated() {
        use crate::model::entity::Artist;

        let dir = std::env::temp_dir().join("gmuse-apply-rename");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let device = Id::<Device>::derived(b"laptop");
        let dolly = Id::<Artist>::derived(b"dolly parton");
        let named =
            |name: &str| format!(r#"{{"fact":"named","artist":"{dolly}","name":"{name}"}}"#);
        let file = dir.join("names.jsonl");
        let now = |dir: &std::path::Path| {
            let (log, _) = Instance::open(dir, device, 0);
            log.catalogue().artist_names.get(&dolly).cloned()
        };

        // **Both names arrive by import**, which is how they did: the plain
        // one from her own albums, then the three-way credit from *Trio*, in a
        // Vorbis tag where the #145 guard could not see the joiner.
        {
            let (mut log, _) = Instance::open(&dir, device, 0);
            log.rename_artist(100, dolly, "Dolly Parton");
            log.rename_artist(200, dolly, "Dolly Parton, Linda Ronstadt & Emmylou Harris");
            log.flush(&dir).expect("writes");
        }
        assert_eq!(
            now(&dir).as_deref(),
            Some("Dolly Parton, Linda Ronstadt & Emmylou Harris"),
            "the joined credit did not take the register over"
        );

        // **The repair**, which the replay guard used to swallow.
        std::fs::write(&file, format!("{}\n", named("Dolly Parton"))).unwrap();
        let outcome = super::run(&file, &dir, &[], device, 300, true).expect("applies");
        assert_eq!(outcome.fresh, 1, "the right name read as already known");
        assert_eq!(now(&dir).as_deref(), Some("Dolly Parton"));

        // And saying it once more, when it is already the answer, writes
        // nothing — a run over an unchanged library stays quiet.
        let again = super::run(&file, &dir, &[], device, 400, true).expect("applies");
        assert_eq!(again.fresh, 0, "a name already in force was written again");
    }

    /// A file on disk, and a log that has since identified it as something
    /// else — which is what a relink is.
    #[test]
    fn a_relinked_recording_is_held() {
        let dir = std::env::temp_dir().join("gmuse-apply-relink");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("01 Callisto.wav");
        crate::model::round_trip::write_wav(&file, 4410);
        crate::model::round_trip::write_tags(&file, "Callisto", "Bodzin", None);

        let device = Id::<Device>::derived(b"laptop");
        let scan = vec![crate::model::ingest::read(&file).expect("reads")];

        // Scanned once, then identified — so the recording the log names is
        // not the one this file's bytes would mint.
        let mut log = Instance::new(device);
        log.ingest(100, &scan);
        let encoding = *log.catalogue().encodings.keys().next().expect("a copy");
        let named = Basis::MusicBrainz(uuid::Uuid::from_u128(0xca11));
        let relinked = Id::of_basis(&named);
        // Built as records and merged, because `append` is the instance's own
        // and a relink arriving from elsewhere is exactly what this is about.
        let mut clock = crate::model::clock::HybridClock::new(device);
        let relink: std::collections::BTreeSet<crate::model::log::Record> = [
            crate::model::log::Fact::RecordingIdentified {
                recording: relinked,
                basis: named.clone(),
            },
            crate::model::log::Fact::EncodingBound {
                encoding,
                from: None,
                to: relinked,
            },
        ]
        .into_iter()
        .map(|fact| crate::model::log::Record {
            stamp: clock.stamp(200),
            fact,
        })
        .collect();
        log.merge(200, &relink);

        // **Through the function `run` uses**, so this tests the code and
        // not a copy of it. The first draft built the `Shelf` by hand and
        // passed while `run` still scanned first.
        let library = super::shelf(&log, std::slice::from_ref(&dir), device, 300);
        assert!(
            library.holds(Subject::Recording(relinked)),
            "a proposal about the relinked recording would be refused as unknown"
        );

        // And scanning first is the bug, which is what makes this a test: the
        // bytes re-mint a different id and the log's is nowhere.
        let mut wrong = Instance::new(device);
        crate::model::populate::populate(&mut wrong, std::slice::from_ref(&dir), 300);
        let scanned_only = Shelf {
            disk: wrong.catalogue(),
            log: log.catalogue(),
        };
        assert!(
            !scanned_only.holds(Subject::Recording(relinked)),
            "scanning first should re-mint from the bytes; if it no longer \
             does, this test has stopped saying anything"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
