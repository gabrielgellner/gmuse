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
use crate::model::entity::{Artist, Device, Distribution, Recording};
use crate::model::id::Id;
use crate::model::instance::{Instance, Scanned};
use crate::model::log::Record;

fn device(n: u128) -> Id<Device> {
    Id::from_uuid(uuid::Uuid::from_u128(n))
}

fn audio(hash: u128) -> AudioId {
    AudioId { scheme: 1, hash }
}

/// Two tracks off one album, as a scan would report them. Both carry a
/// MusicBrainz id, which is the 83% case.
fn album() -> Vec<Scanned> {
    let track = |hash: u128, title: &str, mbid: u128, n: u16| Scanned {
        audio: audio(hash),
        title: title.into(),
        artist: "Stephan Bodzin & Marc Romboy".into(),
        mb_recording: Some(uuid::Uuid::from_u128(mbid)),
        album: Some("Luna".into()),
        album_artist: Some("Stephan Bodzin & Marc Romboy".into()),
        disc: Some(1),
        track: Some(n),
        mb_release_group: Some(uuid::Uuid::from_u128(
            0x9e11_0000_0000_4000_8000_0000_0000_00aa,
        )),
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
            mb_recording: None,
            ..s
        })
        .collect()
}

fn ids(scan: &[Scanned]) -> (Id<Artist>, Id<Recording>, Id<Distribution>) {
    let s = &scan[0];
    (
        Id::<Artist>::derived(s.artist.as_bytes()),
        Id::<Recording>::of_musicbrainz(s.mb_recording.expect("bound fixture")),
        Id::<Distribution>::of_audio(&s.audio),
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
    m2.assign_genre(240, callisto, "Techno");

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
        one.distributions.len(),
        2,
        "one distribution per encoding, not one per device"
    );

    // Nothing collided, because both devices derived the same recording ids.
    assert!(
        one.conflicts.is_empty(),
        "bound tags converge: {:?}",
        one.conflicts
    );

    // The rename is an LWW register; the assignment is row existence.
    assert_eq!(one.artist_names[&artist], "Bodzin & Romboy");
    assert!(one.genres.contains(&(callisto, "Techno".to_string())));

    // Plays are counted, so two devices' listening adds up.
    assert_eq!(one.plays[&callisto], 2, "neither play replaced the other");
}

/// The other half of §11: without a binding to derive from, two devices mint
/// different recordings — and the encoding they agree on is what makes the
/// disagreement visible.
#[test]
fn unbound_tags_surface_a_collision_rather_than_resolving_it() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album_unbound();

    m1.ingest(100, &scan);
    m2.ingest(140, &scan);
    cross_merge(&mut m1, &mut m2, 200);

    let (one, two) = (m1.catalogue(), m2.catalogue());
    assert_eq!(
        postcard::to_allocvec(&one).unwrap(),
        postcard::to_allocvec(&two).unwrap(),
        "they still agree on what happened, including the disagreement"
    );

    assert_eq!(
        one.distributions.len(),
        2,
        "the encodings still converge — that is what makes this detectable"
    );
    assert_eq!(
        one.conflicts.len(),
        2,
        "one per encoding: each was bound to two different recordings"
    );

    let c = &one.conflicts[0];
    assert_eq!(c.base, None, "both moved it from nothing — the same base");
    assert_eq!(c.asserted.len(), 2);
    assert_eq!(
        one.distributions[&c.distribution].recording,
        Some(c.winner()),
        "a value still shows; the disagreement is recorded beside it"
    );
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
        b.assign_genre(200, ids(&scan).1, "Techno");
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

    // Two tracks, one tagged by Picard and one not — the 91% case and the 9%.
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
    assert!(scan[0].mb_recording.is_some() && scan[1].mb_recording.is_none());

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
    assert_eq!(one.distributions.len(), 2, "two files, two encodings");
    assert_eq!(
        one.conflicts.len(),
        1,
        "exactly the untagged track collides; the Picard-tagged one converges"
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

fn held(pairs: &[(Id<Distribution>, &str)]) -> BTreeMap<Id<Distribution>, String> {
    pairs.iter().map(|(d, l)| (*d, l.to_string())).collect()
}

/// **The failure the supersede rule exists to prevent.** Delete a file, and a
/// stale replica must not hand the row back.
#[test]
fn a_deleted_file_does_not_come_back_from_a_stale_replica() {
    let (mut m1, mut m2) = (Instance::new(device(1)), Instance::new(device(2)));
    let scan = album();
    let (a, b) = (
        Id::<Distribution>::of_audio(&scan[0].audio),
        Id::<Distribution>::of_audio(&scan[1].audio),
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
    let a = Id::<Distribution>::of_audio(&scan[0].audio);
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
    let a = Id::<Distribution>::of_audio(&scan[0].audio);
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
    let a = Id::<Distribution>::of_audio(&scan[0].audio);
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
