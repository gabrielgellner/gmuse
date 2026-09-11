//! `gmuse library query` — the half of the contract a tool reads — issue #91,
//! `library-api.md` §2.
//!
//! **A tool is handed an id and a locator, and proposes against the id.** That
//! is the whole reason this exists. Without it a pass has to walk the disk
//! itself and name what it finds by path, which is how `identify.py` came to
//! emit 712 confident matches that `apply` cannot read: they name files, and a
//! path is a property of one machine's copy, stale the moment anything moves.
//!
//! The two fields have deliberately different lifetimes. The **locator** is
//! good until the file moves; the **id** is good forever.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::model::basis::Basis;
use crate::model::entity::Device;
use crate::model::id::Id;
use crate::model::instance::Instance;
use crate::model::log::Record;
use crate::model::proposal::Queried;
use crate::model::reference::CreditOn;
use crate::model::seam::Librarian;

/// Which rows to hand over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Only {
    /// Everything the library holds here.
    Everything,
    /// Recordings resting on their bytes — the ones no authority has named.
    ///
    /// The work list an identification pass actually wants: a recording with a
    /// MusicBrainz id needs no looking up, and asking about it spends a rate
    /// limit to be told what the tags already said.
    ///
    /// **Includes the rows the gate held back** (#160), which have no recording
    /// at all. They are the same question one step earlier: nothing has named
    /// this performance. A row with no `recording` is one of them.
    Unidentified,
}

/// How many rows one recording gets.
///
/// **A different axis from [`Only`]**, which says *which recordings*. This
/// says how many copies of each, and the two compose: a pass can want every
/// copy of the unidentified rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copies {
    /// The one this device would open. What a pass about the *recording*
    /// wants — an identification, a title, a credit — because those are true
    /// of the performance however many files hold it.
    Shown,
    /// Every encoding this device can reach.
    ///
    /// **What a pass about the *bytes* wants** — #183. `EncodingMeasured` is
    /// keyed by encoding, and a library that deliberately holds one recording
    /// in several formats (#159) could only ever measure whichever copy `open`
    /// happened to choose.
    ///
    /// The two formats do not answer the same question, which is why one is
    /// not a sample of the other: a FLAC's cutoff says whether it is really
    /// lossless or a transcode wearing a lossless container, and an MP3's says
    /// whether it delivers what its bitrate declares. Measuring one leaves the
    /// other question unasked.
    Every,
}

/// Build the work list from this disk and this log.
/// Eight, and each one is a separate question this has to be told rather than
/// guess: where to look, what the log says, which device is asking, when, how
/// much to admit, one row per recording or per copy, what to keep, and what is
/// already known about the bytes. Bundling them into a struct would move the
/// same eight names one layer out and add a type whose only job is to hold
/// them.
#[expect(
    clippy::too_many_arguments,
    reason = "eight independent inputs, not a bundle"
)]
pub fn rows(
    roots: &[PathBuf],
    records: &BTreeSet<Record>,
    device: Id<Device>,
    now: i64,
    only: Only,
    copies: Copies,
    filter: Option<&crate::query::filter::Expr>,
    // **One cache across both walks below** — #188. They ask the same question
    // of the same files: `populate` to mint and `reconcile` to publish
    // holdings. Sharing it makes the second walk a `stat` per file; persisting
    // it between runs makes the first one that too, for everything that has
    // not changed.
    measurements: &mut crate::model::ingest::Reads,
) -> Vec<Queried> {
    // **The log first, then the disk.** `ingest` refuses to mint over a
    // binding somebody made — but only over one it can see, and a scan into an
    // empty instance sees nothing. Scanning first re-minted every relinked
    // recording from its bytes, so 377 identifications applied cleanly and the
    // work list did not move.
    let mut scratch = Instance::new(device);
    scratch.merge(now, records);
    // **Gated, like the front door** — #160, and `Anything` here was wrong.
    // Reading does not persist anything, so exempting it looked free; but
    // `populate` under `Anything` does not merely *show* what exists, it mints
    // a recording out of the file's own bytes. Doing that in the scratch
    // recreated exactly the row the gate had refused, and a pass reading this
    // list would then propose an identification whose `from` names a recording
    // that never existed anywhere but here.
    //
    // Gated, a held-back file arrives as what it is: an encoding, a claim, and
    // no performance.
    crate::model::populate::populate(
        &mut scratch,
        roots,
        now,
        crate::model::populate::Admits::Identified,
        measurements,
    );
    // **And publish what is here** (#145). A work list hands a tool an id and
    // a **locator**, so the holdings are half of the answer — and cataloguing
    // stopped asserting them, because a partial import must not unpublish the
    // rest of the library.
    crate::model::populate::reconcile(&mut scratch, roots, now, measurements);
    let catalogue = scratch.catalogue();
    let library = Librarian::from_catalogue(&catalogue);

    // **Compiled once, not per row.** `Expr::compile` says so in as many
    // words, and this list runs over the whole library.
    let env = crate::query::filter::Env {
        listened: None,
        now: now / 1000,
    };
    let keep = filter.map(|f| f.compile(env));

    let mut out = Vec::new();
    for (recording, recorded) in &catalogue.recordings {
        if only == Only::Unidentified
            && !matches!(
                catalogue.identified.get(recording),
                None | Some(Basis::Audio(_))
            )
        {
            continue;
        }
        // The locator comes from `Librarian::open`, which is the one place one
        // is produced — chosen per device, honouring a pin, refusing what is
        // not reachable. A row with no locator is an ordinary row: something
        // the library knows about and this machine cannot open.
        //
        // **Under `Every`, each copy is its own row** (#183), and the locator
        // is this device's holding of that encoding rather than the one `open`
        // would have picked. A copy this machine cannot reach yields no row at
        // all: the point of the list is bytes a pass can read.
        let shown = library.open(*recording, &[device], &[]);
        let here = |encoding: Id<crate::model::entity::Encoding>| {
            library
                .holdings_of(encoding)
                .into_iter()
                .find(|(d, _)| *d == device.to_string())
                .map(|(_, l)| l)
        };
        let each: Vec<(Option<Id<crate::model::entity::Encoding>>, Option<String>)> = match copies {
            Copies::Shown => vec![(shown.map(|(e, _)| e.id), shown.map(|(_, l)| l.to_string()))],
            Copies::Every => {
                let every: Vec<_> = library
                    .encodings
                    .of(*recording)
                    .into_iter()
                    .filter_map(|e| here(e.id).map(|l| (Some(e.id), Some(l))))
                    .collect();
                // **A recording nothing here holds still gets its row**, the
                // same one `Shown` would give it — the library knows about it
                // and this machine cannot open it, which is an answer.
                if every.is_empty() {
                    vec![(shown.map(|(e, _)| e.id), shown.map(|(_, l)| l.to_string()))]
                } else {
                    every
                }
            }
        };
        for (encoding, locator) in each {
            // **The filter is asked about the copy this row names** — #186,
            // and the reason it cannot simply be `listed()`: that answers with
            // the *shown* copy's measurements, which under `Every` is the
            // wrong copy for every row but one. A row here is a copy, so the
            // per-copy fields are taken from the encoding this row is about,
            // exactly as `Queried`'s own `cutoff_hz` and `length_ms` are.
            //
            // The rest of the row — title, credit, album, genre — is of the
            // recording, and `listed()` is right about all of it.
            if let Some(keep) = &keep {
                let Some(id) = encoding
                    .and_then(|e| library.row_of_copy(e))
                    .or(Some(crate::model::rows::CopyId::Loose(*recording)))
                else {
                    continue;
                };
                let Some(mut listed) = crate::model::rows::Listing::listed(&library, id) else {
                    continue;
                };
                if let Some(e) = encoding.and_then(|e| library.encodings.encodings.get(&e)) {
                    listed.bitrate = Some(e.bitrate);
                    listed.cutoff_hz = e.cutoff_hz.map(|m| m.value);
                    listed.cliff_db = e.cliff_db.map(|m| m.value);
                    listed.sample_rate = e.sample_rate;
                    listed.channels = e.channels;
                }
                let subject = crate::model::rows::Subject {
                    row: listed,
                    locator: locator.as_deref(),
                    recording: Some(*recording),
                };
                if !keep(&subject) {
                    continue;
                }
            }
            out.push(Queried {
                recording: Some(*recording),
                encoding,
                locator,
                title: recorded.title.clone(),
                artist: catalogue
                    .artist_names
                    .get(&recorded.artist)
                    .cloned()
                    .unwrap_or_default(),
                // **Of the copy this row names**, not of the recording: two
                // encodings of one performance can differ by a second of silence,
                // and a pass checking a duration is checking the file it has in
                // front of it.
                length_ms: encoding
                    .and_then(|e| catalogue.encodings.get(&e))
                    .and_then(|e| (e.length_ms > 0).then_some(e.length_ms)),
                // **Of the copy this row names**, like the length above: two
                // encodings of one performance measure differently, and that
                // difference is the whole reason the fact is keyed by encoding.
                cutoff_hz: encoding
                    .and_then(|e| catalogue.measurements.get(&e))
                    .map(|m| m.cutoff_hz),
                cliff_db: encoding
                    .and_then(|e| catalogue.measurements.get(&e))
                    .and_then(|m| m.cliff_db),
                // **What the library rests on, where that is MusicBrainz.** The
                // other rungs are deliberately not published as one: an `External`
                // key is opaque by design and an `AudioId` names bytes, so neither
                // is a thing to look up.
                musicbrainz: match catalogue.identified.get(recording) {
                    Some(Basis::MusicBrainz(id)) => Some(id.to_string()),
                    _ => None,
                },
                // By way of the slot this copy is placed in, which is how
                // `files_under` reaches an album and for the same reason: a
                // recording is on every release that holds it, and the one that
                // matters is the one the file is on.
                album: encoding
                    .and_then(|e| catalogue.placed.get(&e))
                    .and_then(|slot| library.group_of(Some(*slot))),
                album_artist: encoding
                    .and_then(|e| catalogue.placed.get(&e))
                    .and_then(|slot| library.group_of(Some(*slot)))
                    .map(|group| library.credited_line(CreditOn::ReleaseGroup(group)))
                    .filter(|line| !line.is_empty()),
            });
        }
    }
    // **The bytes nothing has named** — #160, on the same list rather than on
    // one of their own. The library holds them, which is what `Everything`
    // asks; and nothing has named them, which is what `Unidentified` asks. A
    // pass that walks either proposes an `identified` against the **encoding**,
    // exactly as it would for a row resting on its bytes — so this needs no new
    // vocabulary to be acted on.
    for (encoding, (artist, title)) in &catalogue.claims {
        if catalogue.bound.contains_key(encoding) {
            continue;
        }
        out.push(Queried {
            recording: None,
            encoding: Some(*encoding),
            // **This device's copy**, by the rule `open` follows for a named
            // row: a locator is where *this* machine can reach the bytes, and
            // a holding on another device is not an answer to "open this".
            locator: library
                .holdings_of(*encoding)
                .into_iter()
                .find(|(d, _)| *d == device.to_string())
                .map(|(_, l)| l),
            title: title.clone(),
            artist: artist.clone(),
            length_ms: catalogue
                .encodings
                .get(encoding)
                .and_then(|e| (e.length_ms > 0).then_some(e.length_ms)),
            // **Bytes nothing has named can still have been measured** — the
            // gate held the file back from a catalogue entry, not from an
            // analyser.
            cutoff_hz: catalogue.measurements.get(encoding).map(|m| m.cutoff_hz),
            cliff_db: catalogue
                .measurements
                .get(encoding)
                .and_then(|m| m.cliff_db),
            // Bytes nothing has named rest on nothing, by definition — this is
            // the list of exactly those rows, and nothing has placed them
            // either.
            musicbrainz: None,
            album: None,
            album_artist: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::Recording;
    use crate::model::log::Fact;
    use std::io::Write;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-query-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A tiny real WAV, so there is something to hash.
    fn wav(path: &std::path::Path, frames: usize) {
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

    fn device() -> Id<Device> {
        Id::derived(b"laptop")
    }

    /// Facts as a log would hold them, stamped in order.
    fn records(now: i64, facts: impl IntoIterator<Item = Fact>) -> BTreeSet<Record> {
        let mut clock = crate::model::clock::HybridClock::new(device());
        facts
            .into_iter()
            .map(|fact| Record {
                stamp: clock.stamp(now),
                fact,
            })
            .collect()
    }

    /// **One recording held twice is two rows, or one, and the caller says
    /// which** — #183.
    ///
    /// `EncodingMeasured` is keyed by an encoding, so a pass about the bytes
    /// needs every copy; a pass about the performance needs one. Two files of
    /// one recording is the shape #159 supports, and under `Shown` the second
    /// of them can never be reached.
    ///
    /// The two formats do not answer the same question either, which is why
    /// one is not a sample of the other: a FLAC's cutoff says whether it is
    /// really lossless or a transcode in a lossless container, an MP3's says
    /// whether it delivers what its bitrate declares.
    #[test]
    fn every_copy_is_a_row_of_its_own_where_the_caller_asks_for_one() {
        let root = dir("copies");
        // **Two different files**, because two identical ones are one
        // encoding — the store is content-addressed, and "two files with
        // identical bytes are one encoding, so both locators name it".
        wav(&root.join("a.wav"), 1000);
        wav(&root.join("b.wav"), 1200);

        let mut taken = crate::model::instance::Instance::new(device());
        crate::model::populate::populate(
            &mut taken,
            std::slice::from_ref(&root),
            50,
            crate::model::populate::Admits::Anything,
            &mut crate::model::ingest::Reads::default(),
        );
        let apart = rows(
            std::slice::from_ref(&root),
            taken.records(),
            device(),
            100,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(apart.len(), 2, "two recordings until something joins them");

        // Bind the second copy to the first's recording: one performance, two
        // encodings, which is the shape #159 supports and a lossless upgrade
        // arrives as.
        let one = apart[0].recording.expect("minted");
        let other = apart[1].recording.expect("minted");
        let mut log = taken.records().clone();
        log.extend(records(
            200,
            [Fact::EncodingBound {
                encoding: apart[1].encoding.expect("an encoding"),
                from: Some(other),
                to: one,
            }],
        ));

        let ask = |copies| {
            rows(
                std::slice::from_ref(&root),
                &log,
                device(),
                300,
                Only::Everything,
                copies,
                None,
                &mut crate::model::ingest::Reads::default(),
            )
        };
        let held = |rs: &[Queried]| -> Vec<Queried> {
            rs.iter()
                .filter(|r| r.recording == Some(one))
                .cloned()
                .collect()
        };

        let shown = held(&ask(Copies::Shown));
        let every = held(&ask(Copies::Every));
        assert_eq!(shown.len(), 1, "one row for the recording");
        assert_eq!(every.len(), 2, "one row for each copy of it");

        // **Distinct encodings and distinct locators**, because a pass keyed
        // by encoding would otherwise measure one file twice and call it two.
        let ids: BTreeSet<_> = every.iter().map(|r| r.encoding).collect();
        let paths: BTreeSet<_> = every.iter().map(|r| r.locator.clone()).collect();
        assert_eq!(ids.len(), 2, "two encodings, not one named twice");
        assert_eq!(paths.len(), 2, "and each names its own file");

        // **And the length is the copy's own** — two encodings of one
        // performance differ, and a pass checking a duration is checking the
        // file in front of it.
        let lengths: BTreeSet<_> = every.iter().map(|r| r.length_ms).collect();
        assert_eq!(lengths.len(), 2, "each row measures its own bytes");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The filter is asked of the copy the row names, not of the shown
    /// one** — #186, and the reason this is not one line of glue.
    ///
    /// `Librarian::listed` answers with the *shown* copy's measurements, which
    /// under `Every` is the wrong copy for every row but one. So a re-buy list
    /// filtering on `cutoff` would report the canonical copy's spectrum
    /// against each of its siblings' locators — every row of a recording
    /// passing or failing together, which is precisely the question
    /// `EncodingMeasured` was keyed by encoding to stop anyone asking.
    #[test]
    fn a_filter_reads_the_measurement_of_the_copy_the_row_names() {
        let root = dir("filtered");
        wav(&root.join("a.wav"), 1000);
        wav(&root.join("b.wav"), 1200);

        let mut taken = crate::model::instance::Instance::new(device());
        crate::model::populate::populate(
            &mut taken,
            std::slice::from_ref(&root),
            50,
            crate::model::populate::Admits::Anything,
            &mut crate::model::ingest::Reads::default(),
        );
        let apart = rows(
            std::slice::from_ref(&root),
            taken.records(),
            device(),
            100,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        let one = apart[0].recording.expect("minted");
        let (first, second) = (
            apart[0].encoding.expect("an encoding"),
            apart[1].encoding.expect("an encoding"),
        );

        // One performance, two copies — the shape #159 supports — and they
        // measure differently, which is the only reason to keep both numbers.
        let mut log = taken.records().clone();
        log.extend(records(
            200,
            [
                Fact::EncodingBound {
                    encoding: second,
                    from: apart[1].recording,
                    to: one,
                },
                Fact::EncodingMeasured {
                    encoding: first,
                    cutoff_hz: 20_000,
                    cliff_db: Some(400),
                    analyser: 1,
                },
                Fact::EncodingMeasured {
                    encoding: second,
                    cutoff_hz: 15_500,
                    cliff_db: Some(2_800),
                    analyser: 1,
                },
            ],
        ));

        let ask = |src: &str| -> Vec<Queried> {
            let expr = crate::query::filter::Expr::parse(src).expect("it parses");
            rows(
                std::slice::from_ref(&root),
                &log,
                device(),
                300,
                Only::Everything,
                Copies::Every,
                Some(&expr),
                &mut crate::model::ingest::Reads::default(),
            )
            .into_iter()
            .filter(|r| r.recording == Some(one))
            .collect()
        };

        // The re-buy predicate #87 asks for: a low cutoff **and** a cliff
        // steep enough to be an encoder rather than where the music stopped.
        let want = ask("cutoff<17000 & cliff>2500");
        assert_eq!(want.len(), 1, "one copy of the two measures like that");
        assert_eq!(
            want[0].encoding,
            Some(second),
            "and it is the copy that was measured low, not whichever one is shown"
        );

        // The other direction, so this cannot pass by the filter matching
        // nothing and the shown copy happening to be the one wanted.
        let good = ask("cutoff>19000");
        assert_eq!(good.len(), 1, "and the other copy answers the other way");
        assert_eq!(good[0].encoding, Some(first));

        // **A cutoff alone is not evidence.** A quiet track stops early
        // without an encoder having stopped it; on the real library that
        // difference is 38 rows against 7.
        assert_eq!(
            ask("cutoff<17000").len(),
            1,
            "the cutoff alone still names one copy here"
        );
        assert!(
            ask("cliff>2500 & cutoff>19000").is_empty(),
            "a steep cliff high up is not a transcode"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A row carries both, and they are not the same thing.** The locator is
    /// good until the file moves; the id is good forever. A tool that had only
    /// the path would have to name its findings by path, which is exactly what
    /// left 712 confident matches unusable.
    #[test]
    fn a_row_carries_an_id_to_propose_against_and_a_path_to_open() {
        let root = dir("both");
        wav(&root.join("a.wav"), 1000);

        let rows = rows(
            std::slice::from_ref(&root),
            &BTreeSet::new(),
            device(),
            100,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].encoding.is_some(), "nothing to propose against");
        assert_eq!(
            rows[0].locator.as_deref(),
            Some(root.join("a.wav").to_str().unwrap()),
            "nothing to open"
        );
    }

    /// **The work list is the files no authority has named.** Asking about a
    /// recording that already carries a MusicBrainz id spends a rate limit to
    /// be told what the tags said.
    #[test]
    fn the_unidentified_filter_leaves_out_what_is_already_named() {
        let root = dir("filter");
        wav(&root.join("a.wav"), 1000);
        wav(&root.join("b.wav"), 2000);

        let all = rows(
            std::slice::from_ref(&root),
            &BTreeSet::new(),
            device(),
            100,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(all.len(), 2);
        assert_eq!(
            rows(
                std::slice::from_ref(&root),
                &BTreeSet::new(),
                device(),
                100,
                Only::Unidentified,
                Copies::Shown,
                None,
                &mut crate::model::ingest::Reads::default(),
            )
            .len(),
            2,
            "an untagged file rests on its bytes, which is the whole work list"
        );

        // Now somebody identifies one of them, exactly as `apply` would.
        let named = Id::<Recording>::of_basis(&Basis::MusicBrainz(uuid::Uuid::from_u128(1)));
        let log = records(
            200,
            [
                Fact::RecordingIdentified {
                    recording: named,
                    basis: Basis::MusicBrainz(uuid::Uuid::from_u128(1)),
                },
                Fact::RecordingCatalogued {
                    recording: named,
                    title: "Named".into(),
                    artist: Id::derived(b"someone"),
                },
            ],
        );

        let after = rows(
            &[root],
            &log,
            device(),
            300,
            Only::Unidentified,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(after.len(), 2, "the two on disk still rest on their bytes");
        assert!(
            !after.iter().any(|r| r.recording == Some(named)),
            "a recording an authority has named is not work"
        );
    }

    /// **The rung the row rests on, where it is one a tool can look up** —
    /// #152.
    ///
    /// Here for the reason `length_ms` is: without it a pass wanting to ask
    /// MusicBrainz about a row has to *search* for it by title and artist, and
    /// that is a weaker question than the one it means to ask. `joined.py`
    /// asks whether an artist exists under a credit string; `The Game`,
    /// `La Fleur` and `Sonya Elise` all answer yes for somebody. Handed the
    /// recording id, the same pass asks who is actually credited on that
    /// recording and gets one artist id per artist.
    ///
    /// **Only MusicBrainz.** The other two rungs are not things to look up: an
    /// `External` key is opaque by design (#89 §5) and an `AudioId` names
    /// bytes, so publishing either as a lookup handle would be inviting a tool
    /// to ask a question with no answer.
    #[test]
    fn a_row_publishes_the_musicbrainz_id_it_rests_on_and_no_other_rung() {
        let root = dir("rung");
        wav(&root.join("a.wav"), 1000);

        let bare = rows(
            std::slice::from_ref(&root),
            &BTreeSet::new(),
            device(),
            100,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(bare.len(), 1);
        assert_eq!(
            bare[0].musicbrainz, None,
            "a file resting on its bytes rests on nothing to look up"
        );

        let mbid = uuid::Uuid::from_u128(7);
        let named = Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid));
        let log = records(
            200,
            [
                Fact::RecordingIdentified {
                    recording: named,
                    basis: Basis::MusicBrainz(mbid),
                },
                Fact::RecordingCatalogued {
                    recording: named,
                    title: "Named".into(),
                    artist: Id::derived(b"someone"),
                },
            ],
        );
        let after = rows(
            &[root],
            &log,
            device(),
            300,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(
            after
                .iter()
                .find(|r| r.recording == Some(named))
                .and_then(|r| r.musicbrainz.clone()),
            Some(mbid.to_string()),
            "the row rests on MusicBrainz and does not say so"
        );

        // And the field is omitted rather than emitted as null, so a tool can
        // test for its presence the way it tests for a locator.
        let bare_json = serde_json::to_string(&bare[0]).expect("serialises");
        assert!(
            !bare_json.contains("musicbrainz"),
            "an absent rung should not appear at all: {bare_json}"
        );
        assert!(
            !bare_json.contains("album"),
            "a recording in no slot sits on no album: {bare_json}"
        );
    }

    /// **The album is on the row because it is the level that names a folder**
    /// — #152.
    ///
    /// `ingest` credits the recording *and* the release group from one tag, so
    /// a joined credit is written on both; `file_under` asks the album first
    /// (#84). A pass handed only the recording can make a correct split that
    /// changes nothing anybody sees, which is exactly what happened: 171
    /// release groups here stayed credited to `Marc Romboy,Stephan Bodzin`
    /// after their recordings were split.
    ///
    /// **By way of the slot the copy is placed in.** A recording is on every
    /// release that holds it, so "the" album is a choice — and it is the same
    /// choice `files_under` makes, because the folder follows the file.
    #[test]
    fn a_row_names_the_album_its_copy_sits_on() {
        use crate::model::entity::{Medium, Release, ReleaseGroup, Track};

        let root = dir("album");
        let path = root.join("a.wav");
        wav(&path, 1000);

        // Nothing placed, so nothing to name.
        let loose = rows(
            std::slice::from_ref(&root),
            &BTreeSet::new(),
            device(),
            100,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(loose.len(), 1);
        assert_eq!(
            loose[0].album, None,
            "a recording in no slot is on no album"
        );

        // The gate holds an untagged file without minting a performance
        // (#160), so name one before there is anything to place.
        let encoding = loose[0].encoding.expect("the scan described the bytes");
        let recording = Id::<Recording>::of_basis(&Basis::MusicBrainz(uuid::Uuid::from_u128(9)));
        let group = Id::<ReleaseGroup>::derived(b"a group");
        let release = Id::<Release>::derived(b"a pressing");
        let medium = Id::<Medium>::of_medium(release, 1);
        let slot = Id::<Track>::derived(b"a slot");
        let log = records(
            200,
            [
                Fact::RecordingIdentified {
                    recording,
                    basis: Basis::MusicBrainz(uuid::Uuid::from_u128(9)),
                },
                Fact::RecordingCatalogued {
                    recording,
                    title: "Placed".into(),
                    artist: Id::derived(b"someone"),
                },
                Fact::EncodingBound {
                    encoding,
                    from: None,
                    to: recording,
                },
                Fact::ReleaseGroupCatalogued {
                    group,
                    title: "An Album".into(),
                    artist: Id::derived(b"someone"),
                    first_released: None,
                    kind: Default::default(),
                    secondary: Default::default(),
                },
                Fact::ReleaseCatalogued {
                    release,
                    group,
                    title: "An Album".into(),
                    released: None,
                    label: None,
                    media: Some(1),
                },
                Fact::MediumCatalogued {
                    medium,
                    release,
                    position: 1,
                    format: None,
                    tracks: Some(1),
                },
                Fact::TrackPlaced {
                    track: slot,
                    medium,
                    recording,
                    position: 1,
                    title_on_release: None,
                },
                Fact::EncodingPlaced {
                    encoding,
                    track: slot,
                    basis: crate::model::reference::Source::TagVariant,
                },
            ],
        );

        let placed = rows(
            &[root],
            &log,
            device(),
            300,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(
            placed
                .iter()
                .find(|r| r.recording == Some(recording))
                .and_then(|r| r.album),
            Some(group),
            "the copy sits in a slot on this album and the row does not say so"
        );
    }

    /// **A scan must not mint over an identification, and this is the shape
    /// that let it.**
    ///
    /// The log holds `EncodingBound` and nothing else — encodings are
    /// described by scanning, so a log carrying a relink and no scan beside it
    /// is the ordinary case, not an odd one. The fold wrote the binding onto
    /// the `Encoded` row and dropped it when there was none, so `ingest` asked
    /// "is this encoding already bound?", was told no, and minted from the
    /// bytes again.
    ///
    /// Nothing failed. 377 identifications applied cleanly, the log was
    /// correct, and the work list did not move — which is the only way it
    /// **Bytes the gate held back are on the work list** — #160, and this is
    /// what makes the refusal answerable rather than merely final.
    ///
    /// `import` mints no recording for a file nothing has named, so there is
    /// no row for `Only::Unidentified`'s usual filter to find. Without this
    /// the gate would be a dead end: the file is refused, nothing can see it,
    /// and the only way forward is to take it unidentified after all.
    ///
    /// The row carries an **encoding** and no recording, which is exactly what
    /// an `identified` proposal names — so the pass that walks this list needs
    /// no new vocabulary to act on it.
    #[test]
    fn bytes_the_gate_held_back_are_work_a_pass_can_see() {
        use crate::model::populate::Admits;

        let root = dir("held-back");
        let path = root.join("a.wav");
        wav(&path, 1000);

        // The gate: nothing named it, so nothing catalogued it.
        let mut i = crate::model::instance::Instance::new(device());
        let report = crate::model::populate::populate(
            &mut i,
            std::slice::from_ref(&root),
            100,
            Admits::Identified,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(report.unidentified.len(), 1, "the gate did not hold it");
        crate::model::populate::reconcile(
            &mut i,
            std::slice::from_ref(&root),
            100,
            &mut crate::model::ingest::Reads::default(),
        );

        let work = rows(
            std::slice::from_ref(&root),
            i.records(),
            device(),
            200,
            Only::Unidentified,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert_eq!(work.len(), 1, "the held-back file is invisible to a pass");
        let row = &work[0];
        assert!(
            row.recording.is_none(),
            "a performance was minted after all"
        );
        assert!(row.encoding.is_some(), "nothing to propose against");
        assert_eq!(
            row.locator.as_deref(),
            Some(path.display().to_string().as_str()),
            "a pass cannot open it"
        );

        // **And it stops being work once it is named.** The claim is what the
        // tags said; binding the encoding is what answers the question.
        let named = Id::<Recording>::of_basis(&Basis::MusicBrainz(uuid::Uuid::from_u128(7)));
        let mut log = i.records().clone();
        log.extend(records(
            300,
            [
                Fact::RecordingIdentified {
                    recording: named,
                    basis: Basis::MusicBrainz(uuid::Uuid::from_u128(7)),
                },
                Fact::EncodingBound {
                    encoding: row.encoding.expect("an encoding"),
                    from: None,
                    to: named,
                },
            ],
        ));
        let after = rows(
            std::slice::from_ref(&root),
            &log,
            device(),
            400,
            Only::Unidentified,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        assert!(
            !after.iter().any(|r| r.recording.is_none()),
            "a named file is still being offered as work"
        );
    }

    /// showed up.
    #[test]
    fn a_scan_does_not_mint_over_an_identification() {
        let root = dir("relink");
        wav(&root.join("a.wav"), 1000);

        // What the file mints when somebody takes it unidentified — which
        // since #160 is the only way it mints anything, and is the state this
        // test is about: a byte-minted recording that a later identification
        // moves off. A scan can no longer produce one by itself.
        let mut taken = crate::model::instance::Instance::new(device());
        crate::model::populate::populate(
            &mut taken,
            std::slice::from_ref(&root),
            50,
            crate::model::populate::Admits::Anything,
            &mut crate::model::ingest::Reads::default(),
        );
        let before = rows(
            std::slice::from_ref(&root),
            taken.records(),
            device(),
            100,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        let encoding = before[0].encoding.expect("an encoding");
        let minted = before[0]
            .recording
            .expect("taking it unidentified mints one");

        // Somebody identifies it, exactly as `apply` writes one.
        let named = Id::<Recording>::of_basis(&Basis::MusicBrainz(uuid::Uuid::from_u128(1)));
        let mut log = taken.records().clone();
        log.extend(records(
            200,
            [
                Fact::RecordingIdentified {
                    recording: named,
                    basis: Basis::MusicBrainz(uuid::Uuid::from_u128(1)),
                },
                Fact::EncodingBound {
                    encoding,
                    from: Some(minted),
                    to: named,
                },
            ],
        ));

        let after = rows(
            std::slice::from_ref(&root),
            &log,
            device(),
            300,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        // **The row holding the file names the identified recording.** Asserted
        // through the encoding rather than by counting rows: the byte-minted
        // recording is in the log now — it is what the relink moved *off* — and
        // a recording nothing holds a copy of is an ordinary row (#155), not a
        // second copy of this file.
        let holding: Vec<_> = after
            .iter()
            .filter(|r| r.encoding == Some(encoding))
            .collect();
        assert_eq!(holding.len(), 1, "the file is offered twice");
        assert_eq!(
            holding[0].recording,
            Some(named),
            "the scan minted over a decision somebody made"
        );
        assert!(
            !rows(
                &[root],
                &log,
                device(),
                300,
                Only::Unidentified,
                Copies::Shown,
                None,
                &mut crate::model::ingest::Reads::default(),
            )
            .iter()
            .any(|r| r.encoding == Some(encoding)),
            "an identified file is still on the work list"
        );
    }

    /// A recording the log knows about and this disk does not hold is an
    /// ordinary row with nothing to open — §1 is explicit that a wishlist
    /// entry is exactly one of those, so it is listed rather than dropped.
    #[test]
    fn something_not_here_is_a_row_with_no_locator() {
        let root = dir("absent");
        let elsewhere = Id::<Recording>::of_basis(&Basis::Audio(crate::audio_id::AudioId {
            scheme: 1,
            hash: 42,
        }));
        let log = records(
            100,
            [Fact::RecordingCatalogued {
                recording: elsewhere,
                title: "On the NAS".into(),
                artist: Id::derived(b"someone"),
            }],
        );

        let rows = rows(
            &[root],
            &log,
            device(),
            200,
            Only::Everything,
            Copies::Shown,
            None,
            &mut crate::model::ingest::Reads::default(),
        );
        let row = rows
            .iter()
            .find(|r| r.recording == Some(elsewhere))
            .expect("dropped");
        assert!(row.locator.is_none());
        assert!(row.encoding.is_none());
        assert_eq!(row.title, "On the NAS");
    }
}
