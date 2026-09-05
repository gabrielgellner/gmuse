//! **The test the architecture stands or falls on** — issue #69.
//!
//! Two instances, one album, ingested independently, edited differently,
//! played on both, then merged in both directions. It touches every risky
//! decision at once — convergent minting, the clock, row-union merging and
//! collision surfacing — and its passing is the difference between the
//! no-coordinator bet holding in code and holding only in prose.
//!
//! Everything here is a test. The module exists so the assertions have one
//! place to live and one name to fail under.

#![cfg(test)]

use std::collections::BTreeSet;

use crate::audio_id::AudioId;
use crate::model::basis::Basis;
use crate::model::entity::{Artist, Device, Encoding, Genre, Recording};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::instance::{Instance, Scanned};
use crate::model::log::Record;
use crate::model::reference::Source;
use crate::model::vocabulary::GenreOn;

fn device(n: u128) -> Id<Device> {
    Id::from_uuid(uuid::Uuid::from_u128(n))
}

fn audio(hash: u128) -> AudioId {
    AudioId { scheme: 1, hash }
}

/// Two tracks off one album, as a scan would report them. Both carry a
/// MusicBrainz id, which is the 92.7% case.
fn album() -> Vec<Scanned> {
    let track = |hash: u128, title: &str, mbid: u128, n: u16| Scanned {
        audio: audio(hash),
        title: title.into(),
        artist: "Stephan Bodzin & Marc Romboy".into(),
        identity: Some(Basis::MusicBrainz(uuid::Uuid::from_u128(mbid))),
        artist_mbid: Some(uuid::Uuid::from_u128(0xb0d21)),
        credit_is_joined: false,
        album_artist_mbid: Some(uuid::Uuid::from_u128(0xb0d21)),
        album: Some("Luna".into()),
        album_artist: Some("Stephan Bodzin & Marc Romboy".into()),
        disc: Some(1),
        track: Some(n),
        length_ms: 200_000 + u32::from(n),
        mb_release_group: Some(uuid::Uuid::from_u128(
            0x9e11_0000_0000_4000_8000_0000_0000_00aa,
        )),
        // A *different* id from the group's, because they name different
        // things: the album and the pressing of it that this file came from.
        mb_release: Some(uuid::Uuid::from_u128(
            0x5e1e_0000_0000_4000_8000_0000_0000_00bb,
        )),
        format: Some(crate::model::encoding::Format::Mp3),
        bitrate: 320,
        released: None,
        first_released: None,
        label: None,
        sample_rate: Some(44_100),
        channels: Some(2),
    };
    vec![
        track(
            0x8fb0_dcfb_f145_dfff_6649_7842_65da_0229,
            "Callisto",
            0xca11_1500_0000_4000_8000_0000_0000_0001,
            1,
        ),
        track(
            0x1111_2222_3333_4444_5555_6666_7777_8888,
            "Luna",
            0x1a2b_0000_0000_4000_8000_0000_0000_0002,
            2,
        ),
    ]
}

/// The same album, with no MusicBrainz ids — so each device must mint locally
/// and the two cannot converge on their own.
fn album_unbound() -> Vec<Scanned> {
    album()
        .into_iter()
        .map(|s| Scanned {
            identity: None,
            ..s
        })
        .collect()
}

fn ids(scan: &[Scanned]) -> (Id<Artist>, Id<Recording>, Id<Encoding>) {
    let s = &scan[0];
    (
        Id::<Artist>::derived(s.artist.as_bytes()),
        Id::<Recording>::of_basis(s.identity.as_ref().expect("bound fixture")),
        Id::<Encoding>::of_audio(&s.audio),
    )
}

/// Merge both ways round, as a real sync eventually does.
fn cross_merge(a: &mut Instance, b: &mut Instance, now: i64) {
    let (from_a, from_b): (BTreeSet<Record>, BTreeSet<Record>) =
        (a.records().clone(), b.records().clone());
    a.merge(now, &from_b);
    b.merge(now, &from_a);
}

/// **The whole test.** The four assertions the design is betting on.
#[test]
fn two_instances_one_album_and_a_full_round_trip() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album();
    let (artist, callisto, callisto_file) = ids(&scan);

    // 1. Both ingest the same album, independently, having never met.
    m1.ingest(100, &scan);
    m2.ingest(140, &scan);

    // 2. One renames an artist; the other assigns a genre.
    m1.rename_artist(200, artist, "Bodzin & Romboy");
    m2.assign_genre(
        240,
        GenreOn::Recording(callisto),
        "Techno",
        Source::TagVariant,
        Confidence::Confirmed,
    );

    // 3. Each plays a track.
    m1.play(300, callisto, callisto_file);
    m2.play(340, callisto, callisto_file);

    // 4. Sync, both directions.
    cross_merge(&mut m1, &mut m2, 400);

    let (one, two) = (m1.catalogue(), m2.catalogue());

    // Agreement through determinism — no coordination happened.
    assert_eq!(
        postcard::to_allocvec(&one).unwrap(),
        postcard::to_allocvec(&two).unwrap(),
        "the two folds must be byte-identical"
    );

    // Convergent minting: the same bytes are one encoding, not two.
    assert_eq!(
        one.encodings.len(),
        2,
        "one encoding per encoding, not one per device"
    );

    // Nothing collided, because both devices derived the same recording ids.
    assert!(
        one.conflicts.is_empty(),
        "bound tags converge: {:?}",
        one.conflicts
    );

    // The rename is an LWW register; the assignment is row existence.
    assert_eq!(one.artist_names[&artist], "Bodzin & Romboy");
    let techno = Id::<Genre>::of_name("Techno");
    assert_eq!(
        one.genre_names[&techno], "Techno",
        "the concept has to be resolvable, or the id renders as nothing"
    );
    // **Both halves, and the basis is the one that used to be dropped.** The
    // conversion from a proposal read `{ recording, genre, .. }`, so a Discogs
    // guess and a typed answer arrived in the log as the same fact.
    //
    // The basis is in the *key*, so it crosses the merge or the row is not
    // found at all — a claim without an asserter does not exist here.
    assert_eq!(
        one.genres[&(GenreOn::Recording(callisto), techno, Source::TagVariant)],
        Confidence::Confirmed,
        "the basis crossed the merge with the assignment"
    );

    // Plays are counted, so two devices' listening adds up.
    assert_eq!(one.plays[&callisto], 2, "neither play replaced the other");
}

/// **The collision §11 actually describes**, now that unbound files converge
/// on their bytes.
///
/// It is not two devices reading one untagged disk — they agree, because the
/// audio is the same and the id derives from it. It is one machine's copy
/// being Picard-tagged and the other's not: same bytes, so the same encoding,
/// but one names a MusicBrainz recording and the other names the bytes.
///
/// That is an ordinary situation — you tag a library on one machine and copy
/// it to another before syncing — and it is exactly the shape §4 routes to a
/// surfaced correction: one structural field, two writes, the same empty base.
#[test]
fn a_tagged_and_an_untagged_copy_surface_a_collision() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    m1.ingest(100, &album()); // Picard-tagged
    m2.ingest(140, &album_unbound()); // the same files, untagged
    cross_merge(&mut m1, &mut m2, 200);

    let (one, two) = (m1.catalogue(), m2.catalogue());
    assert_eq!(
        postcard::to_allocvec(&one).unwrap(),
        postcard::to_allocvec(&two).unwrap(),
        "they agree on what happened, including the disagreement"
    );
    assert_eq!(
        one.encodings.len(),
        2,
        "the encodings converge — that is what makes this detectable"
    );
    assert_eq!(one.conflicts.len(), 2, "one per encoding");

    let c = &one.conflicts[0];
    assert_eq!(c.base, None, "both moved it from nothing — the same base");
    assert_eq!(
        one.encodings[&c.encoding].recording,
        Some(c.winner()),
        "a value still shows; the disagreement is recorded beside it"
    );

    // **Why convergent minting is not something last-writer-wins could have
    // done instead.** Creating an entity is row existence, and row existence
    // has no field to overwrite — so both recordings are here. LWW decided
    // which one the *encoding* points at; it could not decide that the other
    // should not exist.
    assert_eq!(
        one.recordings.len(),
        4,
        "two songs, and two recordings each — the loser is an orphan with a \
         title and an artist attached to nothing, and only a merge can \
         retire it"
    );
    let bound: std::collections::BTreeSet<_> =
        one.encodings.values().filter_map(|d| d.recording).collect();
    assert_eq!(bound.len(), 2, "and half of them are bound to nothing");

    // The orphan is not swept, because a recording nothing holds is an
    // ordinary row — a wishlist entry looks exactly the same. It is
    // *proposed for merging* instead, which retires it without deleting it.
    let proposals = one.proposed_merges();
    assert_eq!(proposals.len(), 2, "one per song");
    for (loser, winner) in proposals {
        assert!(!bound.contains(&loser), "the loser is the unbound one");
        assert!(bound.contains(&winner));
        assert_eq!(
            one.recordings[&loser].title, one.recordings[&winner].title,
            "and they are the same song, which is what makes it a merge"
        );
    }
}

/// And the case that is **no longer** a collision, which is the point of
/// deriving an unbound recording from its bytes: two devices reading the same
/// untagged disk have nothing to disagree about, and now say so.
#[test]
fn two_devices_reading_one_untagged_disk_agree() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    m1.ingest(100, &album_unbound());
    m2.ingest(140, &album_unbound());
    cross_merge(&mut m1, &mut m2, 200);

    let one = m1.catalogue();
    assert!(
        one.conflicts.is_empty(),
        "nothing to reconcile: {:?}",
        one.conflicts
    );
    assert_eq!(one.recordings.len(), 2, "two songs, not four");
}

/// Merge order must not change the answer, or "both devices agree" is luck.
#[test]
fn the_result_does_not_depend_on_which_way_the_sync_ran() {
    let scan = album();
    let build = |first_device, second_device, forward: bool| {
        let (mut a, mut b) = (
            Instance::new(device(first_device)),
            Instance::new(device(second_device)),
        );
        a.ingest(100, &scan);
        b.ingest(140, &scan);
        b.assign_genre(
            200,
            GenreOn::Recording(ids(&scan).1),
            "Techno",
            Source::TagVariant,
            Confidence::Confirmed,
        );
        if forward {
            let from_b = b.records().clone();
            a.merge(300, &from_b);
            a.catalogue()
        } else {
            let from_a = a.records().clone();
            b.merge(300, &from_a);
            b.catalogue()
        }
    };
    assert_eq!(build(1, 2, true), build(1, 2, false));
}

/// Syncing twice is a no-op, so a bundle copied round a loop cannot
/// double-count a play.
#[test]
fn merging_the_same_log_twice_changes_nothing() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album();
    let (_, callisto, file) = ids(&scan);
    m1.ingest(100, &scan);
    m2.ingest(140, &scan);
    m2.play(200, callisto, file);

    let from_m2 = m2.records().clone();
    m1.merge(300, &from_m2);
    let once = m1.catalogue();
    m1.merge(400, &from_m2);

    assert_eq!(m1.catalogue(), once, "a repeated sync adds nothing");
    assert_eq!(once.plays[&callisto], 1);
}

/// **Causality across a sync.** After reading another device's log, everything
/// this device writes sorts after what it read — which is what makes "last
/// wins" mean "last" rather than "whichever clock ran faster".
///
/// Without this the round trip still passes, which is why it is asserted
/// separately: a sync that does not advance the clock is invisible until two
/// machines disagree about what happened after what.
#[test]
fn what_is_written_after_a_sync_sorts_after_what_the_sync_brought() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album();
    let (artist, _, _) = ids(&scan);

    // m2 is far ahead in wall time, as an unsynced laptop would be.
    m2.ingest(9_000, &scan);
    m1.ingest(100, &scan);

    let newest_from_m2 = m2.records().iter().next_back().expect("m2 wrote").stamp;
    let from_m2 = m2.records().clone();
    m1.merge(200, &from_m2);

    // Physical time here is still behind m2's — only `observe` can save this.
    m1.rename_artist(200, artist, "Bodzin & Romboy");
    let ours = m1.records().iter().next_back().expect("m1 wrote").stamp;

    assert!(
        ours > newest_from_m2,
        "a write after a sync must follow what the sync brought in"
    );
    assert_eq!(
        m1.catalogue().artist_names[&artist],
        "Bodzin & Romboy",
        "and therefore it is the value that shows"
    );
}

/// **The slice, over real files.** Everything above builds its scan from a
/// fixture, which tests merging but not reading. This does the same round trip
/// starting from bytes on disk, so the ingest ladder is inside the assertion
/// rather than beside it.
#[test]
fn two_instances_ingesting_the_same_real_files() {
    use crate::model::ingest;

    let dir = std::env::temp_dir().join("gmuse-round-trip-files");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Two tracks, one tagged by Picard and one not — the 92.7% case and the 7.3%.
    let mut paths = Vec::new();
    for (n, (title, mbid)) in [
        ("Callisto", Some("183105b5-0000-4000-8000-000000000000")),
        ("Luna", None),
    ]
    .into_iter()
    .enumerate()
    {
        let p = dir.join(format!("{:02} {title}.wav", n + 1));
        write_wav(&p, 4410 + n * 10);
        write_tags(&p, title, "Stephan Bodzin & Marc Romboy", mbid);
        paths.push(p);
    }

    let scan: Vec<_> = paths
        .iter()
        .map(|p| ingest::read(p).expect("reads"))
        .collect();
    assert_eq!(scan[0].title, "Callisto");
    assert!(scan[0].identity.is_some() && scan[1].identity.is_none());

    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    m1.ingest(100, &scan);
    m2.ingest(140, &scan);
    cross_merge(&mut m1, &mut m2, 200);

    let (one, two) = (m1.catalogue(), m2.catalogue());
    assert_eq!(
        postcard::to_allocvec(&one).unwrap(),
        postcard::to_allocvec(&two).unwrap(),
        "reading real files does not change the agreement"
    );
    assert_eq!(one.encodings.len(), 2, "two files, two encodings");
    assert!(
        one.conflicts.is_empty(),
        "two devices reading the same disk have nothing to disagree about — \
         the tagged track converges on its MusicBrainz id and the untagged one \
         on its bytes: {:?}",
        one.conflicts
    );
}

/// A tiny real WAV, so there is something to hash.
pub(crate) fn write_wav(path: &std::path::Path, frames: usize) {
    use std::io::Write;
    let bytes = 4 * frames;
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&((36 + bytes) as u32).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&2u16.to_le_bytes()).unwrap();
    f.write_all(&44100u32.to_le_bytes()).unwrap();
    f.write_all(&176_400u32.to_le_bytes()).unwrap();
    f.write_all(&4u16.to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&(bytes as u32).to_le_bytes()).unwrap();
    for i in 0..frames {
        let s = ((i % 100) as i16).wrapping_mul(300);
        f.write_all(&s.to_le_bytes()).unwrap();
        f.write_all(&s.to_le_bytes()).unwrap();
    }
}

/// The tag Picard writes: a real `UFID` frame, not a text item.
pub(crate) fn write_tags(path: &std::path::Path, title: &str, artist: &str, mbid: Option<&str>) {
    use lofty::config::WriteOptions;
    use lofty::id3::v2::{Frame, Id3v2Tag, UniqueFileIdentifierFrame};
    use lofty::prelude::{Accessor, TagExt};

    let mut t = Id3v2Tag::default();
    t.set_title(title.to_string());
    t.set_artist(artist.to_string());
    if let Some(id) = mbid {
        t.insert(Frame::UniqueFileIdentifier(UniqueFileIdentifierFrame::new(
            "http://musicbrainz.org",
            id.as_bytes().to_vec(),
        )));
    }
    t.save_to_path(path, WriteOptions::default()).unwrap();
}

/// A device whose clock is set wrong still sorts, and is still reported.
#[test]
fn a_wrong_clock_is_reported_across_a_sync() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album();
    m1.ingest(100, &scan);
    m2.ingest(100 + crate::model::clock::IMPLAUSIBLE_DRIFT_MS * 2, &scan);

    let from_m2 = m2.records().clone();
    m1.merge(200, &from_m2);

    assert!(
        !m1.drifts.is_empty(),
        "a machine that far ahead is something to be told about"
    );
}

// ---------------------------------------------------------------------------
// Holdings — the one table where merge is not union (#71).
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;

fn held(pairs: &[(Id<Encoding>, &str)]) -> BTreeMap<Id<Encoding>, String> {
    pairs.iter().map(|(d, l)| (*d, l.to_string())).collect()
}

/// **The failure the supersede rule exists to prevent.** Delete a file, and a
/// stale replica must not hand the row back.
#[test]
fn a_deleted_file_does_not_come_back_from_a_stale_replica() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album();
    let (a, b) = (
        Id::<Encoding>::of_audio(&scan[0].audio),
        Id::<Encoding>::of_audio(&scan[1].audio),
    );
    m1.ingest(100, &scan);

    // m1 holds both, and m2 syncs while that is still true.
    m1.assert_holdings(200, held(&[(a, "/m/1.mp3"), (b, "/m/2.mp3")]));
    let stale = m1.records().clone();
    m2.merge(210, &stale);

    // Then one is deleted on m1, which says so by asserting the new whole set.
    m1.assert_holdings(300, held(&[(a, "/m/1.mp3")]));

    // Sync both ways. The stale assertion is still in both logs.
    cross_merge(&mut m1, &mut m2, 400);

    for cat in [m1.catalogue(), m2.catalogue()] {
        let holdings = &cat.holdings[&m1.device()].held;
        assert!(holdings.contains_key(&a));
        assert!(
            !holdings.contains_key(&b),
            "union would have resurrected the deleted file"
        );
    }
}

/// Only the owning device's own assertion is superseded; one device's scan
/// never speaks for another's disk.
#[test]
fn each_device_speaks_only_for_itself() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album();
    let a = Id::<Encoding>::of_audio(&scan[0].audio);
    m1.ingest(100, &scan);

    m1.assert_holdings(200, held(&[(a, "/laptop/1.mp3")]));
    m2.assert_holdings(210, held(&[(a, "/Volumes/Media/1.mp3")]));
    cross_merge(&mut m1, &mut m2, 300);

    let cat = m1.catalogue();
    let mut holders = cat.holders(a);
    holders.sort_by_key(|(d, _, _)| *d);
    assert_eq!(holders.len(), 2, "both devices hold a copy");
    assert!(holders.iter().any(|(_, l, _)| *l == "/laptop/1.mp3"));
    assert!(holders.iter().any(|(_, l, _)| *l == "/Volumes/Media/1.mp3"));
}

/// A remote holding is an assertion **with an age**, which is what lets the UI
/// say "as of Tuesday" rather than pretending it is live.
#[test]
fn a_holding_carries_when_it_was_claimed() {
    let mut m1 = Instance::new(device(1));
    let scan = album();
    let a = Id::<Encoding>::of_audio(&scan[0].audio);
    m1.ingest(100, &scan);
    m1.assert_holdings(1_788_115_007_000, held(&[(a, "/m/1.mp3")]));

    let cat = m1.catalogue();
    let (_, _, at) = cat.holders(a)[0];
    assert_eq!(at.wall, 1_788_115_007_000, "the claim knows its own age");
}

/// **The property that makes compaction safe.** Folding a compacted log gives
/// exactly what folding the original gives, so it can run whenever.
#[test]
fn compaction_cannot_change_what_the_library_says() {
    let mut m1 = Instance::new(device(1));
    let scan = album();
    let (_, callisto, file) = ids(&scan);
    m1.ingest(100, &scan);
    m1.play(150, callisto, file);
    for n in 0..5 {
        m1.assert_holdings(200 + n * 10, held(&[(file, "/m/1.mp3")]));
    }

    let before = m1.catalogue();
    let counted = m1.records().len();
    m1.compact();

    assert_eq!(m1.catalogue(), before, "the answer is untouched");
    assert_eq!(
        m1.records().len(),
        counted - 4,
        "four superseded assertions went, and nothing else"
    );
    assert_eq!(m1.catalogue().plays[&callisto], 1, "the play survived");
}

/// Compaction on one device and not another must not make them disagree — a
/// compacted record arriving back from a stale replica is simply superseded
/// again.
#[test]
fn compacting_one_device_does_not_split_the_answer() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album();
    let a = Id::<Encoding>::of_audio(&scan[0].audio);
    m1.ingest(100, &scan);
    m1.assert_holdings(200, held(&[(a, "/old.mp3")]));
    m1.assert_holdings(300, held(&[(a, "/new.mp3")]));

    let uncompacted = m1.records().clone();
    m1.compact();
    m2.merge(400, &uncompacted); // m2 still has the superseded one
    let from_m2 = m2.records().clone();
    m1.merge(410, &from_m2); // and hands it back

    assert_eq!(
        m1.catalogue().holdings[&m1.device()].held[&a],
        "/new.mp3",
        "the stale assertion is superseded again, not resurrected"
    );
    assert_eq!(m1.catalogue(), m2.catalogue());
}
