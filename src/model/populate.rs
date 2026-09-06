//! Filling the library from a disk — issue #80.
//!
//! The step that makes every other migration possible: until the model holds
//! the real files, nothing in `src/ui/` can read from it.
//!
//! This is not an addition to `scan.rs` — it is what replaces it (#86), so its
//! cost is to be compared against the existing scan's rather than added to it.
//! Both open every changed file exactly once, and for the same reason: that is
//! where the audio hash has to be computed, and the file is already open.
//!
//! ## Every file is accounted for
//!
//! A scan that finds 5774 files and produces 5700 encodings has lost 74 of
//! them, and a report that does not say so is how a library quietly develops
//! holes. [`Report`] adds up: `files == ingested + unreadable.len()`, asserted
//! rather than hoped for.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::collections::BTreeMap;

use crate::model::entity::{Encoding, Recording};
use crate::model::id::Id;
use crate::model::ingest;
use crate::model::instance::Instance;

/// What a pass over the disk found.
#[derive(Debug, Default)]
pub struct Report {
    /// Audio files seen on disk.
    pub files: usize,
    /// Files that became a recording and an encoding.
    pub ingested: usize,
    /// Files that could not be read as audio at all. **Not** files with bad
    /// metadata — those are ingested and marked, because ingest is total.
    pub unreadable: Vec<(PathBuf, String)>,
    /// Ingested with a MusicBrainz id to converge on.
    pub confirmed: usize,
    /// Ingested with nothing to converge on — the residue §11 leaves to the
    /// collision.
    pub provisional: usize,
    /// **Audio this library has never seen** — #62.
    ///
    /// Reported, never catalogued. A file appearing in a watched root is not
    /// a declaration that it belongs here, and `library-api.md` §2 lists
    /// exactly that inference among the ones that were wrong: cataloguing on
    /// sight is how a quarantine folder becomes part of the library and how a
    /// half-copied download becomes a row.
    ///
    /// `gmuse shelf import` is the declaration. This is the offer.
    pub unknown: Vec<PathBuf>,
    /// Files whose bytes another file already had.
    ///
    /// **A scanner observation, not a library fact.** A holding answers "can
    /// this device open this encoding, and how", and two identical files give
    /// one answer — the second path buys no capability, so `held` is a map and
    /// the second copy has nowhere to go. That is the right shape: a duplicate
    /// is a filesystem accident, and modelling it would make every consumer
    /// decide what two locators for one encoding *mean*.
    ///
    /// What it must not be is **silent**. The drop was invisible until a
    /// duplicated album cost the migration nine play events, and the only way
    /// the one duplicate in this library came to light was somebody hashing
    /// 6,500 files for an unrelated reason. Said here, at the moment it is
    /// seen, by the only thing that can see it.
    pub duplicates: Vec<(PathBuf, PathBuf)>,
    pub elapsed: Duration,
    /// What each scanned path turned out to be.
    ///
    /// Read back out of the catalogue rather than recomputed, so it cannot
    /// drift from the ladder in `Instance::ingest`. A migration needs exactly
    /// this — "what is this path?" is the one question the old log cannot
    /// answer about itself — and re-deriving it here is how two answers to one
    /// question get into the codebase.
    pub identified: BTreeMap<PathBuf, Id<Recording>>,
    /// Recordings in **no collection**, by the folder they were found under.
    ///
    /// **A suggestion, and never a write** (#119). The folder is evidence a
    /// person left, read once and discarded — a scan that filed by folder
    /// would make the filesystem authoritative again, which is exactly what
    /// #90 and #94 took away, and moving a file would silently re-file it.
    ///
    /// So this is the review queue: drop a new album into `Classical/` and the
    /// next scan says twelve things landed there and belong to nothing, and
    /// `gmuse shelf unfiled` turns that into a proposal you read before
    /// applying.
    pub unfiled: BTreeMap<String, Vec<Id<Recording>>>,
}

impl Report {
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — an invariant a test asserts, not an answer anything needs at runtime"
    )]
    /// Whether every file found is accounted for.
    ///
    /// The one invariant worth asserting on a scan: nothing may be dropped
    /// silently, which is how a library develops holes nobody notices.
    pub fn balances(&self) -> bool {
        self.files == self.ingested + self.unreadable.len()
            && self.ingested == self.confirmed + self.provisional
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} files in {:.1}s — {} ingested ({} confirmed, {} provisional), {} unreadable",
            self.files,
            self.elapsed.as_secs_f32(),
            self.ingested,
            self.confirmed,
            self.provisional,
            self.unreadable.len()
        )?;
        // Named, not counted. A count says "you have a problem somewhere in
        // 6,500 files"; the pair says which one to delete.
        // Named by folder and counted: the folder is the suggestion, and the
        // count is what makes it worth reading.
        for (folder, held) in &self.unfiled {
            write!(
                f,
                "\n  {} under {folder}/ are in no collection — `gmuse shelf unfiled` proposes filing them",
                held.len()
            )?;
        }
        for (dropped, kept) in &self.duplicates {
            write!(
                f,
                "\n  duplicate: {}\n     same bytes as {}",
                dropped.display(),
                kept.display()
            )?;
        }
        Ok(())
    }
}

/// Every audio file under these roots.
///
/// Nested roots are pruned, because a root under another root has every one of
/// its files walked twice — read twice on a cold scan and counted twice in the
/// report, which is how a 311-track library came to announce 622 (#21).
pub fn audio_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in crate::scan::prune_nested(roots) {
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file() && crate::scan::is_audio(entry.path()) {
                found.push(entry.path().to_path_buf());
            }
        }
    }
    found.sort();
    found
}

/// Read a disk into an instance's log.
///
/// Ingest is total, so a file with unreadable *metadata* still becomes a
/// recording; only a file that is not audio at all is left out, and it is left
/// out loudly.
///
/// Finishes by publishing this device's holdings **whole** (#71), which is what
/// makes a file deleted since the last pass go away rather than being
/// resurrected by a stale replica.
/// **Reconcile what this device holds, and read no tags** — #62.
///
/// The scan `library-api.md` §3 describes, and the inversion this issue is
/// about. A pass over the disk used to catalogue everything it found, which
/// made "a file is in a watched root" mean "this is part of my library" —
/// an inference, and one of the five §2 lists as wrong.
///
/// So: hash the audio, because that is what says *which* encoding a file is;
/// hold what the library already knows; and **report what it does not**.
/// Nothing is read from a tag, which is what "tags are a serialization" has
/// to mean if it means anything — an external retag changes nothing here, and
/// a rescan stops opening 6,500 files to ask what they claim about
/// themselves.
pub fn reconcile(instance: &mut Instance, roots: &[PathBuf], now: i64) -> Report {
    let started = Instant::now();
    let paths = audio_files(roots);
    let mut report = Report {
        files: paths.len(),
        ..Default::default()
    };
    let known = instance.catalogue();
    let mut held = BTreeMap::new();

    for path in paths {
        let audio = match crate::audio_id::of(&path) {
            Ok(audio) => audio,
            Err(e) => {
                report.unreadable.push((path, e.to_string()));
                continue;
            }
        };
        let encoding = Id::<Encoding>::of_audio(&audio);
        // **Known by its bytes, not by where it sits.** A file that moved is
        // the same encoding at a new locator, which is why a move needs no
        // operation and cannot lose anything (#62, `library-api.md` §2.1).
        if !known.encodings.contains_key(&encoding) {
            report.unknown.push(path);
            continue;
        }
        if let Some(first) = held.insert(encoding, path.display().to_string()) {
            report.duplicates.push((path, PathBuf::from(first)));
        }
        report.ingested += 1;
    }

    instance.assert_holdings(now, held);
    report.elapsed = started.elapsed();
    report
}

/// Read a disk into an instance's log, cataloguing whatever it finds.
///
/// **What `import` does, and no longer what a scan does** (#62). Given paths a
/// person named, this is the declaration that they belong here.
pub fn populate(instance: &mut Instance, roots: &[PathBuf], now: i64) -> Report {
    let started = Instant::now();
    let paths = audio_files(roots);
    let mut report = Report {
        files: paths.len(),
        ..Default::default()
    };

    let mut scanned = Vec::with_capacity(paths.len());
    let mut held = std::collections::BTreeMap::new();
    // Path to encoding, kept **per path** rather than read back out of
    // `held`. Two files with identical bytes mint one encoding, so a map keyed
    // by encoding holds one of their paths and forgets the other — which is
    // not hypothetical: the migration lost nine play events to a duplicated
    // album before this existed, and reported them as unidentifiable files
    // that were sitting on disk under the exact names it had just printed.
    let mut minted: Vec<(PathBuf, Id<Encoding>)> = Vec::with_capacity(paths.len());

    for path in paths {
        match ingest::read(&path) {
            Ok(s) => {
                if s.identity.is_some() {
                    report.confirmed += 1;
                } else {
                    report.provisional += 1;
                }
                let encoding = Id::<Encoding>::of_audio(&s.audio);
                if let Some(first) = held.insert(encoding, path.display().to_string()) {
                    // The map kept the later path; the report names both, so
                    // "which one do I delete" is answerable.
                    report.duplicates.push((path.clone(), PathBuf::from(first)));
                }
                minted.push((path, encoding));
                scanned.push(s);
                report.ingested += 1;
            }
            Err(e) => report.unreadable.push((path, e.to_string())),
        }
    }

    instance.ingest(now, &scanned);

    // Which recording each file became, taken from the catalogue the ingest
    // just produced. Not recomputed: the ladder that chooses a recording id
    // lives in one place and this reads its answer.
    let catalogue = instance.catalogue();
    for (path, encoding) in minted {
        if let Some(recording) = catalogue.encodings.get(&encoding).and_then(|e| e.recording) {
            report.identified.insert(path, recording);
        }
    }

    instance.assert_holdings(now, held);

    // **After the ingest, so the collections are the ones this fold knows.**
    // Asked of the catalogue rather than recomputed, for the reason
    // `identified` is: two answers to one question is how they drift.
    let cat = instance.catalogue();
    let filed: std::collections::BTreeSet<Id<Recording>> =
        cat.entries.iter().map(|(_, _, r)| *r).collect();
    for (path, recording) in &report.identified {
        if filed.contains(recording) {
            continue;
        }
        if let Some(folder) = folder_of(path, roots) {
            report.unfiled.entry(folder).or_default().push(*recording);
        }
    }

    report.elapsed = started.elapsed();
    report
}

/// The first folder below whichever root holds this path.
///
/// `~/MusicLibrary/Dance/Akufen/…` under root `~/MusicLibrary` is `Dance`. A
/// file sitting directly in a root is under no folder and gets none — there is
/// nothing to suggest.
fn folder_of(path: &Path, roots: &[PathBuf]) -> Option<String> {
    for root in roots {
        if let Ok(rest) = path.strip_prefix(root) {
            let mut parts = rest.components();
            let first = parts.next()?;
            // The last component is the file itself, so a path with only one
            // component is a file in the root rather than a file in a folder.
            parts.next()?;
            return Some(first.as_os_str().to_string_lossy().into_owned());
        }
    }
    None
}

/// A convenience for the common case: one device, one set of roots.
pub fn read_library(device_name: &str, roots: &[PathBuf], now: i64) -> (Instance, Report) {
    let mut instance = Instance::new(Id::derived(device_name.as_bytes()));
    let report = populate(&mut instance, roots, now);
    (instance, report)
}

#[expect(
    dead_code,
    reason = "exposed so a caller can ask without walking, and no caller has"
)]
/// Whether a path is one this pass would look at. Exposed so a caller can ask
/// without walking.
pub fn is_audio(path: &Path) -> bool {
    crate::scan::is_audio(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use crate::model::round_trip::{write_tags, write_wav};
    use crate::model::seam::Librarian;

    /// **A scan offers; it does not catalogue** — #62, and the inversion the
    /// issue is about.
    ///
    /// A file appearing in a watched root is not a declaration that it belongs
    /// here. `library-api.md` §2 lists cataloguing on sight among the
    /// inferences that were wrong, and this is the one that made a quarantine
    /// folder part of the library.
    #[test]
    fn a_scan_reports_audio_it_does_not_know_rather_than_taking_it_in() {
        let root = std::env::temp_dir().join("gmuse-reconcile-unknown");
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("An Artist").join("A Record");
        std::fs::create_dir_all(&dir).unwrap();
        let known = dir.join("01 Known.wav");
        let stranger = dir.join("02 Stranger.wav");
        write_wav(&known, 4410);
        write_tags(&known, "Known", "An Artist", None);
        write_wav(&stranger, 5510);
        write_tags(&stranger, "Stranger", "An Artist", None);

        // One of them is imported — declared — and the other is merely there.
        let mut i = Instance::new(crate::fixture::device());
        populate(&mut i, std::slice::from_ref(&known), 100);

        let report = reconcile(&mut i, std::slice::from_ref(&root), 200);
        assert_eq!(report.files, 2);
        assert_eq!(
            report.unknown,
            vec![stranger.clone()],
            "the file nobody declared was catalogued, or the declared one was not held"
        );
        assert_eq!(report.ingested, 1, "the known file is a holding");

        let l = Librarian::from_catalogue(&i.catalogue());
        let titles: BTreeSet<String> = crate::model::rows::Listing::rows(&l)
            .into_iter()
            .filter_map(|id| crate::model::rows::Listing::listed(&l, id))
            .map(|r| r.title.to_string())
            .collect();
        assert_eq!(
            titles,
            BTreeSet::from(["Known".to_string()]),
            "a scan put a row in the library that nobody imported"
        );

        // And importing it is the answer to the offer.
        populate(&mut i, &report.unknown, 300);
        let after = reconcile(&mut i, std::slice::from_ref(&root), 400);
        assert!(after.unknown.is_empty(), "still unknown after an import");
        assert_eq!(after.ingested, 2);
    }

    /// **An external retag changes nothing**, which is what "tags are a
    /// serialization" has to mean if it means anything — #62, `library-api.md`
    /// §3.
    ///
    /// The scan reads no tags, so a tool that rewrites every `TCON` in the
    /// library is invisible to it. The old pass re-read them and would have
    /// taken the new title as the truth.
    #[test]
    fn retagging_a_file_behind_the_librarys_back_changes_nothing() {
        let root = std::env::temp_dir().join("gmuse-reconcile-retag");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let at = root.join("01 Callisto.wav");
        write_wav(&at, 4410);
        write_tags(&at, "Callisto", "Stephan Bodzin", None);

        let mut i = Instance::new(crate::fixture::device());
        populate(&mut i, std::slice::from_ref(&root), 100);

        // Somebody else rewrites the tags. The audio is untouched.
        write_tags(&at, "Something Else Entirely", "Not Bodzin", None);
        let report = reconcile(&mut i, std::slice::from_ref(&root), 200);
        assert!(
            report.unknown.is_empty(),
            "a retag made the file a stranger — the audio did not change"
        );

        let l = Librarian::from_catalogue(&i.catalogue());
        let titles: Vec<String> = crate::model::rows::Listing::rows(&l)
            .into_iter()
            .filter_map(|id| crate::model::rows::Listing::listed(&l, id))
            .map(|r| r.title.to_string())
            .collect();
        assert_eq!(
            titles,
            vec!["Callisto".to_string()],
            "the library read a tag it was told never to read again"
        );
    }

    /// **A file that moved is the same file**, so a move needs no operation —
    /// #62, and the case `library-api.md` §2.1 dissolved rather than built.
    ///
    /// Identity is the audio, and the locator is republished whole every scan.
    #[test]
    fn moving_a_file_costs_nothing_because_identity_is_the_audio() {
        let root = std::env::temp_dir().join("gmuse-reconcile-move");
        let _ = std::fs::remove_dir_all(&root);
        let from = root.join("before");
        let to = root.join("after");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let was = from.join("01 Callisto.wav");
        write_wav(&was, 4410);
        write_tags(&was, "Callisto", "Stephan Bodzin", None);

        let mut i = Instance::new(crate::fixture::device());
        populate(&mut i, std::slice::from_ref(&root), 100);
        let recording =
            crate::model::rows::Listing::rows(&Librarian::from_catalogue(&i.catalogue()))[0];

        let now = to.join("07 Renamed Entirely.wav");
        std::fs::rename(&was, &now).unwrap();
        let report = reconcile(&mut i, std::slice::from_ref(&root), 200);
        assert!(report.unknown.is_empty(), "a moved file became a stranger");

        let mut l = Librarian::from_catalogue(&i.catalogue());
        // Locators are a fact about *this* machine, so the librarian has to be
        // told which one it is before it can say where anything is.
        l.here(crate::fixture::device());
        assert_eq!(
            crate::model::rows::Listing::rows(&l),
            vec![recording],
            "the row changed identity when the file changed place"
        );
        assert_eq!(
            l.where_held(recording).as_deref(),
            Some(now.display().to_string().as_str()),
            "the locator did not follow the file"
        );
    }

    /// **The folder proposes; nothing files by it** — #119.
    ///
    /// Drop an album into `Classical/` and the next scan says twelve things
    /// landed there and belong to nothing. It is a review queue, not a rule:
    /// a scan that filed by folder would make the filesystem authoritative
    /// again, which is exactly what #90 and #94 took away, and moving a file
    /// would silently re-file it.
    #[test]
    fn the_folder_a_file_landed_in_is_a_suggestion() {
        let root = std::env::temp_dir().join("gmuse-populate-unfiled");
        let _ = std::fs::remove_dir_all(&root);
        for (folder, title) in [("Classical", "Nocturne"), ("Dance", "Kompressor")] {
            let dir = root.join(folder).join("An Artist");
            std::fs::create_dir_all(&dir).unwrap();
            let at = dir.join(format!("01 {title}.wav"));
            write_wav(&at, 4410 + title.len() * 7);
            write_tags(&at, title, "An Artist", None);
        }
        // Directly in the root, so there is no folder to suggest.
        write_wav(&root.join("loose.wav"), 5000);
        write_tags(&root.join("loose.wav"), "Loose", "An Artist", None);

        let mut i = Instance::new(Id::derived(b"test"));
        let report = populate(&mut i, std::slice::from_ref(&root), 100);
        assert_eq!(
            report.unfiled.keys().collect::<Vec<_>>(),
            ["Classical", "Dance"],
            "a file in the root has no folder to suggest"
        );
        assert_eq!(report.unfiled["Classical"].len(), 1);
        assert!(
            format!("{report}").contains("under Classical/ are in no collection"),
            "{report}"
        );

        // **File one, and it stops being suggested.** The suggestion is about
        // what nobody has decided, not about where the bytes are.
        let recording = report.unfiled["Classical"][0];
        let list = Id::<crate::model::entity::Collection>::of_name("Classical");
        i.name_collection(200, list, "Classical", false);
        i.add_to_collection(200, list, recording, crate::model::ord::OrdKey::first());

        let again = populate(&mut i, std::slice::from_ref(&root), 300);
        assert_eq!(
            again.unfiled.keys().collect::<Vec<_>>(),
            ["Dance"],
            "a filed recording is still being suggested"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn library(name: &str, files: &[(&str, Option<&str>)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gmuse-populate-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (n, (title, mbid)) in files.iter().enumerate() {
            let p = dir.join(format!("{:02} {title}.wav", n + 1));
            write_wav(&p, 4410 + n * 7);
            write_tags(&p, title, "Stephan Bodzin", *mbid);
        }
        dir
    }

    /// **Two files, identical bytes, and both paths still answer.**
    ///
    /// Duplicates mint one encoding, so a path map read back out of an
    /// encoding-keyed collection keeps one of them and forgets the other. The
    /// migration found this for real: nine play events on a duplicated album
    /// came back as unidentifiable files that were sitting on disk under the
    /// exact names the report had just printed.
    #[test]
    fn a_duplicated_file_identifies_under_both_of_its_paths() {
        let dir = std::env::temp_dir().join("gmuse-populate-duplicate");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (one, two) = (dir.join("01 Callisto.wav"), dir.join("1 - Callisto.wav"));
        for path in [&one, &two] {
            write_wav(path, 4410);
            write_tags(path, "Callisto", "Stephan Bodzin", None);
        }

        let (_, report) = read_library("laptop", &[dir], 100);
        assert_eq!(report.ingested, 2, "both files were read");
        assert_eq!(
            report.identified.get(&one),
            report.identified.get(&two),
            "same bytes, same recording"
        );
        assert!(
            report.identified.contains_key(&one),
            "and the first path answers"
        );
        assert!(
            report.identified.contains_key(&two),
            "and so does the second"
        );
    }

    /// **The second copy is named, not silently dropped.**
    ///
    /// A holding answers "can this device open this encoding, and how", and
    /// two identical files give one answer — so `held` is keyed by encoding
    /// and the second path has nowhere to go. That is the right shape: a
    /// duplicate is a filesystem accident, and modelling it would leave every
    /// consumer deciding what two locators for one encoding *mean*.
    ///
    /// What it must not be is invisible. The one duplicate in the real library
    /// came to light only because somebody hashed 6,500 files for an unrelated
    /// reason, and a proxy of (size, duration) had put the figure at 28.
    #[test]
    fn a_duplicate_is_reported_with_both_of_its_paths() {
        let dir = std::env::temp_dir().join("gmuse-populate-duplicate-said");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (one, two) = (dir.join("01 Callisto.wav"), dir.join("01 Callisto (1).wav"));
        for path in [&one, &two] {
            write_wav(path, 4410);
            write_tags(path, "Callisto", "Stephan Bodzin", None);
        }
        // A third file, different bytes, that must not be reported.
        let other = dir.join("02 Luna.wav");
        write_wav(&other, 8820);
        write_tags(&other, "Luna", "Stephan Bodzin", None);

        let (_, report) = read_library("laptop", &[dir], 100);

        assert_eq!(report.duplicates.len(), 1, "one pair, and only the pair");
        let (dropped, kept) = &report.duplicates[0];
        assert_ne!(dropped, kept, "two paths, not one named twice");
        assert_eq!(
            [dropped.clone(), kept.clone()]
                .iter()
                .collect::<BTreeSet<_>>(),
            [&one, &two].into_iter().collect::<BTreeSet<_>>(),
            "and both of them, so the answer to \"which do I delete\" is in it"
        );
        let said = report.to_string();
        assert!(
            said.contains("01 Callisto (1).wav"),
            "named in the report: {said}"
        );
        assert!(!said.contains("Luna"), "and nothing else is: {said}");
    }

    /// **Every file is accounted for.** A scan that loses files without saying
    /// so is how a library develops holes nobody notices.
    #[test]
    fn the_report_adds_up() {
        let dir = library(
            "adds-up",
            &[
                ("Callisto", Some("183105b5-0000-4000-8000-000000000000")),
                ("Luna", None),
                ("Atlas", Some("859d0860-0000-4000-8000-000000000000")),
            ],
        );
        std::fs::write(dir.join("cover.jpg"), b"not audio").unwrap();

        let (_, report) = read_library("laptop", &[dir], 100);

        assert_eq!(report.files, 3, "the jpg is not an audio file");
        assert_eq!(report.ingested, 3);
        assert_eq!(report.confirmed, 2);
        assert_eq!(report.provisional, 1);
        assert!(report.unreadable.is_empty());
        assert!(report.balances(), "{report}");
    }

    /// A file that is not audio at all is left out **loudly**. Bad metadata is
    /// not the same thing and is ingested anyway.
    #[test]
    fn something_that_only_looks_like_audio_is_reported_not_dropped() {
        let dir = library("junk", &[("Real", None)]);
        std::fs::write(dir.join("99 Broken.wav"), b"RIFF not really").unwrap();

        let (_, report) = read_library("laptop", &[dir], 100);

        assert_eq!(report.files, 2);
        assert_eq!(report.ingested, 1);
        assert_eq!(report.unreadable.len(), 1, "and named, not merely counted");
        assert!(report.balances(), "{report}");
    }

    /// The whole point: a disk becomes something a player can draw.
    #[test]
    fn a_disk_becomes_rows() {
        let dir = library(
            "rows",
            &[("Callisto", Some("183105b5-0000-4000-8000-000000000000"))],
        );
        let (instance, _) = read_library("laptop", &[dir], 100);
        let l = Librarian::from_catalogue(&instance.catalogue());

        let rec = *instance.catalogue().recordings.keys().next().unwrap();
        let row = l.row(rec).expect("a row");
        assert_eq!(row.title, "Callisto");
        assert_eq!(row.file_under, "Stephan Bodzin");
        assert!(row.available, "and this machine holds it");
    }

    /// Two machines reading the same disk agree without talking, which is the
    /// no-coordinator bet applied to a real directory.
    #[test]
    fn two_devices_reading_one_disk_agree() {
        let dir = library(
            "agree",
            &[
                ("Callisto", Some("183105b5-0000-4000-8000-000000000000")),
                ("Luna", Some("859d0860-0000-4000-8000-000000000000")),
            ],
        );
        let (m1, _) = read_library("laptop", std::slice::from_ref(&dir), 100);
        let (m2, _) = read_library("nas", &[dir], 140);

        let (a, b) = (m1.catalogue(), m2.catalogue());
        assert_eq!(
            a.recordings.keys().collect::<Vec<_>>(),
            b.recordings.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            a.encodings.keys().collect::<Vec<_>>(),
            b.encodings.keys().collect::<Vec<_>>()
        );
        assert!(a.conflicts.is_empty() && b.conflicts.is_empty());
    }

    /// **Measured against the real library**, because the 2026-08-27 lesson is
    /// that a benchmark outside the running player understates by 3–5×.
    ///
    /// Ignored by default — it wants 5774 files and fourteen seconds. Run with
    /// `cargo test --release measure_the_real_library -- --ignored --nocapture`.
    ///
    /// As of 2026-09-02:
    ///
    /// ```text
    /// 5774 files in 15.3s — 5774 ingested (5297 confirmed, 477 provisional), 0 unreadable
    /// 23097 records -> 5644 recordings, 5759 encodings, folded in under 10ms
    /// 5644 rows drawn in 0.15s
    /// ```
    ///
    /// The 15.3s is the hashing, and it is what the existing scan already
    /// spends — this replaces that pass rather than adding one. Everything
    /// after it is free by comparison, so the fold needs no cleverness.
    ///
    /// 91.7% confirmed matches the tag census (#70) to a tenth of a point.
    #[test]
    #[ignore = "needs the real library; run deliberately"]
    fn measure_the_real_library() {
        use crate::model::seam::Librarian;
        let root = PathBuf::from(std::env::var("HOME").unwrap()).join("MusicLibrary");
        if !root.exists() {
            eprintln!("no library at {}", root.display());
            return;
        }

        let (instance, report) = read_library("laptop", &[root], 1_788_000_000_000);
        eprintln!("scan     {report}");
        assert!(report.balances(), "every file must be accounted for");

        let cat = instance.catalogue();
        eprintln!(
            "fold     {} records -> {} recordings, {} encodings",
            instance.records().len(),
            cat.recordings.len(),
            cat.encodings.len()
        );

        let started = Instant::now();
        let l = Librarian::from_catalogue(&cat);
        let rows = l.recordings().filter_map(|r| l.row(r)).count();
        eprintln!(
            "rows     {rows} drawn in {:.2}s",
            started.elapsed().as_secs_f32()
        );

        // Not asserted at zero: a conflict here is a finding about the
        // library, not a defect in the model. The first run turned up an
        // album duplicated in place under two naming conventions, tagged
        // against different MusicBrainz recordings — which is exactly the
        // duplicate detector working.
        eprintln!("conflicts {}", cat.conflicts.len());
    }

    /// A deleted file goes away rather than being resurrected — the holdings
    /// set is published whole every pass (#71).
    #[test]
    fn a_second_pass_drops_what_is_no_longer_there() {
        let dir = library("deleted", &[("Callisto", None), ("Luna", None)]);
        let mut i = Instance::new(Id::derived(b"laptop"));
        populate(&mut i, std::slice::from_ref(&dir), 100);
        assert_eq!(i.catalogue().holdings[&i.device()].held.len(), 2);

        std::fs::remove_file(dir.join("02 Luna.wav")).unwrap();
        populate(&mut i, &[dir], 200);

        assert_eq!(
            i.catalogue().holdings[&i.device()].held.len(),
            1,
            "the second pass says what is there now, not what changed"
        );
        assert_eq!(
            i.catalogue().recordings.len(),
            2,
            "and the recording stays — it is decided about, just not owned"
        );
    }
}
