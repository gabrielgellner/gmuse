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

    // What the disk says exists, which is what "holds" is asking about.
    let (scanned, _) = crate::model::populate::read_library("apply", roots, now);
    let held = scanned.catalogue();
    let holds = |subject: Subject| match subject {
        Subject::Encoding(id) => held.encodings.contains_key(&id),
        Subject::Recording(id) => held.recordings.contains_key(&id),
    };

    let (mut log, _) = Instance::open(data_dir, device, now);
    let (proposed, report) = proposal::parse(&text, device, now, holds);

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
