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
    disk: crate::projection::catalogue::Catalogue,
    log: crate::projection::catalogue::Catalogue,
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
            //
            // **Or a disc this library has already seated a copy on** — #170.
            // The rule above reads the *tags*, so a tracklist could only ever
            // describe a pressing the files already named — which is how one
            // folder of eighteen Adele files, assembled from three sources,
            // stayed spread across the three pressings its tags happened to
            // mention while MusicBrainz had a single eighteen-track edition
            // that seats all of them. `Placed` is how a copy reaches such a
            // pressing, and once one has, the library has plainly seen the
            // disc and may be told how long it is.
            //
            // The safety property is unchanged: a tracklist for a record
            // nothing here connects to is still refused. What is admitted is a
            // record something here already sits on.
            // **A pressing the library already holds** — #204. The
            // identification derives its id from the mbid, so this arm is
            // where a tag naming a different edition is refused: three of the
            // 1,043 folders measured claim one the library never minted, and
            // they land here rather than in the pass.
            Subject::Release(id) => self.disk.releases.contains_key(&id),
            Subject::Medium(id) => {
                self.disk.media.contains_key(&id)
                    || self.log.tracks.values().any(|t| t.medium == id)
            }
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

    /// **What the bytes claimed, for a file the gate held back** — #160.
    ///
    /// The log first and the disk second, like every other answer here: a
    /// claim recorded on an earlier import is in the log, and one made by the
    /// scan that just ran is on the disk.
    fn claim(&self, encoding: Id<crate::model::entity::Encoding>) -> Option<(String, String)> {
        self.log
            .claims
            .get(&encoding)
            .or_else(|| self.disk.claims.get(&encoding))
            .cloned()
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

    fn sleeve(&self, track: Id<crate::model::entity::Track>) -> Option<String> {
        self.log
            .tracks
            .get(&track)
            .or_else(|| self.disk.tracks.get(&track))
            .and_then(|t| t.title_on_release.clone())
    }

    fn named(&self, artist: Id<crate::model::entity::Artist>) -> Option<String> {
        self.log
            .artist_names
            .get(&artist)
            .or_else(|| self.disk.artist_names.get(&artist))
            .cloned()
    }

    fn artist_named(&self, name: &str) -> Option<Id<crate::model::entity::Artist>> {
        let wanted = crate::model::ingest::normalise(name);
        // **Through the merges**, or a name whose two ids were joined last
        // week looks ambiguous today and mints a third.
        let found: std::collections::BTreeSet<_> = [&self.log, &self.disk]
            .into_iter()
            .flat_map(|cat| {
                cat.artist_names
                    .iter()
                    .filter(|(_, held)| crate::model::ingest::normalise(held) == wanted)
                    .map(|(id, _)| cat.artist_merges.resolve(*id))
            })
            .collect();
        match found.into_iter().collect::<Vec<_>>().as_slice() {
            [one] => Some(*one),
            [] => {
                // **The row this name would mint, if a merge already retired
                // it** — #310. A merge keeps the winner's name, so the loser's
                // name is in no register and the scan above misses it; the
                // caller would then mint that same loser and write
                // `ArtistNamed` for it, which the fold redirects to the winner.
                // Applying the Campfire Songs tracklist renamed Animal
                // Collective that way, and 57 files would have moved.
                let derived = Id::<crate::model::entity::Artist>::of_artist(None, name);
                [&self.log, &self.disk]
                    .into_iter()
                    .map(|cat| cat.artist_merges.resolve(derived))
                    .find(|&winner| winner != derived)
            }
            _ => None,
        }
    }
}

/// What a run did, and what it declined to do.
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
    /// Of `fresh`, how many are **another device's records**, carrying its own
    /// stamps (#338) — a Plex server's plays and ratings, bound for its own log.
    pub elsewhere: usize,
    /// **The log with the facts merged in, for the caller to land** — #218.
    ///
    /// `run` used to commit; it cannot any more, because a running engine may hold
    /// the log and only the caller knows whether to write or to hand the facts over.
    /// Untouched on a dry run, so landing it appends nothing.
    pub log: Instance,
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
        if self.elsewhere > 0 {
            write!(
                f,
                "\n{} of them another device's, to its own log",
                self.elsewhere
            )?;
        }
        if self.dry_run {
            write!(f, "\nnothing written — pass `--write` to apply")
        } else {
            write!(f, "\n{} to append", self.fresh)
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

fn paired(fact: &crate::facts::Fact) -> Option<(PairKey, bool)> {
    use crate::facts::Fact;
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
    // **Gated, for the reason `query` is** — #160. Under `Anything` this
    // scratch mints a recording from the bytes of a file the gate refused, and
    // `bound` then reports that invented row as what an identification is
    // moving *from* — so the proposal would carry a merge redirecting an id
    // that exists nowhere. Gated, `from` is `None`, which is the truth.
    crate::operations::populate::populate(
        &mut scanned,
        roots,
        now,
        crate::operations::populate::Admits::Identified,
        &mut crate::model::ingest::Reads::default(),
    );
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
///
/// **It does not write.** An earlier cut took an `Appending` and committed, which made
/// this the one operation that decided for itself where its facts went — and once a
/// running engine could be holding the log (#218), "where" has two answers and the
/// caller is the only place that can pick. So the merged facts come back in
/// [`Outcome::log`] and `main` lands them exactly as it lands every other operation's.
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
    let redundant = |fact: &crate::facts::Fact| match paired(fact) {
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
    let restates_the_name = |fact: &crate::facts::Fact| match fact {
        crate::facts::Fact::ArtistNamed { artist, name } => {
            Some(cat.artist_names.get(artist).map(String::as_str) == Some(name.as_str()))
        }
        _ => None,
    };

    // **Another device's records are judged as records** — #338. Their stamp is
    // their identity: two plays of one track are one fact told apart by when, so
    // the wire-form check below would keep the first and drop every later one.
    // A record already held is skipped; a *different* fact under a stamp already
    // held is a contradiction, refused rather than counted as a collision.
    let (foreign, proposed): (BTreeSet<_>, BTreeSet<_>) =
        proposed.into_iter().partition(|r| r.stamp.device != device);
    // The stamps held, once: a lookup per record rather than a scan of the log.
    let stamps: BTreeSet<crate::model::clock::Stamp> =
        log.records().iter().map(|r| r.stamp).collect();
    let mut taken_elsewhere = BTreeSet::new();
    let mut elsewhere_already = 0;
    let mut report = report;
    for record in foreign {
        if log.records().contains(&record) {
            elsewhere_already += 1;
        } else if stamps.contains(&record.stamp) {
            report.refused.push((
                0,
                format!("another fact already holds the stamp {:?}", record.stamp),
            ));
        } else {
            taken_elsewhere.insert(record);
        }
    }

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
    let already = proposed.len() - fresh.len() + elsewhere_already;

    if write {
        log.merge(now, &fresh);
        log.record_elsewhere(now, &taken_elsewhere);
    }

    Ok(Outcome {
        report,
        already,
        fresh: fresh.len() + taken_elsewhere.len(),
        elsewhere: taken_elsewhere.len(),
        log,
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

    /// Run an apply and put its facts on disk, which `run` no longer does itself.
    ///
    /// `run` hands back its log since #218, because whether the facts are committed
    /// here or handed to a running engine is the *caller's* question. A test is the
    /// only appender in its own temp directory, so it commits.
    fn landed(
        outcome: Result<super::Outcome, super::ApplyError>,
        dir: &std::path::Path,
    ) -> Result<super::Outcome, super::ApplyError> {
        let mut outcome = outcome?;
        let appending =
            crate::librarian::Appending::claim(dir).expect("a test has its log to itself");
        crate::librarian::commit(&mut outcome.log, dir, &appending)?;
        Ok(outcome)
    }

    use crate::model::basis::Basis;
    use crate::model::entity::Device;
    use crate::model::id::Id;
    use crate::model::instance::Instance;
    use crate::model::proposal::{Holdings, Subject};

    use super::Shelf;

    /// **A name the library already holds does not mint a second row** — #181.
    ///
    /// Every artist on the proposal wire is named rather than identified, and
    /// the minting behind it is `of_artist(None, name)`. An artist that exists
    /// only under a MusicBrainz-derived id has a *different* id, so crediting
    /// them by name used to land on a fresh row and hold the person twice. It
    /// happened once in ten while crediting five albums in !334, and the nine
    /// that were fine were fine only because an earlier merge had already
    /// joined their two ids.
    #[test]
    fn crediting_by_name_finds_the_artist_the_library_already_holds() {
        use crate::model::entity::Artist;
        use crate::projection::catalogue::Catalogue;

        let mb = |n: u128| uuid::Uuid::from_u128(n);
        let from_mb = Id::<Artist>::of_artist(Some(mb(0xd0117)), "Dolly Parton");
        let from_name = Id::<Artist>::of_artist(None, "Dolly Parton");
        assert_ne!(
            from_mb, from_name,
            "if these ever coincide this test says nothing"
        );

        let holding = |who: &[(Id<Artist>, &str)]| {
            let mut cat = Catalogue::default();
            for (id, name) in who {
                cat.artist_names.insert(*id, (*name).to_string());
            }
            Shelf {
                disk: Catalogue::default(),
                log: cat,
            }
        };

        let shelf = holding(&[(from_mb, "Dolly Parton")]);
        assert_eq!(
            shelf.artist_named("Dolly Parton"),
            Some(from_mb),
            "the row already there, not the one a name would mint"
        );
        // **After `normalise`**, which is what makes this a resolution rather
        // than a string compare — and exactly why the caller must not restate
        // `ArtistNamed` with the spelling it was handed.
        assert_eq!(shelf.artist_named("dolly  parton"), Some(from_mb));
        assert_eq!(
            shelf.artist_named("Linda Ronstadt"),
            None,
            "nobody holds it, so the caller mints — which is right"
        );

        // **A name two people share resolves to neither.** Choosing one is the
        // error nothing reports afterwards; minting a second row is one
        // `shelf artists` prints and one merge repairs. Same asymmetry the
        // rest of the model is built on.
        let twice = holding(&[
            (from_mb, "Dolly Parton"),
            (
                Id::<Artist>::of_artist(Some(mb(0xd0112)), "Dolly Parton"),
                "Dolly Parton",
            ),
        ]);
        assert_eq!(
            twice.artist_named("Dolly Parton"),
            None,
            "two people really do share a name"
        );
    }

    /// **A name whose row a merge retired finds the row it went into** — #310.
    ///
    /// Campfire Songs was merged into Animal Collective, which kept its own
    /// name, so no register says "Campfire Songs" any more. A lookup that
    /// only scanned names minted Campfire Songs' old id again, and the
    /// `ArtistNamed` that came with it was redirected to the winner and
    /// renamed Animal Collective.
    #[test]
    fn a_name_merged_away_finds_the_artist_it_was_merged_into() {
        use crate::model::entity::Artist;
        use crate::projection::catalogue::Catalogue;

        let winner =
            Id::<Artist>::of_artist(Some(uuid::Uuid::from_u128(0xac)), "Animal Collective");
        let retired = Id::<Artist>::of_artist(None, "Campfire Songs");

        let mut cat = Catalogue::default();
        cat.artist_names
            .insert(winner, "Animal Collective".to_string());
        cat.artist_merges
            .merge(retired, winner)
            .expect("a first merge is accepted");
        assert_eq!(
            cat.artist_merges.resolve(retired),
            winner,
            "the fixture must hold the merge, or this test says nothing"
        );
        let shelf = Shelf {
            disk: Catalogue::default(),
            log: cat,
        };

        assert_eq!(
            shelf.artist_named("Campfire Songs"),
            Some(winner),
            "the retired name's row resolves to where it went"
        );
        assert_eq!(
            shelf.artist_named("Sung Tongs"),
            None,
            "a name nobody merged is still a mint"
        );
    }

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
        landed(super::run(&file, &dir, &[], device, 100, true), &dir).expect("applies");
        assert!(live(&dir), "the edge did not land");

        // **In the same file**, which is what a curation pass emits when a
        // term moves and moves back.
        std::fs::write(&file, format!("{undo}\n{edge}\n")).unwrap();
        landed(super::run(&file, &dir, &[], device, 200, true), &dir).expect("applies");
        assert!(live(&dir), "a retraction two lines up swallowed the edge");

        // **And across runs**, where the retraction is history rather than
        // batch — the shape that actually bit.
        std::fs::write(&file, format!("{undo}\n")).unwrap();
        landed(super::run(&file, &dir, &[], device, 300, true), &dir).expect("applies");
        assert!(!live(&dir), "the retraction did not take");

        std::fs::write(&file, format!("{edge}\n")).unwrap();
        landed(super::run(&file, &dir, &[], device, 400, true), &dir).expect("applies");
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
        let outcome =
            landed(super::run(&file, &dir, &[], device, 300, true), &dir).expect("applies");
        assert_eq!(outcome.fresh, 1, "the right name read as already known");
        assert_eq!(now(&dir).as_deref(), Some("Dolly Parton"));

        // And saying it once more, when it is already the answer, writes
        // nothing — a run over an unchanged library stays quiet.
        let again = landed(super::run(&file, &dir, &[], device, 400, true), &dir).expect("applies");
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
        let relink: std::collections::BTreeSet<crate::facts::Record> = [
            crate::facts::Fact::RecordingIdentified {
                recording: relinked,
                basis: named.clone(),
            },
            crate::facts::Fact::EncodingBound {
                encoding,
                from: None,
                to: relinked,
            },
        ]
        .into_iter()
        .map(|fact| crate::facts::Record {
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
        crate::operations::populate::populate(
            &mut wrong,
            std::slice::from_ref(&dir),
            300,
            crate::operations::populate::Admits::Anything,
            &mut crate::model::ingest::Reads::default(),
        );
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

    // --- Another device's records — #337, #338 ---------------------------------

    /// A library holding one recording — a real file, ingested and on disk, as
    /// `holds` asks the folded disk — and the ids a Plex test needs.
    fn plex_library(
        name: &str,
    ) -> (
        std::path::PathBuf,
        Id<Device>,
        Id<crate::model::entity::Recording>,
    ) {
        let dir = std::env::temp_dir().join(format!("gmuse-apply-plex-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("01 Callisto.wav");
        crate::model::round_trip::write_wav(&file, 4410);
        crate::model::round_trip::write_tags(&file, "Callisto", "Bodzin", None);
        let device = Id::<Device>::derived(b"laptop");
        let mut log = Instance::new(device);
        log.ingest(100, &[crate::model::ingest::read(&file).expect("reads")]);
        log.flush(&dir).expect("writes");
        let recording = *log
            .catalogue()
            .recordings
            .keys()
            .next()
            .expect("a recording");
        (dir, device, recording)
    }

    const PLEX: &str = "plex:0c1eda1ba823bfe1fcce17edfd592f48560c435d";

    fn played(recording: Id<crate::model::entity::Recording>, at_ms: i64, seq: u32) -> String {
        format!(
            r#"{{"fact":"played","recording":"{recording}","elsewhere":{{"device":"{PLEX}","at_ms":{at_ms},"seq":{seq}}}}}"#
        )
    }

    fn plays(
        dir: &std::path::Path,
        device: Id<Device>,
        recording: Id<crate::model::entity::Recording>,
    ) -> usize {
        let (log, _) = Instance::open(dir, device, 0);
        log.records()
            .iter()
            .filter(|r| matches!(r.fact, crate::facts::Fact::RecordingPlayed { recording: x, .. } if x == recording))
            .count()
    }

    /// **A Plex play lands in Plex's log, under Plex's time** — not this device's.
    #[test]
    fn a_play_elsewhere_lands_in_that_devices_log_at_its_time() {
        let (dir, device, recording) = plex_library("lands");
        let file = dir.join("plex.jsonl");
        std::fs::write(&file, played(recording, 1_785_341_390_000, 41) + "\n").unwrap();
        let outcome = landed(
            super::run(&file, &dir, &[], device, 2_000_000_000_000, true),
            &dir,
        )
        .expect("applies");
        assert_eq!(outcome.elsewhere, 1);

        let plex = Id::<Device>::derived(PLEX.as_bytes());
        let theirs = crate::model::durable::log_path(&dir, plex);
        let ours = crate::model::durable::log_path(&dir, device);
        assert!(
            std::fs::read_to_string(&theirs)
                .expect("Plex has a log")
                .contains("RecordingPlayed")
        );
        assert!(
            !std::fs::read_to_string(&ours)
                .unwrap()
                .contains("RecordingPlayed"),
            "instrument passed, claim failed: the play went into this device's log"
        );
        let (log, _) = Instance::open(&dir, device, 0);
        let play = log
            .records()
            .iter()
            .find(|r| matches!(r.fact, crate::facts::Fact::RecordingPlayed { .. }))
            .unwrap();
        assert_eq!(
            play.stamp.wall, 1_785_341_390_000,
            "stamped with when it was played, not when it was read"
        );
        assert_eq!(play.stamp.device, plex);
    }

    /// **Reading Plex twice cannot count a play twice** — the stamp is the play.
    #[test]
    fn the_same_play_applied_twice_is_one_play() {
        let (dir, device, recording) = plex_library("twice");
        let file = dir.join("plex.jsonl");
        std::fs::write(&file, played(recording, 1_000, 7) + "\n").unwrap();
        landed(super::run(&file, &dir, &[], device, 2_000, true), &dir).expect("applies");
        assert_eq!(
            plays(&dir, device, recording),
            1,
            "instrument: the first apply recorded it"
        );
        let again =
            landed(super::run(&file, &dir, &[], device, 3_000, true), &dir).expect("applies");
        assert_eq!(again.fresh, 0);
        assert_eq!(again.already, 1);
        assert_eq!(plays(&dir, device, recording), 1);
    }

    /// **Two plays of one track are two plays** — the fact is identical, the stamps are not.
    /// The wire-form replay check, which a local proposal still gets, would keep one.
    #[test]
    fn two_plays_of_one_track_elsewhere_are_two_plays() {
        let (dir, device, recording) = plex_library("two");
        let file = dir.join("plex.jsonl");
        std::fs::write(
            &file,
            played(recording, 1_000, 7) + "\n" + &played(recording, 9_000, 8) + "\n",
        )
        .unwrap();
        landed(super::run(&file, &dir, &[], device, 10_000, true), &dir).expect("applies");
        assert_eq!(plays(&dir, device, recording), 2);
        std::fs::write(&file, played(recording, 20_000, 9) + "\n").unwrap();
        landed(super::run(&file, &dir, &[], device, 30_000, true), &dir).expect("applies");
        assert_eq!(
            plays(&dir, device, recording),
            3,
            "and a later run adds the new one, not a count of facts"
        );
    }

    /// **A rating is judged as a record, not by whether its fact was ever said** —
    /// rated 5, then 4, then 5 in Plexamp: the last must land.
    #[test]
    fn a_rating_elsewhere_lands_though_its_fact_is_in_the_history() {
        let (dir, device, recording) = plex_library("rerated");
        let file = dir.join("ratings.jsonl");
        let local =
            |stars: u8| format!(r#"{{"fact":"rated","recording":"{recording}","stars":{stars}}}"#);
        std::fs::write(&file, local(5) + "\n").unwrap();
        landed(super::run(&file, &dir, &[], device, 1_000, true), &dir).expect("applies");
        std::fs::write(&file, local(4) + "\n").unwrap();
        landed(super::run(&file, &dir, &[], device, 2_000, true), &dir).expect("applies");
        let stars = |dir: &std::path::Path| {
            Instance::open(dir, device, 0)
                .0
                .catalogue()
                .ratings
                .get(&recording)
                .copied()
        };
        assert_eq!(stars(&dir), Some(4), "instrument: the library now says 4");

        let plex = format!(
            r#"{{"fact":"rated","recording":"{recording}","stars":5,"elsewhere":{{"device":"{PLEX}","at_ms":3000,"seq":1}}}}"#
        );
        std::fs::write(&file, plex + "\n").unwrap();
        landed(super::run(&file, &dir, &[], device, 4_000, true), &dir).expect("applies");
        assert_eq!(
            stars(&dir),
            Some(5),
            "the later rating, made in Plexamp, wins"
        );
    }

    /// **A stamp naming this device is refused** — ours come from our own clock.
    #[test]
    fn a_stamp_naming_this_device_is_refused() {
        let (dir, device, recording) = plex_library("ours");
        let file = dir.join("forged.jsonl");
        let forged = format!(
            r#"{{"fact":"played","recording":"{recording}","elsewhere":{{"device":"laptop","at_ms":1000,"seq":1}}}}"#
        );
        std::fs::write(&file, played(recording, 1_000, 1) + "\n" + &forged + "\n").unwrap();
        let outcome =
            landed(super::run(&file, &dir, &[], device, 2_000, true), &dir).expect("applies");
        assert_eq!(outcome.elsewhere, 1, "instrument: the Plex line was taken");
        assert_eq!(outcome.report.refused.len(), 1);
        assert_eq!(plays(&dir, device, recording), 1);
    }

    /// **Another fact under a stamp already held is a contradiction**, refused rather
    /// than kept as a collision.
    #[test]
    fn a_different_fact_under_a_held_stamp_is_refused() {
        let (dir, device, recording) = plex_library("clash");
        let file = dir.join("plex.jsonl");
        std::fs::write(&file, played(recording, 1_000, 7) + "\n").unwrap();
        landed(super::run(&file, &dir, &[], device, 2_000, true), &dir).expect("applies");
        let rated = format!(
            r#"{{"fact":"rated","recording":"{recording}","stars":3,"elsewhere":{{"device":"{PLEX}","at_ms":1000,"seq":7}}}}"#
        );
        std::fs::write(&file, rated + "\n").unwrap();
        let outcome =
            landed(super::run(&file, &dir, &[], device, 3_000, true), &dir).expect("applies");
        assert_eq!(outcome.report.refused.len(), 1);
        assert_eq!(outcome.fresh, 0);
        assert_eq!(
            plays(&dir, device, recording),
            1,
            "instrument: the first record is still there"
        );
    }
}
