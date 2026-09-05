//! From what the scanner read to what the librarian holds — issue #77.
//!
//! **Transitional, and it dies with `Store` (#86).** The player needs a
//! `Librarian` to read through before `Store` can go, and `Store` cannot go
//! until the player reads through a `Librarian`. This is the plank across that
//! gap: the scanner still fills a store, and this turns a store into the
//! catalogue the librarian projects from.
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

use crate::model::basis::Basis;
use crate::model::encoding::Format;
use crate::model::entity::{Device, Encoding};
use crate::model::id::Id;
use crate::model::instance::{Instance, Scanned};
use crate::model::log::Record;
use crate::model::seam::Librarian;
use crate::store::Store;

/// What the scanner found, in the shape the ingest ladder takes.
///
/// A file with no `audio_id` is left out: it has no identity, so there is
/// nothing to catalogue it as. That is the same call `populate` makes, and it
/// is a file gmuse could not read as audio at all rather than one with bad
/// tags.
fn scanned(store: &Store) -> Vec<Scanned> {
    store
        .iter()
        .filter_map(|(_, t)| {
            Some(Scanned {
                audio: t.audio_id?,
                title: t.display_title().to_string(),
                artist: t.artist.clone().unwrap_or_default(),
                identity: t.mb_recording.map(Basis::MusicBrainz),
                // `Track` carries no artist id, so an artist crossing this
                // plank is minted from its name and a guest credit still
                // splits it. `populate` reads the tag properly; this goes away
                // with `Store` (#86).
                artist_mbid: None,
                credit_is_joined: false,
                album_artist_mbid: None,
                album: t.album.clone(),
                album_artist: t.albumartist.clone(),
                disc: t.discnumber.map(|d| d as u16),
                track: t.tracknumber.map(|n| n as u16),
                mb_release_group: None,
                mb_release: None,
                length_ms: t.duration.as_millis() as u32,
                format: format_of(t),
                bitrate: t.bitrate.unwrap_or(0),
                sample_rate: t.samplerate,
                channels: t.channels,
            })
        })
        .collect()
}

/// The container, from the file's extension.
///
/// The scanner does not record it, and the extension is what the decoder went
/// by anyway. An unknown extension is `None` rather than a guess — a verdict
/// on a format nobody identified would be a verdict on nothing.
fn format_of(track: &crate::store::Track) -> Option<Format> {
    let ext = track.path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "flac" => Format::Flac,
        "m4a" | "alac" => Format::Alac,
        "wav" | "aiff" | "aif" => Format::Wav,
        "mp3" => Format::Mp3,
        "aac" => Format::Aac,
        "ogg" | "oga" => Format::Vorbis,
        "opus" => Format::Opus,
        _ => return None,
    })
}

/// The library the player reads, assembled from this disk and this log.
///
/// Two sources, one fold. The scan says what is *here*; the log says what has
/// been decided about it — ratings, plays, when it was acquired. Folding them
/// together is what makes a row carry both, and doing it through the same
/// `Instance` the model uses everywhere else means the ladder that chooses a
/// recording's identity lives in exactly one place.
pub fn librarian(
    store: &Store,
    records: &BTreeSet<Record>,
    device: Id<Device>,
    now: i64,
) -> Librarian {
    // The log first: `ingest` will not mint over a binding it can see, and a
    // scan into an empty instance can see none. See `query::rows`.
    let mut scratch = Instance::new(device);
    scratch.merge(now, records);
    scratch.ingest(now, &scanned(store));

    // **What this device can actually open** — issue #85.
    //
    // Without it the librarian knows every recording and can reach none of
    // them: `open` looks for a holding on this device and finds no rows, so it
    // refuses everything. A scan is exactly the moment that answer is known,
    // and it is the only moment — by the time the player asks, all it has is
    // an id.
    let held: BTreeMap<Id<Encoding>, String> = store
        .iter()
        .filter_map(|(_, t)| {
            let audio = t.audio_id?;
            Some((
                Id::<Encoding>::of_audio(&audio),
                t.path.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    scratch.assert_holdings(now, held);

    Librarian::from_catalogue(&scratch.catalogue())
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

    use super::*;
    use crate::audio_id::AudioId;
    use crate::model::clock::HybridClock;
    use crate::model::encoding::Format;
    use crate::model::log::Fact;
    use crate::model::spine::Recording;
    use crate::store::Track;

    fn file(path: &str, hash: u128) -> Track {
        Track {
            path: std::path::PathBuf::from(path),
            title: Some("Callisto".into()),
            artist: Some("Bodzin".into()),
            album: Some("Luna".into()),
            albumartist: Some("Bodzin".into()),
            audio_id: Some(AudioId { scheme: 1, hash }),
            ..Default::default()
        }
    }

    fn store_of(tracks: &[Track]) -> Store {
        let mut s = Store::default();
        for t in tracks {
            s.upsert(t.clone());
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
        let before = librarian(
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
        let after = librarian(&store_of(&[]), &history, device(), 200);

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
        mp3.mb_recording = Some(mbid);
        let mut flac = file("/m/callisto.flac", 8);
        flac.mb_recording = Some(mbid);

        let l = librarian(&store_of(&[mp3, flac]), &BTreeSet::new(), device(), 100);
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
        let split = librarian(&store, &BTreeSet::new(), device(), 100);
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

        let l = librarian(&store, &repair, device(), 300);
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
        first.mb_recording = Some(mbid);
        let before = librarian(&store_of(&[first]), &BTreeSet::new(), device(), 100);
        let recording = before.recordings().next().expect("a recording");
        let old = before.encodings.of(recording)[0].id;

        let mut better = file("/m/callisto.mp3", 9);
        better.mb_recording = Some(mbid);
        let after = librarian(&store_of(&[better]), &BTreeSet::new(), device(), 200);

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

        let l = librarian(&store_of(&[]), &records, device(), 200);
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
    use crate::model::spine::Recording;
    use crate::store::Track;

    fn store_with(path: &str, hash: u128) -> Store {
        let mut s = Store::default();
        s.upsert(Track {
            path: std::path::PathBuf::from(path),
            title: Some("Callisto".into()),
            artist: Some("Bodzin".into()),
            audio_id: Some(AudioId { scheme: 1, hash }),
            ..Default::default()
        });
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
        let l = librarian(&store, &BTreeSet::new(), device, 100);

        let recording = l.recordings().next().expect("a recording");
        let (encoding, locator) = l
            .open(recording, &[device], &[])
            .expect("the library refused a file this device is holding");
        assert_eq!(locator, "/m/callisto.mp3");
        assert_eq!(encoding.audio, AudioId { scheme: 1, hash: 7 });
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
        let mut store = Store::default();
        store.upsert(Track {
            path: std::path::PathBuf::from("/m/callisto.mp3"),
            title: Some("Callisto".into()),
            artist: Some("Bodzin".into()),
            audio_id: Some(AudioId { scheme: 1, hash: 7 }),
            samplerate: Some(48_000),
            channels: Some(2),
            ..Default::default()
        });
        let l = librarian(&store, &BTreeSet::new(), device, 100);

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
        use crate::model::clock::HybridClock;
        use crate::model::encoding::Format;
        use crate::model::log::Fact;

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

        let l = librarian(&store, &records, mine, 200);
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
        let l = librarian(&store, &BTreeSet::new(), mine, 100);

        let recording = l.recordings().next().expect("a recording");
        assert!(l.open(recording, &[theirs], &[]).is_none());
        let _: Option<Id<Recording>> = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_id::AudioId;
    use crate::model::entity::Recording;
    use crate::store::Track;
    use std::path::PathBuf;
    use std::time::Duration;

    fn track(path: &str, title: &str, artist: &str, hash: u128) -> Track {
        Track {
            path: PathBuf::from(path),
            artist: Some(artist.into()),
            albumartist: Some(artist.into()),
            album: Some("An Album".into()),
            title: Some(title.into()),
            tracknumber: Some(1),
            duration: Duration::from_secs(200),
            bitrate: Some(320),
            audio_id: Some(AudioId { scheme: 1, hash }),
            ..Default::default()
        }
    }

    fn device() -> Id<Device> {
        Id::derived(b"test")
    }

    /// A file on disk becomes a row the player can draw.
    #[test]
    fn a_scanned_file_becomes_a_row() {
        let mut store = Store::new();
        store.upsert(track("/m/a.mp3", "Callisto", "Stephan Bodzin", 1));
        let l = librarian(&store, &BTreeSet::new(), device(), 1_000);

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
        let mut store = Store::new();
        store.upsert(track("/m/a.mp3", "Callisto", "Stephan Bodzin", 1));

        // What the recording will be, derived the same way the ladder does.
        let scratch_l = librarian(&store, &BTreeSet::new(), device(), 1_000);
        let recording: Id<Recording> = scratch_l.recordings().next().unwrap();
        assert_eq!(scratch_l.row(recording).unwrap().rating, None);

        let mut writer = Instance::new(device());
        writer.rate(2_000, recording, Some(4));
        writer.play(2_100, recording, Id::derived(b"whatever"));

        let l = librarian(&store, writer.records(), device(), 3_000);
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
        let mut store = Store::new();
        let mbid = uuid::Uuid::from_u128(0x1831_05b5);

        // One recording, one copy here, and it is the short one.
        let mut partial = track("/m/partial.mp3", "Callisto", "Stephan Bodzin", 2);
        partial.mb_recording = Some(mbid);
        partial.duration = Duration::from_secs(12);
        store.upsert(partial);

        let l = librarian(&store, &BTreeSet::new(), device(), 1_000);
        let recording = l.recordings().next().expect("one recording");
        assert_eq!(
            l.row(recording).unwrap().length,
            Duration::from_secs(12),
            "the row reported something other than the copy it holds"
        );
    }

    /// A file with no readable audio has no identity, so there is nothing to    /// A file with no readable audio has no identity, so there is nothing to
    /// catalogue it as. Left out rather than invented.
    #[test]
    fn a_file_with_no_audio_id_is_left_out() {
        let mut store = Store::new();
        let mut orphan = track("/m/orphan.mp3", "Unknown", "Nobody", 1);
        orphan.audio_id = None;
        store.upsert(orphan);
        store.upsert(track("/m/a.mp3", "Callisto", "Stephan Bodzin", 2));

        let l = librarian(&store, &BTreeSet::new(), device(), 1_000);
        assert_eq!(l.recordings().count(), 1);
    }

    /// Two files, identical audio, one recording — and the row still draws.
    /// The library has a real album duplicated in place under two naming
    /// conventions, and it must not appear twice.
    #[test]
    fn one_recording_however_many_copies() {
        let mut store = Store::new();
        store.upsert(track("/m/01 Callisto.mp3", "Callisto", "Stephan Bodzin", 1));
        store.upsert(track(
            "/m/1 - Callisto.mp3",
            "Callisto",
            "Stephan Bodzin",
            1,
        ));

        let l = librarian(&store, &BTreeSet::new(), device(), 1_000);
        assert_eq!(
            l.recordings().count(),
            1,
            "the duplicate became a second row"
        );
    }
}
