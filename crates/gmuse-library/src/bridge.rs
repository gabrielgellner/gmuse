//! From what the scanner read to what the librarian holds — issue #77.
//!
//! **This was the plank and it is now the bridge** (#250). The header said
//! *"transitional, and it dies with `Scan` (#86) … the scanner still fills a store,
//! and this turns a store into the catalogue"*. #86 closed, the `Store` type is gone,
//! and what happened to this module is the opposite of dying: `bridge::librarian` is
//! the call the librarian's own fold makes, and the table further down this file
//! attributes **76% of a 53 ms fold** to it. Load-bearing infrastructure, documented
//! for years as scaffolding.
//!
//! So what it does now, plainly: it takes the holdings a scan produced and the records
//! the log holds, and returns the `Librarian` everything reads through. `imported` is
//! the same job for one arrival rather than a whole pass.
//!
//! ## Why not simply scan twice
//!
//! `populate` reads every file and would answer this exactly, at fifteen
//! seconds a launch on top of the seventeen the scan already costs. But
//! `Track` already carries everything `Scanned` wants — the scanner read the
//! tags and hashed the audio on the way past — so the second pass would be
//! re-reading 6500 files to learn what the first pass already knew.
//!
//! ## Nothing here is ever written
//!
//! The instance this builds is **scratch**. It is folded once for the
//! projection and dropped, and it never sees `flush`.
//!
//! That is not a shortcut, it is the rule: a scan reconciles what is on this
//! disk, and `library-api.md` says catalogue facts arrive by `import`. If a
//! launch appended `RecordingCatalogued` for every file it saw, the log would
//! grow by the size of the library every time gmuse started, recording as
//! history what a rescan can produce from scratch.
//!
//! ## What is lost in the crossing, and it is one thing
//!
//! `Track` has no MusicBrainz **release group** id, so albums here derive
//! their identity from the album artist and title rather than converging on an
//! upstream id. Two devices still agree — the derivation is the same on both —
//! but a retitled album mints a new one. Adding the field to a type that is
//! being deleted would be churn; `populate` reads it properly, and this goes
//! away when it takes over.

use std::collections::{BTreeMap, BTreeSet};

use crate::facts::Record;
use crate::model::entity::{Device, Encoding};
use crate::model::id::Id;
use crate::model::instance::Instance;
#[cfg(test)]
use crate::model::instance::Scanned;
use crate::model::seam::Librarian;

// **`scanned()` and `format_of()` lived here and are gone** (#86).
//
// They converted a `store::Track` into the `Scanned` the ingest ladder takes,
// field by field, and guessed the container from the file extension because a
// `Track` did not record one. A scan produces `Scanned` directly now, so there
// is nothing to convert and nothing to guess.
//
// The conversion was lossy in a way its own comment admitted: a `Track`
// carried no artist id, so every artist crossing this plank was minted from
// its *name* — and `2Pac feat. Nate Dogg` is not `2Pac` to a hash, so a guest
// credit made a new artist. 92.7% of this library carries the id and it was
// thrown away here. It is not any more.

/// The library the player reads, assembled from this disk and this log.
///
/// Two sources, one fold. The scan says what is *here*; the log says what has
/// been decided about it — ratings, plays, when it was acquired. Folding them
/// together is what makes a row carry both, and doing it through the same
/// `Instance` the model uses everywhere else means the ladder that chooses a
/// recording's identity lives in exactly one place.
/// **A librarian over a catalogue somebody already folded** — #256, #261.
///
/// [`librarian`] builds a scratch `Instance`, merges every record into it, and folds *that* —
/// so a fold copied the whole log (83.5 MB on the real library) to compute a catalogue its
/// caller was computing anyway. The copy is transient, so it never looked like a leak; it
/// showed up only in the **peak**, which is what a resident set follows (#255).
///
/// The scratch exists for one thing: a log that has never asserted holdings needs the scan's
/// map standing in, and that has to go through a fact and a refold. `Job::Scan` writes the
/// assertion now (#237), so after a library's first scan the scratch is a copy for nothing.
///
/// **The bootstrap still goes the long way**, deliberately: poking the holdings straight into
/// the catalogue would put "how this fact lands" in a second place, which is the hazard #261
/// is about.
pub fn librarian_from(
    catalogue: &crate::projection::catalogue::Catalogue,
    held: &crate::operations::scan::Holdings,
    records: &BTreeSet<Record>,
    device: Id<Device>,
    now: i64,
) -> Librarian {
    let mut l = Librarian::from_catalogue(catalogue);
    if !catalogue.holdings.contains_key(&device) {
        // **No assertion in the log yet, so the scan's map stands in — on the
        // librarian, not the catalogue** (#269). This used to go through a scratch
        // `Instance` of every record with a synthetic `HoldingsAsserted` folded in,
        // which was the honest route while a fold was the only way a holding could
        // land — and a copy of the log for a launch that happens once per library.
        // The librarian is L2: derived, rebuilt per publish, never stored. Standing
        // a row in here puts nothing in a second place, because the projection is
        // untouched and the first `Job::Scan` writes the real assertion (#237).
        for h in held.iter() {
            l.encodings.holdings.push(crate::model::encoding::Holding {
                encoding: crate::model::id::Ref::to(Id::<Encoding>::of_audio(&h.audio)),
                device: crate::model::id::Ref::to(device),
                locator: h.at.to_string_lossy().into_owned(),
            });
        }
        let _ = now;
    }
    l.record_count = records.len();
    l.here(device);
    l
}

pub fn librarian(
    held: &crate::operations::scan::Holdings,
    records: &BTreeSet<Record>,
    device: Id<Device>,
    now: i64,
) -> Librarian {
    // **The log is the catalogue, and this no longer re-derives it** (#140).
    //
    // Until `import` existed, a launch ingested the whole scan into this
    // scratch instance — 160,000 facts minted from 6,500 files, folded once
    // and dropped. It filled a real gap and it made §2's "a file's tags
    // change → nothing" false of every launch: a retag outside gmuse became
    // the library's answer at the next scan.
    //
    // Now `import` puts description in the log, and a scan contributes
    // holdings and nothing else. What a file *claims about itself* is read
    // once, when somebody declares it belongs here.
    let mut scratch = Instance::new(device);
    scratch.merge(now, records);

    // **What this device can actually open** — #85, and #237 for who gets to say.
    //
    // Without it the librarian knows every recording and can reach none of them:
    // `open` looks for a holding on this device and finds no rows, so it refuses
    // everything.
    //
    // **The log wins where it has spoken.** This used to assert the scan's answer
    // unconditionally, and holdings supersede whole — so a fold with an empty
    // `held` erased what the log said. That is what an attached view is (#219): it
    // never scans, so it arrived here with nothing and left unable to open a
    // single row it could see. And a view *with* a cache was no better, because
    // the cache is from the last local launch while the log is whatever the engine
    // reconciled a moment ago; asserting the cache with `now` would put the stale
    // answer on top.
    //
    // So the scan's map is a **bootstrap**, for a library whose log has no
    // assertion yet. Once `Job::Scan` has written one — which it now does — the
    // log is the single answer, which is what `library-is-the-truth` says about
    // every other field.
    if scratch.holdings_asserted(device).is_none() {
        let holdings: BTreeMap<Id<Encoding>, String> = held
            .iter()
            .map(|h| {
                (
                    Id::<Encoding>::of_audio(&h.audio),
                    h.at.to_string_lossy().into_owned(),
                )
            })
            .collect();
        scratch.assert_holdings(now, holdings);
    }

    let mut l = Librarian::from_catalogue(&scratch.catalogue());
    // **A count, where this used to clone the whole log** (#128, #256). `scratch` holds
    // both the log's facts and whatever an ingest minted; only the first were ever
    // wanted here, and carrying them cost **75.5 MB of the 102 MB** a fold of the real
    // library kept. The info panel's history — the one reader — is a question for the
    // log's owner now (`Desk::history`), asked when a focus changes and cached per
    // focus, rather than a copy every fold pays for.
    l.record_count = records.len();
    // Which device this is, so a row knows where it can be opened. The
    // librarian holds every holding; which of them is reachable *now* is a
    // fact about the machine, and this is the only place that knows it.
    l.here(device);
    l
}

/// A librarian over files that have been **imported**, for a test that means
/// "these are in the library" — #140.
///
/// A launch no longer catalogues what a scan walks past, so a test that wants
/// rows has to declare them the way a person does. This is `import` followed
/// by the fold, in one call.
#[cfg(any(test, feature = "testing"))]
pub fn imported(
    held: &crate::operations::scan::Holdings,
    records: &BTreeSet<Record>,
    device: Id<Device>,
    now: i64,
) -> Librarian {
    let mut log = Instance::new(device);
    log.merge(now, records);
    log.ingest(now, &held.tags());
    librarian(held, log.records(), device, now)
}

#[cfg(test)]
mod forget_and_relink {

    //! **What happens when a file stops being the file** — issue #88.
    //!
    //! `library-api.md` names `forget` and `relink` as operations that must be
    //! explicit and declines to say what they do. Worked here as the cases the
    //! issue asks for rather than decided in the abstract, and the cases give
    //! a shorter answer than the question expects:
    //!
    //! **`relink` already exists**, as `Proposal::Identified` folding to
    //! `EncodingBound { encoding, from, to }`. "This file is that recording"
    //! is exactly what an identification asserts, and 377 of them have been
    //! applied to this library. There is nothing left to design.
    //!
    //! **`forget` is not about the holding.** Holdings are published whole and
    //! supersede, so a file that goes away stops being held at the next scan
    //! with nothing said — and it cannot be otherwise, because a command that
    //! removed one holding would be overwritten by the next scan anyway.
    //!
    //! Which leaves what `forget` could still mean: *this is not part of my
    //! library*, said about a recording rather than about a file. That is a
    //! **collection** decision, and it is #28's exclusion and #87's
    //! quarantine, not a third mechanism.

    /// **A retag outside gmuse changes nothing** — #140, and the property the
    /// whole inversion is for.
    ///
    /// A launch used to ingest the entire scan into a scratch instance, so a
    /// tool that rewrote a title made that title the library's answer at the
    /// next scan. `library-api.md` §2 says a file whose tags change causes
    /// gmuse to do *nothing*, and until now that was true of no launch.
    #[test]
    fn a_launch_reads_the_log_and_not_the_tags() {
        let imported_as = |title: &str| crate::model::instance::Scanned {
            audio: crate::audio_id::AudioId {
                scheme: 1,
                hash: 42,
            },
            title: title.into(),
            artist: "Stephan Bodzin".into(),
            album: Some("Luna".into()),
            length_ms: 200_000,
            ..Default::default()
        };

        // Imported once, under the title it had then.
        let mut log = Instance::new(device());
        log.ingest(100, &[imported_as("Callisto")]);

        // The scan now sees the same audio with different tags — somebody
        // retagged it, or a tool did.
        let mut held = crate::operations::scan::Holdings::new();
        held.take(
            std::path::PathBuf::from("/m/Luna/01 Callisto.flac"),
            0,
            0,
            imported_as("Something Else Entirely"),
        );

        let l = librarian(&held, log.records(), device(), 200);
        let titles: Vec<String> = crate::model::rows::Listing::rows(&l)
            .into_iter()
            .filter_map(|id| crate::model::rows::Listing::listed(&l, id))
            .map(|r| r.title.to_string())
            .collect();
        assert_eq!(
            titles,
            vec!["Callisto".to_string()],
            "a launch read a tag it was told never to read again"
        );
    }

    /// **A file nobody imported is not a row**, however plainly it is on the
    /// disk — #140. The offer a scan makes is `unknown`, and taking it up is
    /// `import`.
    #[test]
    fn a_file_on_disk_that_nothing_imported_is_not_in_the_library() {
        let mut held = crate::operations::scan::Holdings::new();
        held.take(
            std::path::PathBuf::from("/m/Stranger.flac"),
            0,
            0,
            crate::model::instance::Scanned {
                audio: crate::audio_id::AudioId { scheme: 1, hash: 9 },
                title: "Stranger".into(),
                artist: "Nobody".into(),
                length_ms: 1000,
                ..Default::default()
            },
        );

        let bare = librarian(&held, &BTreeSet::new(), device(), 100);
        assert_eq!(
            crate::model::rows::Listing::rows(&bare).len(),
            0,
            "a scan catalogued a file nobody declared"
        );

        // And declaring it is what puts it there.
        let declared = imported(&held, &BTreeSet::new(), device(), 100);
        assert_eq!(crate::model::rows::Listing::rows(&declared).len(), 1);
    }
    use super::*;
    use crate::audio_id::AudioId;
    use crate::facts::Fact;
    use crate::model::basis::Basis;
    use crate::model::clock::HybridClock;
    use crate::model::encoding::Format;
    use crate::model::spine::Recording;
    use crate::operations::scan::Holdings;

    /// **A file, as the tags a fixture means** — #141. A holding carries the
    /// audio id alone now, so what a test says about a file is a `Scanned` and
    /// the store is built by declaring them.
    fn file(path: &str, hash: u128) -> (std::path::PathBuf, Scanned) {
        (
            std::path::PathBuf::from(path),
            Scanned {
                audio: AudioId { scheme: 1, hash },
                title: "Callisto".into(),
                artist: "Bodzin".into(),
                album: Some("Luna".into()),
                album_artist: Some("Bodzin".into()),
                ..Default::default()
            },
        )
    }

    fn store_of(files: &[(std::path::PathBuf, Scanned)]) -> Holdings {
        let mut s = Holdings::new();
        for (at, seen) in files {
            s.take(at.clone(), 100, 4_000_000, seen.clone());
        }
        s
    }

    fn device() -> Id<Device> {
        Id::derived(b"laptop")
    }

    /// **Case 4 — a file that simply vanished, and `forget` has nothing to
    /// do.**
    ///
    /// Holdings supersede rather than accumulate, so the next scan publishes a
    /// set without it and the holding is gone. What survives is everything the
    /// split exists to protect: the recording, its rating and its plays, which
    /// hang off the performance and never off the file.
    #[test]
    fn a_file_that_goes_away_needs_no_command() {
        let gone = file("/m/callisto.mp3", 7);
        let before = imported(
            &store_of(std::slice::from_ref(&gone)),
            &BTreeSet::new(),
            device(),
            100,
        );
        let recording = before.recordings().next().expect("a recording");
        assert!(before.open(recording, &[device()], &[]).is_some());

        // Somebody deleted it. The next scan simply does not see it.
        let mut clock = HybridClock::new(device());
        let history: BTreeSet<Record> = [
            Fact::RecordingRated {
                recording,
                stars: Some(5),
            },
            Fact::RecordingPlayed {
                recording,
                encoding: None,
            },
        ]
        .into_iter()
        .map(|fact| Record {
            stamp: clock.stamp(100),
            fact,
        })
        .collect();
        let after = imported(&store_of(&[]), &history, device(), 200);

        assert!(
            after.open(recording, &[device()], &[]).is_none(),
            "a file nobody holds is still openable"
        );
        assert_eq!(
            after.ratings.get(&recording),
            Some(&5),
            "the rating went with the file"
        );
        assert_eq!(
            after.plays.get(&recording),
            Some(&1),
            "the play went with the file"
        );
    }

    /// **Case 1 — a re-buy, when something names the performance.**
    ///
    /// A better copy arrives and the old one is still on disk. Both files
    /// carry the same recording id, so both are encodings of one recording,
    /// both are held, and the canonical rule picks which represents it.
    /// Nothing is forgotten and nothing is pinned against — the question the
    /// issue asks turns out to have no operation in it.
    #[test]
    fn a_re_buy_of_a_named_recording_is_a_second_encoding() {
        let mbid = uuid::Uuid::from_u128(0xca11);
        let mut mp3 = file("/m/callisto.mp3", 7);
        mp3.1.identity = Some(Basis::MusicBrainz(mbid));
        let mut flac = file("/m/callisto.flac", 8);
        flac.1.identity = Some(Basis::MusicBrainz(mbid));

        let l = imported(&store_of(&[mp3, flac]), &BTreeSet::new(), device(), 100);
        assert_eq!(l.recordings().count(), 1, "a re-buy split the recording");
        let recording = l.recordings().next().unwrap();
        assert_eq!(l.encodings.of(recording).len(), 2, "one copy was dropped");
    }

    /// **And when nothing names it, a re-buy splits — which is the answer
    /// rather than a bug.**
    ///
    /// An unnamed recording rests on its bytes, so a better rip of it is a
    /// different recording: the only evidence available says they are two
    /// things. That is the error this model prefers, because a split is one
    /// merge from repaired and a false merge is permanent.
    ///
    /// The repair is `RecordingMerged` and it already exists. What `forget`
    /// would have to be, to help here, is a destructive answer to a question a
    /// merge answers safely.
    #[test]
    fn a_re_buy_of_an_unnamed_recording_splits_and_a_merge_repairs_it() {
        let store = store_of(&[file("/m/callisto.mp3", 7), file("/m/callisto.flac", 8)]);
        let split = imported(&store, &BTreeSet::new(), device(), 100);
        assert_eq!(split.recordings().count(), 2, "the bytes are all there is");

        let mut ids = split.recordings();
        let (a, b) = (ids.next().unwrap(), ids.next().unwrap());
        let mut clock = HybridClock::new(device());
        let repair: BTreeSet<Record> = std::iter::once(Record {
            stamp: clock.stamp(200),
            fact: Fact::RecordingMerged {
                loser: a,
                winner: b,
            },
        })
        .collect();

        let l = imported(&store, &repair, device(), 300);
        assert_eq!(l.recordings().count(), 1, "the merge did not join them");
        assert_eq!(
            l.encodings.of(b).len(),
            2,
            "both copies belong to the surviving recording"
        );
    }

    /// **Case 2 — a re-rip in place, where the path is the same and the audio
    /// is not.**
    ///
    /// The old encoding stops being held, because those bytes are gone. Where
    /// something names the performance, the song is unchanged and the new file
    /// is simply its copy — so a path changing hand to hand is invisible to
    /// the library, which is the whole point of not keying by one.
    #[test]
    fn a_re_rip_in_place_replaces_the_copy_and_keeps_the_song() {
        let mbid = uuid::Uuid::from_u128(0xca11);
        let mut first = file("/m/callisto.mp3", 7);
        first.1.identity = Some(Basis::MusicBrainz(mbid));
        let before = imported(&store_of(&[first]), &BTreeSet::new(), device(), 100);
        let recording = before.recordings().next().expect("a recording");
        let old = before.encodings.of(recording)[0].id;

        let mut better = file("/m/callisto.mp3", 9);
        better.1.identity = Some(Basis::MusicBrainz(mbid));
        let after = imported(&store_of(&[better]), &BTreeSet::new(), device(), 200);

        assert_eq!(
            after.recordings().next(),
            Some(recording),
            "re-ripping a file changed which song it is"
        );
        assert!(
            after.encodings.holders(old).is_empty(),
            "the bytes that are gone are still held"
        );
    }

    /// **Case 5 — two files that turn out to be one recording is `merge`, not
    /// a third thing.**
    ///
    /// Confirmed rather than assumed, because the issue asks. `RecordingMerged`
    /// carries the history across, which is precisely what neither `forget`
    /// nor `relink` is allowed to touch.
    #[test]
    fn two_files_turning_out_to_be_one_song_is_a_merge() {
        let a = Id::<Recording>::derived(b"a");
        let b = Id::<Recording>::derived(b"b");
        let mut clock = HybridClock::new(device());
        let records: BTreeSet<Record> = [
            Fact::RecordingRated {
                recording: a,
                stars: Some(4),
            },
            Fact::RecordingMerged {
                loser: a,
                winner: b,
            },
        ]
        .into_iter()
        .map(|fact| Record {
            stamp: clock.stamp(100),
            fact,
        })
        .collect();

        let l = imported(&store_of(&[]), &records, device(), 200);
        assert_eq!(
            l.ratings.get(&b),
            Some(&4),
            "the rating stayed on the row nothing points at"
        );
        let _ = Format::Mp3;
    }
}

#[cfg(test)]
mod opening {
    //! **What this device can open** — issue #85.

    use super::*;
    use crate::audio_id::AudioId;
    use crate::model::basis::Basis;
    use crate::model::encoding::Format;
    use crate::model::spine::Recording;
    use crate::operations::scan::Holdings;

    fn store_with(path: &str, hash: u128) -> Holdings {
        let mut s = Holdings::new();
        s.take(
            std::path::PathBuf::from(path),
            100,
            4_000_000,
            Scanned {
                audio: AudioId { scheme: 1, hash },
                title: "Callisto".into(),
                artist: "Bodzin".into(),
                ..Default::default()
            },
        );
        s
    }

    /// **The librarian could reach nothing at all**, because the bridge never
    /// said what was here. `open` looks for a holding on this device and found
    /// no rows, so it refused every file — which is the whole reason nothing
    /// called it.
    #[test]
    fn the_library_can_open_what_this_device_holds() {
        let device = Id::<Device>::derived(b"laptop");
        let store = store_with("/m/callisto.mp3", 7);
        let l = imported(&store, &BTreeSet::new(), device, 100);

        let recording = l.recordings().next().expect("a recording");
        let (encoding, locator) = l
            .open(recording, &[device], &[])
            .expect("the library refused a file this device is holding");
        assert_eq!(locator, "/m/callisto.mp3");
        assert_eq!(encoding.audio, AudioId { scheme: 1, hash: 7 });
    }

    /// **An imprint is a thing the library holds, not a string on a file.**
    ///
    /// A label sampler's album artist is "Various Artists", so the imprint has
    /// nowhere else to go — and Bandcamp writes it into the album artist for
    /// want of anywhere better, which is what makes a label appear in a
    /// library as though it were an artist. 5,786 of 6,500 files here name
    /// one, across 218 distinct imprints.
    ///
    /// **On the pressing rather than the album**, because that is what a
    /// reissue changes: the record is the same and the imprint is not.
    #[test]
    fn a_pressing_names_the_imprint_that_issued_it() {
        let device = Id::<Device>::derived(b"laptop");
        let mut store = Holdings::new();
        store.take(
            std::path::PathBuf::from("/m/callisto.mp3"),
            100,
            4_000_000,
            Scanned {
                audio: AudioId { scheme: 1, hash: 7 },
                title: "Callisto".into(),
                artist: "Bodzin".into(),
                album: Some("Luna".into()),
                album_artist: Some("Bodzin".into()),
                label: Some("Herzblut".into()),
                ..Default::default()
            },
        );
        let l = imported(&store, &BTreeSet::new(), device, 100);

        let release = l.spine.releases.values().next().expect("a pressing");
        let label = release.label.expect("the pressing named no imprint").id();
        assert_eq!(
            l.reference.labels.get(&label).map(|r| r.name.as_str()),
            Some("Herzblut"),
            "the imprint was referenced but never named, so nothing can render it"
        );
        assert_eq!(
            label,
            Id::<crate::model::entity::Label>::of_name("  herzblut  "),
            "two sleeves spelling it differently must be one imprint"
        );
    }

    /// **How loud a copy should play is a fact about that copy** — #109.
    ///
    /// Two rips of one recording are mastered differently and carry different
    /// ReplayGain, so it belongs to the copy rather than to the performance.
    /// It was read off whichever file happened to be open, which was right by
    /// accident and stopped being right the moment the library chose a copy.
    #[test]
    fn a_copy_carries_the_loudness_its_tags_claim() {
        let device = Id::<Device>::derived(b"laptop");
        let mut store = Holdings::new();
        store.take(
            std::path::PathBuf::from("/m/callisto.mp3"),
            100,
            4_000_000,
            Scanned {
                audio: AudioId { scheme: 1, hash: 7 },
                title: "Callisto".into(),
                artist: "Bodzin".into(),
                levels: crate::model::encoding::Levels {
                    track_cdb: Some(-728),
                    track_peak_e4: Some(9885),
                    album_cdb: Some(-810),
                    album_peak_e4: Some(10_000),
                },
                ..Default::default()
            },
        );
        let l = imported(&store, &BTreeSet::new(), device, 100);

        let recording = l.recordings().next().expect("a recording");
        let copy = l.encodings.of(recording)[0];
        assert_eq!(copy.levels.track_cdb, Some(-728));
        assert_eq!(copy.levels.album_cdb, Some(-810));
        // And back out as the float a fader wants.
        assert_eq!(copy.levels.track_db(), Some(-7.28));
        assert_eq!(copy.levels.album_peak(), Some(1.0));
    }

    /// **A copy knows its own shape** — issue #86.
    ///
    /// Sample rate and channel count are the last two fields a display row
    /// asks for that the model could not answer, and they are not rare: 6,493
    /// of 6,500 files here carry a rate and every one carries a channel
    /// count. A librarian that could not say them could not replace the
    /// scanner as the thing a row is drawn from, so deleting the scanner
    /// would have quietly emptied two template fields.
    #[test]
    fn a_copy_carries_its_sample_rate_and_channels() {
        let device = Id::<Device>::derived(b"laptop");
        let mut store = Holdings::new();
        store.take(
            std::path::PathBuf::from("/m/callisto.mp3"),
            100,
            4_000_000,
            Scanned {
                audio: AudioId { scheme: 1, hash: 7 },
                title: "Callisto".into(),
                artist: "Bodzin".into(),
                sample_rate: Some(48_000),
                channels: Some(2),
                ..Default::default()
            },
        );
        let l = imported(&store, &BTreeSet::new(), device, 100);

        let recording = l.recordings().next().expect("a recording");
        let copy = l.encodings.of(recording)[0];
        assert_eq!(copy.sample_rate, Some(48_000));
        assert_eq!(copy.channels, Some(2));
    }

    /// **The copy that plays is not always the best copy**, which is why a
    /// play has to name the one it opened.
    ///
    /// A better encoding on the NAS wins `canonical` — it is the one to prefer
    /// when there is a choice — and this machine cannot reach it, so `open`
    /// hands back the local one. Recording the canonical copy against the play
    /// would then say a listen came from bytes this device has never read.
    #[test]
    fn the_copy_that_opens_is_not_always_the_canonical_one() {
        use crate::facts::Fact;
        use crate::model::clock::HybridClock;

        let mine = Id::<Device>::derived(b"laptop");
        let nas = Id::<Device>::derived(b"asnas");
        let store = store_with("/m/callisto.mp3", 7);

        // A better copy, held only by the NAS.
        let better = AudioId { scheme: 1, hash: 8 };
        let elsewhere = Id::<Encoding>::of_audio(&better);
        let recording = Id::<Recording>::of_basis(&Basis::Audio(AudioId { scheme: 1, hash: 7 }));
        let mut clock = HybridClock::new(nas);
        let records: BTreeSet<Record> = [
            Fact::EncodingDescribed {
                encoding: elsewhere,
                audio: better,
                format: Some(Format::Flac),
                bitrate: 1000,
                length_ms: 200_000,
                sample_rate: Some(44_100),
                channels: Some(2),
            },
            Fact::EncodingBound {
                encoding: elsewhere,
                from: None,
                to: recording,
            },
            Fact::HoldingsAsserted {
                device: nas,
                held: [(elsewhere, "/nas/callisto.flac".to_string())]
                    .into_iter()
                    .collect(),
            },
        ]
        .into_iter()
        .map(|fact| Record {
            stamp: clock.stamp(100),
            fact,
        })
        .collect();

        let l = imported(&store, &records, mine, 200);
        assert_eq!(
            l.encodings.canonical(recording).map(|e| e.id),
            Some(elsewhere),
            "the NAS copy is the better one, or this test proves nothing"
        );
        let (opened, locator) = l.open(recording, &[mine], &[]).expect("a local copy");
        assert_eq!(locator, "/m/callisto.mp3");
        assert_ne!(
            opened.id, elsewhere,
            "the player was handed a copy this device cannot read"
        );
    }

    /// And refuses one it does not hold, rather than inventing a path. That is
    /// the property that makes a locator safe to produce at the moment of
    /// opening: it is either true of this machine now, or absent.
    #[test]
    fn the_library_refuses_what_another_device_holds() {
        let mine = Id::<Device>::derived(b"laptop");
        let theirs = Id::<Device>::derived(b"asnas");
        let store = store_with("/m/callisto.mp3", 7);
        let l = imported(&store, &BTreeSet::new(), mine, 100);

        let recording = l.recordings().next().expect("a recording");
        assert!(l.open(recording, &[theirs], &[]).is_none());
        let _: Option<Id<Recording>> = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_id::AudioId;
    use crate::model::basis::Basis;
    use crate::model::encoding::Format;
    use crate::model::entity::Recording;
    use crate::operations::scan::Holdings;
    use std::path::PathBuf;

    fn track(path: &str, title: &str, artist: &str, hash: u128) -> (PathBuf, Scanned) {
        (
            PathBuf::from(path),
            Scanned {
                audio: AudioId { scheme: 1, hash },
                artist: artist.into(),
                album_artist: Some(artist.into()),
                album: Some("An Album".into()),
                title: title.into(),
                track: Some(1),
                length_ms: 200_000,
                bitrate: 320,
                format: Some(Format::Mp3),
                ..Default::default()
            },
        )
    }

    fn device() -> Id<Device> {
        Id::derived(b"test")
    }

    /// A file on disk becomes a row the player can draw.
    #[test]
    fn a_scanned_file_becomes_a_row() {
        let mut store = Holdings::new();
        {
            let (at, seen) = track("/m/a.mp3", "Callisto", "Stephan Bodzin", 1);
            store.take(at, 100, 4_000_000, seen);
        }
        let l = imported(&store, &BTreeSet::new(), device(), 1_000);

        let recording = l.recordings().next().expect("one recording");
        let row = l.row(recording).expect("a row");
        assert_eq!(row.title, "Callisto");
        assert_eq!(row.file_under, "Stephan Bodzin");
        assert_eq!(row.album.as_deref(), Some("An Album"));
        assert_eq!(row.format, Some(Format::Mp3), "read from the extension");
        assert_eq!(row.bitrate, Some(320));
        assert!(row.available, "a file on this disk is available");
    }

    /// **The whole point of folding both sources.** The scan knows the track
    /// exists; only the log knows it is worth four stars, and a row has to
    /// carry both or the player still needs two places to look.
    #[test]
    fn the_log_supplies_what_the_disk_cannot() {
        let mut store = Holdings::new();
        {
            let (at, seen) = track("/m/a.mp3", "Callisto", "Stephan Bodzin", 1);
            store.take(at, 100, 4_000_000, seen);
        }

        // What the recording will be, derived the same way the ladder does.
        let scratch_l = imported(&store, &BTreeSet::new(), device(), 1_000);
        let recording: Id<Recording> = scratch_l.recordings().next().unwrap();
        assert_eq!(scratch_l.row(recording).unwrap().rating, None);

        let mut writer = Instance::new(device());
        writer.rate(2_000, recording, Some(4));
        writer.play(2_100, recording, Id::derived(b"whatever"));

        let l = imported(&store, writer.records(), device(), 3_000);
        let row = l.row(recording).expect("still a row");
        assert_eq!(row.rating, Some(4), "the rating did not survive the fold");
        assert_eq!(row.plays, 1);
    }

    /// **A recording has no length; its encodings do.**
    ///
    /// An edit, a radio cut and a truncated rip are one recording and three
    /// durations. A row reports the copy it would open, because "did I hear
    /// enough of it" is a question about the file that played — and ranking
    /// the copies to pick one was a rule invented to fill a field that should
    /// not have been filled. Under it, a twelve-second rip measured against a
    /// four-minute sibling could never have earned a play at all.
    #[test]
    fn a_row_reports_the_length_of_the_copy_it_would_open() {
        let mut store = Holdings::new();
        let mbid = uuid::Uuid::from_u128(0x1831_05b5);

        // One recording, one copy here, and it is the short one.
        let mut partial = track("/m/partial.mp3", "Callisto", "Stephan Bodzin", 2);
        partial.1.identity = Some(Basis::MusicBrainz(mbid));
        partial.1.length_ms = 12_000;
        let (at, seen) = partial;
        store.take(at, 100, 4_000_000, seen);

        let l = imported(&store, &BTreeSet::new(), device(), 1_000);
        let recording = l.recordings().next().expect("one recording");
        assert_eq!(
            l.row(recording).unwrap().length,
            std::time::Duration::from_secs(12),
            "the row reported something other than the copy it holds"
        );
    }

    /// **A file with no readable audio cannot be a holding at all** (#86).
    ///
    /// This used to build one with `audio_id: None` and assert the bridge left
    /// it out. A `Held` has an audio id by construction — `ingest::read` fails
    /// for a file whose audio will not read, so the scan reports it as an
    /// error and it never reaches here. The case is a type error now rather
    /// than a filter, which is why this asserts the *scan*'s half instead.
    #[test]
    fn a_file_with_no_readable_audio_is_reported_rather_than_held() {
        let dir = std::env::temp_dir().join("gmuse-bridge-unreadable");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("99 Broken.wav"), b"RIFF not really").unwrap();

        let mut store = Holdings::new();
        let report = crate::operations::scan::scan(&mut store, std::slice::from_ref(&dir));

        assert_eq!(store.len(), 0, "it is not a holding");
        assert_eq!(report.errors.len(), 1, "and it is named, not dropped");

        let l = imported(&store, &BTreeSet::new(), device(), 1_000);
        assert_eq!(l.recordings().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two files, identical audio, one recording — and the row still draws.
    /// The library has a real album duplicated in place under two naming
    /// conventions, and it must not appear twice.
    #[test]
    fn one_recording_however_many_copies() {
        let mut store = Holdings::new();
        {
            let (at, seen) = track("/m/01 Callisto.mp3", "Callisto", "Stephan Bodzin", 1);
            store.take(at, 100, 4_000_000, seen);
        }
        {
            let (at, seen) = track("/m/1 - Callisto.mp3", "Callisto", "Stephan Bodzin", 1);
            store.take(at, 100, 4_000_000, seen);
        }

        let l = imported(&store, &BTreeSet::new(), device(), 1_000);
        assert_eq!(
            l.recordings().count(),
            1,
            "the duplicate became a second row"
        );
    }
}

// **`the_two_sides_agree` lived here and is gone** (#86).
//
// It compared `store.listed(id)` against `library.listed(id)` field by field
// over 6,485 real rows, because `rows::Listing` is the seam and the migration
// was a matter of swapping which side answered — which is only safe if the two
// answers are the same.
//
// It earned its place: it found a `u16::MAX` sentinel rendering as track
// 65535, a credit whose artist had no name still emitting its join phrase, one
// artist credited twice, `year` reading a field 98% of the library does not
// carry, and a librarian with no genres at all. All five were fixed because it
// ran.
//
// There is one implementation now, so there is nothing left to compare. What
// it was protecting is protected by the tests those findings became.
