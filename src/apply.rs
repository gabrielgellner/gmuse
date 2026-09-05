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
}

/// What a run did, and what it declined to do.
#[derive(Debug)]
pub struct Outcome {
    pub report: Report,
    /// Facts already in the log, so a second run is not a second play count.
    pub already: usize,
    /// Records actually appended. Zero on a dry run.
    pub written: usize,
    pub dry_run: bool,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} proposals — {} would apply, {} already known, {} unknown to this library, {} malformed",
            self.report.applied + self.report.unknown.len() + self.report.malformed.len(),
            self.report.applied.saturating_sub(self.already),
            self.already,
            self.report.unknown.len(),
            self.report.malformed.len(),
        )?;
        if self.dry_run {
            write!(f, "\nnothing written — pass `--write` to apply")
        } else {
            write!(f, "\n{} appended", self.written)
        }
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
    let fresh: BTreeSet<_> = proposed
        .iter()
        .filter(|r| serde_json::to_string(&r.fact).is_ok_and(|encoded| !seen.contains(&encoded)))
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
