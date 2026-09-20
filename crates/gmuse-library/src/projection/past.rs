//! **Which facts change the past, and the net for an incremental fold** — #261 Goals 2 and 4.
//!
//! `advance` refolds today, so `advance ≡ rebuild` is true by construction. That is
//! exactly the moment to write the net: a net written after the thing it catches is a net
//! shaped by it. Every scenario here is a record set in the order it **arrives**, cut where
//! an `advance` would see it — and arrival order is not stamp order, because a sync merges
//! another device's records with stamps below everything already folded.
//!
//! ## What the audit of `fold` found
//!
//! Three shapes reach backwards, and `Fact::blast()` had named one of them.
//!
//! - **The merges**, applied in a first pass and resolved through the finished set. Wide,
//!   and classified so.
//! - **Facts resolved in a pass after the loop, against the finished set.** `EncodingPlaced`
//!   picks the best-evidenced of *every* placement the copy was ever given, and the rung it
//!   ranks by is a property of the release the track sits on — so a `ReleaseCatalogued`,
//!   `MediumCatalogued` or `TrackPlaced` arriving later flips the placement of encodings it
//!   never names. `EncodingBound` is last-by-stamp over every binding, with `conflicts`
//!   derived from the whole set. A retitle is applied last and outranks a
//!   `RecordingCatalogued` **whatever its stamp**. The three spine facts were classified
//!   narrow, and their arm said one "cannot reach past the pressing it identifies". The fold
//!   says otherwise, and [`a_release_catalogued_later_moves_a_placement_it_never_names`] is
//!   the proof.
//! - **Every last-writer-wins map.** `ratings`, `artist_names`, `claims`, the credit shadow,
//!   retired-and-restored — correct only because the set is folded in stamp order. Applied in
//!   *arrival* order they are wrong, and the catalogue keeps no stamps to notice. The log has
//!   one device today, so this cannot happen until sync lands; the rule an incremental fold
//!   needs is that a record stamped below anything already folded is a rebuild.
//!
//! The candidate sets the second shape needs are small: on the real library 9,467 placements,
//! 9,730 bindings and 33 retitles — under a megabyte by count, **3.1 MB kept** once every
//! encoding's `Vec` and the map nodes are paid for (`just memory-parts`, 11.8 → 14.9 MB for
//! the fold) — against the 83.5 MB of records they let go of. Retaining them is the trade;
//! classifying those facts as rebuild is not needed.

use std::collections::BTreeSet;

use super::{Advanced, Projected};
use crate::facts::{Blast, Fact, Record, Touched};
use crate::model::clock::Stamp;
use crate::model::entity::{
    Artist, Device, Encoding, Medium, Recording, Release, ReleaseGroup, Role, Track,
};
use crate::model::id::Id;
use crate::model::reference::{CreditOn, Source};
use crate::projection::catalogue::{Catalogue, fold};

fn device(n: u128) -> Id<Device> {
    Id::from_uuid(uuid::Uuid::from_u128(n))
}

/// A record with a chosen clock, so a scenario can say "this one is older" outright.
fn at(d: Id<Device>, wall: i64, fact: Fact) -> Record {
    Record {
        stamp: Stamp {
            wall,
            counter: 0,
            device: d,
        },
        fact,
    }
}

fn catalogued(recording: Id<Recording>, title: &str) -> Fact {
    catalogued_by(recording, title, b"someone")
}

fn catalogued_by(recording: Id<Recording>, title: &str, artist: &[u8]) -> Fact {
    Fact::RecordingCatalogued {
        recording,
        title: title.into(),
        artist: Id::<Artist>::derived(artist),
    }
}

/// One scenario: what arrives, in batches, and what the fold must say at the end.
struct Scenario {
    name: &'static str,
    /// Each batch is what one `advance` sees on top of the previous ones. Within and
    /// across batches the stamps are whatever the scenario needs, not ascending.
    batches: Vec<Vec<Record>>,
    /// The semantics the whole set has to fold to, checked on the rebuild *and* on the
    /// advanced projection — so a scenario pins the fold as well as feeding the property.
    check: fn(&Catalogue),
}

fn scenarios() -> Vec<Scenario> {
    let here = device(1);
    let slow = device(2);
    let a = Id::<Recording>::derived(b"a");
    let b = Id::<Recording>::derived(b"b");
    let copy = Id::<Encoding>::derived(b"a copy");
    let role = Id::<Role>::derived(b"performer");
    let who = Id::<Artist>::derived(b"who");

    // The spine a placement is ranked against: one release minted from a MusicBrainz
    // id and one from the album's name, each with a disc and a slot for the same song.
    let group = Id::<ReleaseGroup>::derived(b"luna");
    let named = Id::<Release>::of_release(None, group);
    let minted = Id::<Release>::of_musicbrainz(uuid::Uuid::from_u128(0xdc70));
    let good = Id::<Medium>::of_medium(minted, 1);
    let weak = Id::<Medium>::of_medium(named, 1);
    let on_good = Id::<Track>::of_track(good, a);
    let on_weak = Id::<Track>::of_track(weak, a);
    let disc = |release, medium| Fact::MediumCatalogued {
        medium,
        release,
        position: 1,
        format: None,
        tracks: Some(1),
    };
    let pressing = |release| Fact::ReleaseCatalogued {
        release,
        group,
        title: "Luna".into(),
        released: None,
        label: None,
        media: Some(1),
    };
    let slot = |track, medium| Fact::TrackPlaced {
        track,
        medium,
        recording: a,
        position: 1,
        title_on_release: None,
    };
    let placed = |track| Fact::EncodingPlaced {
        encoding: copy,
        track,
        basis: Source::TagVariant,
    };
    let credit = |basis| Fact::CreditAsserted {
        subject: CreditOn::Recording(a),
        artist: who,
        role,
        basis,
        position: 0,
        as_credited: None,
        join_phrase: None,
    };

    vec![
        Scenario {
            name: "a merge arrives after the rating and the plays it moves",
            batches: vec![
                vec![
                    at(here, 1_000, catalogued(a, "A")),
                    at(here, 1_001, catalogued(b, "B")),
                    at(
                        here,
                        1_002,
                        Fact::RecordingRated {
                            recording: a,
                            stars: Some(4),
                        },
                    ),
                    at(
                        here,
                        1_003,
                        Fact::RecordingPlayed {
                            recording: a,
                            encoding: None,
                        },
                    ),
                ],
                vec![at(
                    here,
                    2_000,
                    Fact::RecordingMerged {
                        loser: a,
                        winner: b,
                    },
                )],
            ],
            check: |cat| {
                let b = Id::<Recording>::derived(b"b");
                let a = Id::<Recording>::derived(b"a");
                assert_eq!(cat.ratings.get(&b), Some(&4), "the rating did not move");
                assert_eq!(cat.plays.get(&b), Some(&1), "the play did not move");
                assert!(!cat.recordings.contains_key(&a), "the loser kept a row");
            },
        },
        Scenario {
            name: "a retitle arrives before the cataloguing it outranks",
            batches: vec![
                vec![at(
                    here,
                    1_000,
                    Fact::RecordingRetitled {
                        recording: a,
                        title: "Right".into(),
                    },
                )],
                vec![at(here, 2_000, catalogued(a, "From the tags"))],
            ],
            check: |cat| {
                let a = Id::<Recording>::derived(b"a");
                assert_eq!(
                    cat.recordings[&a].title, "Right",
                    "a scan overwrote a retitle"
                );
            },
        },
        Scenario {
            name: "a scan re-catalogues over a retitle",
            batches: vec![
                vec![
                    at(here, 1_000, catalogued(a, "From the tags")),
                    at(
                        here,
                        1_001,
                        Fact::RecordingRetitled {
                            recording: a,
                            title: "Right".into(),
                        },
                    ),
                ],
                // The tags changed, so the scan moves the artist — and the title
                // still does not follow it.
                vec![at(
                    here,
                    2_000,
                    catalogued_by(a, "From the tags again", b"someone else"),
                )],
            ],
            check: |cat| {
                let a = Id::<Recording>::derived(b"a");
                assert_eq!(
                    cat.recordings[&a].title, "Right",
                    "a later scan undid a retitle"
                );
                assert_eq!(
                    cat.recordings[&a].artist,
                    Id::<Artist>::derived(b"someone else"),
                    "the scan's artist did not land, so the batch was not applied"
                );
            },
        },
        Scenario {
            name: "a release is catalogued after the placement it ranks",
            batches: vec![
                vec![
                    at(here, 1_000, catalogued(a, "Triton")),
                    at(here, 1_001, pressing(named)),
                    at(here, 1_002, disc(minted, good)),
                    at(here, 1_003, disc(named, weak)),
                    at(here, 1_004, slot(on_good, good)),
                    at(here, 1_005, slot(on_weak, weak)),
                    // The MusicBrainz slot first, then the name-minted one — so with
                    // no release row to rank the first by, the last word wins.
                    at(here, 1_006, placed(on_good)),
                    at(here, 1_007, placed(on_weak)),
                ],
                // Names one release, and moves a placement of an encoding it never names.
                vec![at(here, 2_000, pressing(minted))],
            ],
            check: |cat| {
                let copy = Id::<Encoding>::derived(b"a copy");
                let a = Id::<Recording>::derived(b"a");
                let minted = Id::<Release>::of_musicbrainz(uuid::Uuid::from_u128(0xdc70));
                let on_good = Id::<Track>::of_track(Id::<Medium>::of_medium(minted, 1), a);
                assert_eq!(
                    cat.placed.get(&copy),
                    Some(&on_good),
                    "the release arriving did not re-rank the placement"
                );
            },
        },
        Scenario {
            name: "a worse-evidenced placement arrives after a better one",
            batches: vec![
                vec![
                    at(here, 1_000, catalogued(a, "Triton")),
                    at(here, 1_001, pressing(named)),
                    at(here, 1_002, pressing(minted)),
                    at(here, 1_003, disc(minted, good)),
                    at(here, 1_004, disc(named, weak)),
                    at(here, 1_005, slot(on_good, good)),
                    at(here, 1_006, slot(on_weak, weak)),
                    at(here, 1_007, placed(on_good)),
                ],
                // Narrow, and the last word — which is not the best one.
                vec![at(here, 2_000, placed(on_weak))],
            ],
            check: |cat| {
                let copy = Id::<Encoding>::derived(b"a copy");
                let a = Id::<Recording>::derived(b"a");
                let minted = Id::<Release>::of_musicbrainz(uuid::Uuid::from_u128(0xdc70));
                let on_good = Id::<Track>::of_track(Id::<Medium>::of_medium(minted, 1), a);
                assert_eq!(
                    cat.placed.get(&copy),
                    Some(&on_good),
                    "the later, worse-evidenced placement won"
                );
            },
        },
        Scenario {
            name: "a second binding arrives and is a conflict",
            batches: vec![
                vec![at(
                    here,
                    1_000,
                    Fact::EncodingBound {
                        encoding: copy,
                        from: None,
                        to: a,
                    },
                )],
                vec![at(
                    here,
                    2_000,
                    Fact::EncodingBound {
                        encoding: copy,
                        from: None,
                        to: b,
                    },
                )],
            ],
            check: |cat| {
                let copy = Id::<Encoding>::derived(b"a copy");
                let b = Id::<Recording>::derived(b"b");
                assert_eq!(
                    cat.bound.get(&copy),
                    Some(&b),
                    "the later binding does not show"
                );
                assert_eq!(
                    cat.conflicts.len(),
                    1,
                    "two answers and no conflict recorded"
                );
            },
        },
        Scenario {
            name: "a rating from a slower clock arrives last",
            batches: vec![
                vec![
                    at(here, 9_000, catalogued(a, "A")),
                    at(
                        here,
                        9_001,
                        Fact::RecordingRated {
                            recording: a,
                            stars: Some(5),
                        },
                    ),
                ],
                vec![at(
                    slow,
                    1_000,
                    Fact::RecordingRated {
                        recording: a,
                        stars: Some(2),
                    },
                )],
            ],
            check: |cat| {
                let a = Id::<Recording>::derived(b"a");
                assert_eq!(
                    cat.ratings.get(&a),
                    Some(&5),
                    "the older rating had the last word"
                );
            },
        },
        Scenario {
            name: "an artist named by a slower clock arrives last",
            batches: vec![
                vec![at(
                    here,
                    9_000,
                    Fact::ArtistNamed {
                        artist: who,
                        name: "Current".into(),
                    },
                )],
                vec![at(
                    slow,
                    1_000,
                    Fact::ArtistNamed {
                        artist: who,
                        name: "Stale".into(),
                    },
                )],
            ],
            check: |cat| {
                let who = Id::<Artist>::derived(b"who");
                assert_eq!(
                    cat.artist_names[&who], "Current",
                    "the older name had the last word"
                );
            },
        },
        Scenario {
            name: "a retraction from a slower clock arrives after the assertion that lifts it",
            batches: vec![
                vec![
                    at(here, 9_000, catalogued(a, "A")),
                    at(here, 9_001, credit(Source::Manual)),
                ],
                vec![at(
                    slow,
                    1_000,
                    Fact::CreditRetracted {
                        subject: CreditOn::Recording(a),
                        artist: who,
                        role,
                        basis: Source::Manual,
                    },
                )],
            ],
            check: |cat| {
                let a = Id::<Recording>::derived(b"a");
                let who = Id::<Artist>::derived(b"who");
                let role = Id::<Role>::derived(b"performer");
                assert!(
                    cat.credits
                        .contains_key(&(CreditOn::Recording(a), who, role, Source::Manual)),
                    "a retraction stamped before the assertion shadowed it"
                );
            },
        },
        Scenario {
            name: "a binding from a slower clock arrives last",
            batches: vec![
                vec![at(
                    here,
                    9_000,
                    Fact::EncodingBound {
                        encoding: copy,
                        from: None,
                        to: a,
                    },
                )],
                vec![at(
                    slow,
                    1_000,
                    Fact::EncodingBound {
                        encoding: copy,
                        from: None,
                        to: b,
                    },
                )],
            ],
            check: |cat| {
                let copy = Id::<Encoding>::derived(b"a copy");
                let a = Id::<Recording>::derived(b"a");
                assert_eq!(cat.bound.get(&copy), Some(&a), "the older binding won");
                assert_eq!(
                    cat.conflicts.len(),
                    1,
                    "two answers and no conflict recorded"
                );
            },
        },
        Scenario {
            name: "a restore arrives before the retirement it undoes",
            batches: vec![
                vec![at(here, 9_000, Fact::EncodingRestored { encoding: copy })],
                vec![at(slow, 1_000, Fact::EncodingRetired { encoding: copy })],
            ],
            check: |cat| {
                let copy = Id::<Encoding>::derived(b"a copy");
                assert!(
                    !cat.retired.contains(&copy),
                    "a retirement stamped before the restore stood"
                );
            },
        },
    ]
}

/// Everything that has arrived by the end of batch `n`.
fn through(s: &Scenario, n: usize) -> BTreeSet<Record> {
    s.batches[..=n].iter().flatten().cloned().collect()
}

/// The batch as an arrival-order apply would see it: stamped above everything before it.
///
/// What a last-writer-wins map applied in arrival order actually computes, and so the
/// answer a wrong incremental fold would give.
fn as_if_newest(before: &BTreeSet<Record>, batch: &[Record]) -> BTreeSet<Record> {
    let top = before.iter().map(|r| r.stamp.wall).max().unwrap_or(0);
    let mut out = before.clone();
    out.extend(batch.iter().enumerate().map(|(i, r)| Record {
        stamp: Stamp {
            wall: top + 1 + i as i64,
            counter: 0,
            device: r.stamp.device,
        },
        fact: r.fact.clone(),
    }));
    out
}

/// **Every batch could fail the property**, or it is vacuous for that scenario.
///
/// The instrument check, in two shapes because the scenarios come in two. A batch that
/// arrives in stamp order has to *move* the catalogue, or `advance ≡ rebuild` holds for it
/// whatever `advance` does. A batch from a slower clock is meant to move nothing — the
/// older record loses — so for it the proof is the other way round: applied as if it were
/// newest, which is what an arrival-order fold would do, the answer has to differ.
#[test]
fn every_batch_could_fail_the_property() {
    let all = scenarios();
    assert!(all.len() >= 11, "the table was not built: {}", all.len());
    for s in &all {
        assert!(
            s.batches.len() >= 2,
            "{}: a scenario needs an arrival to test",
            s.name
        );
        for n in 1..s.batches.len() {
            let before = through(s, n - 1);
            let truth = fold(&through(s, n));
            let moved = fold(&before) != truth;
            let would_differ = fold(&as_if_newest(&before, &s.batches[n])) != truth;
            assert!(
                moved || would_differ,
                "{}: batch {n} changes nothing in either order, so the property cannot fail on it",
                s.name
            );
        }
        (s.check)(&fold(&through(s, s.batches.len() - 1)));
    }
}

/// **`advance` lands where `rebuild` would, in every arrival order** — the net.
///
/// Batch by batch, so an implementation that applies one fact at a time is checked at
/// each point it could have gone wrong, and against the scenario's own expectation as
/// well as against a rebuild — a rebuild that was itself wrong would agree with an
/// advance that copied it. Every scenario runs, so a failure names all of them.
///
/// **Seen to fail, two ways, and every other projection test stayed green both times.**
/// With `advance` applying what arrived as if it were newest — an arrival-order fold, the
/// natural first draft — all five slower-clock scenarios fail and
/// the merge, retitle and release ones pass. With a single watermark that *drops* records
/// stamped below the newest folded, only the binding scenario fails: a last-writer map
/// gets the right answer by accident when the loser is the one thrown away, and the
/// recorded conflict is what betrays it.
#[test]
fn advancing_lands_where_rebuilding_would_whatever_the_order() {
    let mut failed = Vec::new();
    for s in scenarios() {
        let outcome = std::panic::catch_unwind(|| {
            let mut advanced = Projected::rebuild(&through(&s, 0));
            for n in 1..s.batches.len() {
                let so_far = through(&s, n);
                assert_ne!(
                    advanced.advance(&so_far),
                    Advanced::AlreadyThere,
                    "batch {n} arrived and the projection thought it was covered"
                );
                let rebuilt = Projected::rebuild(&so_far);
                assert_eq!(
                    advanced.catalogue(),
                    rebuilt.catalogue(),
                    "after batch {n}, advancing did not land where rebuilding would"
                );
                assert_eq!(advanced.position(), rebuilt.position());
            }
            (s.check)(advanced.catalogue());
        });
        if outcome.is_err() {
            failed.push(s.name);
        }
    }
    assert!(failed.is_empty(), "advance ≠ rebuild for: {failed:#?}");
}

/// **The net covers every tier a fact can be in.**
///
/// A table that happened to hold only narrow facts would prove nothing about a merge,
/// and one with only stamp-ordered arrivals nothing about a sync.
#[test]
fn the_net_reaches_every_tier_and_both_orders() {
    let all = scenarios();
    let facts: Vec<&Fact> = all
        .iter()
        .flat_map(|s| s.batches.iter().flatten())
        .map(|r| &r.fact)
        .collect();
    assert!(facts.len() > 25, "{} facts", facts.len());
    assert!(facts.iter().any(|f| f.blast() == Blast::Narrow));
    assert!(facts.iter().any(|f| f.blast() == Blast::Wide));

    let out_of_order = all
        .iter()
        .filter(|s| {
            let first = s.batches[0].iter().map(|r| r.stamp).max().unwrap();
            s.batches[1..].iter().flatten().any(|r| r.stamp < first)
        })
        .count();
    assert!(
        out_of_order >= 5,
        "{out_of_order} scenarios arrive out of stamp order"
    );
}

/// **A release catalogued later moves a placement of an encoding it never names** —
/// the fact the audit found classified narrow, and why it is not.
///
/// `blast()`'s arm said a spine fact "cannot reach past the pressing it identifies". The
/// rung a placement is ranked by is computed *from* the release row, so the row arriving
/// re-ranks every placement whose candidates sit on it — a set the fact does not name, and
/// the definition of wide. The same holds one step down for the medium and the track.
#[test]
fn a_release_catalogued_later_moves_a_placement_it_never_names() {
    let s = scenarios()
        .into_iter()
        .find(|s| s.name.starts_with("a release is catalogued after"))
        .expect("the scenario");
    let copy = Id::<Encoding>::derived(b"a copy");
    let before = fold(&through(&s, 0));
    let after = fold(&through(&s, 1));
    assert_ne!(before.placed.get(&copy), after.placed.get(&copy));

    let arrived = &s.batches[1][0].fact;
    assert!(
        !arrived.touches().contains(&Touched::Encoding(copy)),
        "the fact names the encoding, so this proves nothing"
    );
    for fact in [
        arrived,
        &s.batches[0][2].fact, // a MediumCatalogued
        &s.batches[0][4].fact, // a TrackPlaced
    ] {
        assert_eq!(
            fact.blast(),
            Blast::Wide,
            "{fact:?} moves placements it does not name and is classified narrow"
        );
    }
}
